# Backpressure without protocol starvation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop a slow consumer from starving the socket loop's protocol handling — deliver inbound frames via `try_send` plus a backpressure sub-loop that holds one frame and keeps keepalive/outgoing alive without reading inbound, and raise the channel default so transient hiccups never trigger it.

**Architecture:** In `socket_loop_split`, inbound frame handling is split into `handle_incoming` (protocol: ping/pong/close/errors) and `deliver_with_backpressure` (data delivery). Data is delivered with `try_send`; on a full channel the loop holds the single frame and waits on `Sender::reserve()` while still servicing outgoing sends and the keepalive timer, but deliberately stops reading the inbound stream (TCP backpressure, bounded memory, zero loss). The `CHANNEL_SIZE` const-generic default goes from 4 to 256.

**Tech Stack:** Rust (edition 2024), `tokio` mpsc (`try_send`/`reserve`), `tokio-tungstenite`.

## Global Constraints

- Rust edition 2024, MSRV 1.85.
- `#![deny(missing_docs)]` — every public item needs a doc comment.
- Lint clean: `cargo clippy --all-features --tests --benches -- -Dclippy::all -Dclippy::pedantic`.
- Format clean: `cargo fmt --check`.
- Compile/lint clean across `--no-default-features`, `--features tracing`, `--all-features`.
- Conventional commits (feat:, fix:, refactor:, test:, docs:).
- Public API signatures unchanged except the `CHANNEL_SIZE` default value (4 → 256).
- End commit messages with: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

## File Structure

- `src/mock_server.rs` (modify): add `backpressure_probe_server` — sends a fixed burst, then watches for a client keepalive ping and replies with an "alive" marker only if it arrives. This is the test discriminator for protocol-liveness-during-backpressure.
- `src/lib.rs` (modify): re-export `backpressure_probe_server` under the `mocking` feature; change the `CHANNEL_SIZE` default from `4` to `256` and update its doc.
- `src/socket_loop.rs` (modify): replace `socket_message_received` with `handle_incoming` (returns an `Incoming` enum) and add `deliver_with_backpressure`; rewire the `socket_loop_split` inbound arm.
- `src/config.rs` (modify): one doc sentence noting backpressure resilience depends on keepalive being enabled.
- `tests/integration.rs` (modify): probe-server validation test (Task 1) and the backpressure discriminator test (Task 2).

---

## Task 1: Probe mock server + validation test

Adds the `backpressure_probe_server` test harness and a test that validates it on a connection large enough that backpressure never engages (so it passes on the current, unmodified loop).

**Files:**
- Modify: `src/mock_server.rs`
- Modify: `src/lib.rs`
- Test: `tests/integration.rs`

**Interfaces:**
- Produces: `pub async fn backpressure_probe_server(ws: WebSocketStreamType) -> Result<bool, tungstenite::Error>` (behind `mocking`). On connect it sends 5 `EchoControlMessage::Message("burst-0..4")` Text frames, waits up to 500ms for a client `Ping`, and — only if one arrives — sends a final `EchoControlMessage::Message("alive")` Text frame; then it pongs/drains until the client closes.

- [ ] **Step 1: Add imports to `src/mock_server.rs`**

Add these to the existing `use` block at the top of `src/mock_server.rs`:

```rust
use std::time::Duration;
use tokio::time::timeout;
```

- [ ] **Step 2: Add the probe server**

Append this function to `src/mock_server.rs` (after `auth_echo_server`, before `get_mock_address`):

```rust
/// Probe server for the backpressure tests.
///
/// On connect it immediately sends a burst of five `EchoControlMessage::Message`
/// frames (`"burst-0"`..`"burst-4"`), then waits up to 500ms for a client
/// keepalive [`Message::Ping`]. It sends a final `EchoControlMessage::Message`
/// `"alive"` frame ONLY if that ping arrives — so a client whose socket loop is
/// stalled by a full receive buffer (and therefore cannot send keepalives) never
/// receives the marker. After that it pongs and drains until the client closes.
///
/// This is a test harness exposed under the `mocking` feature flag and is **not**
/// intended for production use.
/// # Errors
/// - If sending a burst frame, the marker, or a pong fails
pub async fn backpressure_probe_server(
    ws: WebSocketStreamType,
) -> Result<bool, tungstenite::Error> {
    let (mut sink, mut stream) = ws.split();

    // Immediately send a burst of five data frames.
    for i in 0..5u32 {
        let msg = EchoControlMessage::Message(format!("burst-{i}"));
        let text = serde_json::to_string(&msg).unwrap();
        sink.send(Message::Text(text.into())).await?;
    }

    // Wait for a client keepalive ping — proof the client kept the protocol
    // alive while its consumer was behind.
    let got_ping = timeout(Duration::from_millis(500), async {
        while let Some(Ok(frame)) = stream.next().await {
            if matches!(frame, Message::Ping(_)) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    if got_ping {
        let alive = EchoControlMessage::Message("alive".to_string());
        let text = serde_json::to_string(&alive).unwrap();
        sink.send(Message::Text(text.into())).await?;
    }

    // Drain until the client closes.
    while let Some(Ok(frame)) = stream.next().await {
        match frame {
            Message::Ping(_) => sink.send(Message::Pong(Bytes::new())).await?,
            Message::Close(_) => break,
            _ => {}
        }
    }
    Ok(true)
}
```

- [ ] **Step 3: Re-export it from `src/lib.rs`**

In `src/lib.rs`, update the `mocking` re-export line to include `backpressure_probe_server`:

```rust
#[cfg(feature = "mocking")]
pub use mock_server::{
    EchoControlMessage, auth_echo_server, backpressure_probe_server, echo_server, get_mock_address,
};
```

- [ ] **Step 4: Write the validation test**

In `tests/integration.rs`, ensure the top `use socketeer::{...}` import list includes `NoopHandler` and `backpressure_probe_server` (add them to the existing list). Then add:

```rust
#[tokio::test]
async fn test_backpressure_probe_server_delivers_burst_and_marker() {
    // Large buffer (16) > burst (5): no backpressure. Validates the probe
    // server — the client buffers the burst, goes idle, and its keepalive ping
    // prompts the "alive" marker. Passes on the unmodified loop.
    let server_address = get_mock_address(backpressure_probe_server).await;
    let options = ConnectOptions {
        keepalive_interval: Some(Duration::from_millis(100)),
        ..ConnectOptions::default()
    };
    let mut socketeer: Socketeer<EchoJson, NoopHandler, 16> =
        Socketeer::connect_with(&format!("ws://{server_address}"), options)
            .await
            .unwrap();

    let mut received = Vec::new();
    for _ in 0..5 {
        match socketeer.next_message().await.unwrap() {
            EchoControlMessage::Message(s) => received.push(s),
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(
        received,
        vec!["burst-0", "burst-1", "burst-2", "burst-3", "burst-4"]
    );

    let marker = socketeer.next_message().await.unwrap();
    assert_eq!(marker, EchoControlMessage::Message("alive".to_string()));

    socketeer.close_connection().await.unwrap();
}
```

- [ ] **Step 5: Run the test**

Run: `cargo test --all-features --test integration test_backpressure_probe_server_delivers_burst_and_marker`
Expected: PASS (the probe server works; no backpressure with a buffer of 16).

- [ ] **Step 6: Lint, format, full suite**

Run: `cargo clippy --all-features --tests --benches -- -Dclippy::all -Dclippy::pedantic` → clean.
Run: `cargo fmt --check` → clean.
Run: `cargo test --all-features` → PASS.

- [ ] **Step 7: Commit**

```bash
git add src/mock_server.rs src/lib.rs tests/integration.rs
git commit -m "test: add backpressure probe mock server

Sends a burst then replies with an 'alive' marker only if it receives a client
keepalive ping — the discriminator for protocol-liveness during backpressure.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Backpressure-resilient delivery

Restructures the inbound path so a full receive channel no longer blocks the `select!`. TDD: the discriminator test is RED on the current loop (a stalled loop sends no keepalive, so the probe server withholds the marker) and GREEN after the change.

**Files:**
- Modify: `src/socket_loop.rs`
- Test: `tests/integration.rs`

**Interfaces:**
- Consumes: `backpressure_probe_server` (Task 1); existing `send_socket_message`, `send_keepalive`, `LoopState`, `TxChannelPayload`.
- Produces (internal): `enum Incoming { State(LoopState), Data(Message) }`; `async fn handle_incoming(message, sink) -> Incoming`; `async fn deliver_with_backpressure(frame, sender, receiver, sink, keepalive_interval, keepalive_message) -> LoopState`.

- [ ] **Step 1: Write the failing discriminator test**

In `tests/integration.rs`, ensure `use tokio::time::timeout;` is present at the top (add it next to the existing `use tokio::time::sleep;`). Then add:

```rust
#[tokio::test]
async fn test_backpressure_keeps_protocol_alive() {
    // Small buffer (2) < burst (5) forces backpressure. The client pauses past
    // the keepalive interval before draining; the "alive" marker proves the
    // loop kept firing keepalives WHILE backpressured (the server withholds it
    // if no ping arrives within 500ms). On the pre-backpressure loop the loop
    // is blocked on a full-channel send and sends no ping until the drain at
    // 800ms — past the server's window — so the marker never comes.
    let server_address = get_mock_address(backpressure_probe_server).await;
    let options = ConnectOptions {
        keepalive_interval: Some(Duration::from_millis(100)),
        ..ConnectOptions::default()
    };
    let mut socketeer: Socketeer<EchoJson, NoopHandler, 2> =
        Socketeer::connect_with(&format!("ws://{server_address}"), options)
            .await
            .unwrap();

    // Stay paused well past the server's 500ms ping window.
    sleep(Duration::from_millis(800)).await;

    // All five burst frames must arrive, in order — zero loss.
    let mut received = Vec::new();
    for _ in 0..5 {
        match socketeer.next_message().await.unwrap() {
            EchoControlMessage::Message(s) => received.push(s),
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(
        received,
        vec!["burst-0", "burst-1", "burst-2", "burst-3", "burst-4"]
    );

    // Marker present => the client kept the protocol alive while backpressured.
    let marker = timeout(Duration::from_secs(2), socketeer.next_message())
        .await
        .expect("alive marker should arrive, not hang")
        .unwrap();
    assert_eq!(marker, EchoControlMessage::Message("alive".to_string()));

    socketeer.close_connection().await.unwrap();
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --all-features --test integration test_backpressure_keeps_protocol_alive`
Expected: FAIL — the five burst frames still arrive (no loss even today), but the marker read hits the 2s timeout and `.expect("alive marker should arrive, not hang")` panics, because the stalled loop sent no keepalive within the server's 500ms window.

- [ ] **Step 3: Replace `socket_message_received` with `handle_incoming`**

In `src/socket_loop.rs`, delete the entire `socket_message_received` function and replace it with the `Incoming` enum and `handle_incoming` (same protocol handling, but data frames are returned instead of delivered):

```rust
enum Incoming {
    State(LoopState),
    Data(Message),
}

#[cfg_attr(feature = "tracing", instrument)]
async fn handle_incoming(
    message: Option<Result<Message, tungstenite::Error>>,
    sink: &mut SocketSink,
) -> Incoming {
    const PONG_BYTES: Bytes = Bytes::from_static(b"pong");
    match message {
        Some(Ok(message)) => match message {
            Message::Ping(_) => {
                let send_result = sink
                    .send(Message::Pong(PONG_BYTES))
                    .await
                    .map_err(Error::from);
                Incoming::State(match send_result {
                    Ok(()) => LoopState::Running,
                    Err(e) => {
                        #[cfg(feature = "tracing")]
                        error!("Error sending Pong: {:?}", e);
                        LoopState::Error(e)
                    }
                })
            }
            Message::Close(_) => {
                let close_result = sink.close().await;
                Incoming::State(match close_result {
                    Ok(()) => LoopState::Closed,
                    Err(e) => {
                        #[cfg(feature = "tracing")]
                        error!("Error sending Close: {:?}", e);
                        LoopState::Error(Error::from(e))
                    }
                })
            }
            Message::Text(_) | Message::Binary(_) => Incoming::Data(message),
            _ => Incoming::State(LoopState::Running),
        },
        Some(Err(e)) => {
            #[cfg(feature = "tracing")]
            error!("Error receiving message: {:?}", e);
            Incoming::State(LoopState::Error(Error::WebsocketError(e)))
        }
        None => {
            #[cfg(feature = "tracing")]
            info!("Websocket Closed, closing rx channel");
            Incoming::State(LoopState::Error(Error::WebsocketClosed))
        }
    }
}
```

- [ ] **Step 4: Add `deliver_with_backpressure`**

In `src/socket_loop.rs`, add this function immediately after `handle_incoming`:

```rust
/// Deliver one inbound data frame to the consumer.
///
/// Fast path: `try_send`. If the receive channel is full, hold the single frame
/// and wait for capacity via `reserve()` while still servicing outgoing sends
/// and firing keepalives — but do NOT read the inbound stream. Unread bytes stay
/// in the TCP buffer (end-to-end backpressure), memory is bounded to this one
/// frame, and ordering is preserved (this frame is delivered before the loop
/// reads the next).
async fn deliver_with_backpressure(
    frame: Message,
    sender: &mut mpsc::Sender<Message>,
    receiver: &mut mpsc::Receiver<TxChannelPayload>,
    sink: &mut SocketSink,
    keepalive_interval: Option<Duration>,
    keepalive_message: Option<&Message>,
) -> LoopState {
    let frame = match sender.try_send(frame) {
        Ok(()) => return LoopState::Running,
        Err(mpsc::error::TrySendError::Closed(_)) => {
            return LoopState::Error(Error::SocketeerDroppedWithoutClosing);
        }
        Err(mpsc::error::TrySendError::Full(frame)) => frame,
    };
    #[cfg(feature = "tracing")]
    trace!("Receive buffer full; holding frame and applying backpressure");
    loop {
        if let Some(interval) = keepalive_interval {
            select! {
                permit = sender.reserve() => match permit {
                    Ok(permit) => {
                        permit.send(frame);
                        return LoopState::Running;
                    }
                    Err(_) => return LoopState::Error(Error::SocketeerDroppedWithoutClosing),
                },
                outgoing = receiver.recv() => match send_socket_message(outgoing, sink).await {
                    LoopState::Running => {}
                    other => return other,
                },
                () = sleep(interval) => match send_keepalive(sink, keepalive_message).await {
                    LoopState::Running => {}
                    other => return other,
                },
            }
        } else {
            select! {
                permit = sender.reserve() => match permit {
                    Ok(permit) => {
                        permit.send(frame);
                        return LoopState::Running;
                    }
                    Err(_) => return LoopState::Error(Error::SocketeerDroppedWithoutClosing),
                },
                outgoing = receiver.recv() => match send_socket_message(outgoing, sink).await {
                    LoopState::Running => {}
                    other => return other,
                },
            }
        }
    }
}
```

(Note: `frame` is moved only in the `permit` arm, which returns; the other arms don't touch it, so it stays valid across loop iterations. This compiles under NLL — no `Option`/`take` wrapper is needed.)

- [ ] **Step 5: Rewire the `socket_loop_split` inbound arm**

In `src/socket_loop.rs`, in `socket_loop_split`, replace BOTH `incoming_message = stream.next() => ...` arms (the one in the `if let Some(interval)` branch and the one in the `else` branch) so they route data through `deliver_with_backpressure`. The two `select!` blocks become:

```rust
        state = if let Some(interval) = keepalive_interval {
            select! {
                outgoing_message = receiver.recv() => send_socket_message(outgoing_message, &mut sink).await,
                incoming_message = stream.next() => match handle_incoming(incoming_message, &mut sink).await {
                    Incoming::State(s) => s,
                    Incoming::Data(frame) => deliver_with_backpressure(
                        frame,
                        &mut sender,
                        &mut receiver,
                        &mut sink,
                        keepalive_interval,
                        keepalive_message.as_ref(),
                    ).await,
                },
                () = sleep(interval) => send_keepalive(&mut sink, keepalive_message.as_ref()).await,
            }
        } else {
            select! {
                outgoing_message = receiver.recv() => send_socket_message(outgoing_message, &mut sink).await,
                incoming_message = stream.next() => match handle_incoming(incoming_message, &mut sink).await {
                    Incoming::State(s) => s,
                    Incoming::Data(frame) => deliver_with_backpressure(
                        frame,
                        &mut sender,
                        &mut receiver,
                        &mut sink,
                        keepalive_interval,
                        keepalive_message.as_ref(),
                    ).await,
                },
            }
        };
```

- [ ] **Step 6: Run the discriminator test to verify it passes**

Run: `cargo test --all-features --test integration test_backpressure_keeps_protocol_alive`
Expected: PASS — the five burst frames arrive in order and the "alive" marker is received (the loop fired a keepalive while backpressured).

- [ ] **Step 7: Lint, format, full suite**

Run: `cargo clippy --all-features --tests --benches -- -Dclippy::all -Dclippy::pedantic` → clean.
Run: `cargo fmt --check` → clean.
Run: `cargo test --all-features` → PASS (all existing tests still green).

- [ ] **Step 8: Commit**

```bash
git add src/socket_loop.rs tests/integration.rs
git commit -m "feat: keep protocol alive under receive backpressure

Inbound delivery now uses try_send + a backpressure sub-loop that holds one
frame and keeps keepalive/outgoing alive (without reading inbound) until the
consumer frees capacity. A slow consumer no longer stalls ping/pong and
keepalive, so the server no longer drops the connection. Zero data loss,
bounded memory, ordering preserved.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Raise the channel default + docs

Raises the `CHANNEL_SIZE` default so transient consumer hiccups fit in the channel and never trigger the backpressure sub-loop, and documents the keepalive dependency.

**Files:**
- Modify: `src/lib.rs`
- Modify: `src/config.rs`

**Interfaces:**
- Produces: `Socketeer<C, Handler = NoopHandler, const CHANNEL_SIZE: usize = 256>` (default changed from 4).

- [ ] **Step 1: Change the default and its doc in `src/lib.rs`**

In `src/lib.rs`, update the `CHANNEL_SIZE` doc bullet in the `Socketeer` type-parameter docs:

```rust
/// - `CHANNEL_SIZE`: Capacity of the internal mpsc channels between the
///   connection task and the handle. Defaults to 256. A larger buffer absorbs
///   transient consumer slowdowns so backpressure (which holds one frame and
///   relies on keepalives to stay alive) engages only under sustained overload;
///   tune it for your feed's burstiness.
```

Then change the struct declaration default:

```rust
pub struct Socketeer<C: Codec, Handler = NoopHandler, const CHANNEL_SIZE: usize = 256>
```

- [ ] **Step 2: Document the keepalive dependency in `src/config.rs`**

In `src/config.rs`, extend the doc comment on the `keepalive_interval` field to add a final sentence:

```rust
    /// Idle timeout before sending a keepalive. `None` disables keepalives entirely.
    /// Defaults to 2 seconds.
    ///
    /// Keepalives also keep the connection alive while the receive buffer is
    /// full and the reader is applying backpressure; with keepalives disabled, a
    /// persistently slow consumer may eventually be disconnected by the server.
    pub keepalive_interval: Option<Duration>,
```

- [ ] **Step 3: Build and run the full suite**

Run: `cargo test --all-features`
Expected: PASS — all existing tests still green with the larger default (none depend on the buffer being 4).

- [ ] **Step 4: Lint and format across feature sets**

Run: `cargo clippy --all-features --tests --benches -- -Dclippy::all -Dclippy::pedantic` → clean.
Run: `cargo clippy --no-default-features -- -Dclippy::all -Dclippy::pedantic` → clean.
Run: `cargo clippy --features tracing -- -Dclippy::all -Dclippy::pedantic` → clean.
Run: `cargo fmt --check` → clean.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/config.rs
git commit -m "feat: raise default channel size to 256 and document backpressure

A larger default buffer absorbs transient consumer slowdowns so backpressure
engages only under sustained overload. Documents that backpressure resilience
relies on keepalive being enabled.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Final verification (after Task 3)

- [ ] `cargo test --all-features` — all pass (existing + 2 new backpressure tests).
- [ ] `cargo clippy --all-features --tests --benches -- -Dclippy::all -Dclippy::pedantic` — clean.
- [ ] `cargo clippy --no-default-features -- -Dclippy::all -Dclippy::pedantic` — clean.
- [ ] `cargo clippy --features tracing -- -Dclippy::all -Dclippy::pedantic` — clean.
- [ ] `cargo fmt --check` — clean.
- [ ] Skim the spec (`docs/superpowers/specs/2026-06-29-backpressure-resilience-design.md`) — every acceptance criterion met.
- [ ] Note: spec Testing item #3 (dead peer detected via keepalive during backpressure) is intentionally NOT a separate test. Its distinguishing case — a *frozen* consumer that never reads, where the new loop self-terminates via a failing keepalive but the old loop would block forever — is not observable through the public API (no exposed handle on the background task), and the variant where the consumer does read resolves identically on old and new code. The keepalive-failure-terminates-the-loop path is exercised by the existing `test_abrupt_close_surfaces_websocket_error` and the keepalive arm in `deliver_with_backpressure`. Documented here so the coverage decision is explicit, not silent.
- [ ] Note for the controller: `CLAUDE.md` is gitignored; if updating the local copy, mention the loop's new backpressure behavior there, but it won't be part of the branch.
