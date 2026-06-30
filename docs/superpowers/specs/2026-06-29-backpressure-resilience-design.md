# Backpressure without protocol starvation

**Status:** Approved design — ready for implementation planning
**Date:** 2026-06-29
**Scope:** PR 2 of the post-architecture-review series (issues #3 + #9). Stacks on
PR 3 (`feature/split-stream-api`), since it touches the socket loop's delivery
path that PR 3's `Stream`/`split` consumers read from.

## Problem

`socket_loop_split` runs a single `select!` that races three arms: outgoing
sends (`receiver.recv()`), inbound frames (`stream.next()`), and the keepalive
timer (`sleep(interval)`). When an inbound data frame arrives,
`socket_message_received` delivers it with `rx_sender.send(message).await`. If
the consumer has fallen behind and the rx channel (default `CHANNEL_SIZE = 4`)
is full, that `.await` blocks **inside the `select!` arm** — so the loop stops
servicing the other arms: it no longer pongs incoming pings and no longer fires
keepalives. The server sees an unresponsive client and drops the connection.
For a bursty data feed with an occasionally-slow consumer this is a latent
disconnect whose cause (protocol timeout) is opaque and misleading.

Issue #9 is the adjacent observation that the default buffer of `4` is far too
small for a feed.

## Goals

- A slow or briefly-paused consumer must never cause a protocol-timeout
  disconnect.
- Zero data loss and bounded memory: all delivered frames reach the consumer,
  in order; the loop never buffers unboundedly.
- Keep the change minimal and non-breaking (no public API signature changes).

## Non-goals

- Dropping/overflow policies (latest-only feeds). The chosen philosophy is
  preserve-all-data backpressure, not drop.
- Removing or changing the `CHANNEL_SIZE` const generic (only its default
  value changes).
- Guaranteeing liveness against a server that demands prompt pongs to its own
  pings during *sustained* overload (see Accepted caveat).

## Design

### 1. Deliver via a held permit, never a blind blocking send

When an inbound data frame (`Text`/`Binary`) arrives, attempt non-blocking
delivery first: `rx_sender.try_send(frame)`.

- `Ok(())` — fast path, continue the main loop.
- `Err(TrySendError::Closed(_))` — the consumer half was dropped; terminate
  with `Error::SocketeerDroppedWithoutClosing` (same as today's closed-channel
  handling).
- `Err(TrySendError::Full(frame))` — enter the **backpressure sub-loop** holding
  exactly that one `frame`.

The backpressure sub-loop `select!`s on:

- `rx_sender.reserve()` → a permit becomes available (consumer freed a slot) →
  `permit.send(frame)`; return `LoopState::Running` to the main loop, which
  resumes reading the socket and pongs any piled-up pings in order.
- `receiver.recv()` (outgoing) → keep sending the user's outbound frames via the
  existing send path; propagate any non-`Running` `LoopState` it produces
  (e.g. a socket error or `SocketeerDroppedWithoutClosing`).
- `sleep(keepalive_interval)` → fire a keepalive (the existing
  `send_keepalive`); propagate a non-`Running` result if the keepalive send
  fails (peer gone). Only present when `keepalive_interval` is `Some`.

Crucially, the sub-loop does **not** poll `stream.next()`. Holding the one
undelivered frame and not reading further means: memory is bounded to a single
in-flight frame, and unread socket bytes stay in the OS/TCP receive buffer,
applying genuine end-to-end backpressure to the server (its writes block via TCP
flow control). No data is lost; ordering is preserved (the held frame is
delivered before the loop reads the next one).

### 2. Liveness during backpressure comes from our keepalive pings

Because the sub-loop does not read inbound frames, it does not pong the server's
own pings until the consumer drains and the main loop resumes. The connection is
kept alive by our outgoing keepalive pings (default interval 2s). If a keepalive
`sink.send` fails, that surfaces as the loop's terminal error (the peer is gone)
rather than a hang.

### 3. Raise the channel default (issue #9)

Change the `CHANNEL_SIZE` const-generic **default** from `4` to `256`. The const
generic is retained (non-breaking) as the tuning knob. A larger default means
transient consumer hiccups fit entirely within the channel and never trigger the
backpressure sub-loop (and therefore never open a no-pong window); backpressure
engages only under sustained overload, which is exactly when riding it out
gracefully matters. This is why no internal "keep ponging while backpressured"
buffer is added — the larger channel already absorbs the transient case where
ponging-through would matter, at a fraction of the complexity.

## Accepted caveat

During *sustained* backpressure the loop relies solely on our keepalive pings
for liveness and does not pong the server's pings until it resumes reading. This
is safe for servers that treat client keepalive pings as liveness (the common
case) but a server that strictly requires prompt pongs to its own pings within a
tight window could still drop the connection under sustained overload. This is an
accepted trade-off versus the complexity of bounded ping-through buffering.

Additionally, backpressure resilience depends on keepalive being enabled (the
default). With `keepalive_interval = None`, a stalled consumer sends nothing
proactively during backpressure and the connection may still eventually drop;
this will be documented.

## Components touched

- `src/socket_loop.rs`: the delivery path (currently `sender.send(message).await`
  in `socket_message_received`) is restructured to `try_send` + a backpressure
  sub-loop. The sub-loop needs access to the rx sender, the outgoing receiver,
  the sink, and the keepalive config — so the delivery decision moves up into /
  alongside `socket_loop_split` rather than staying buried in the
  `socket_message_received` helper. Keep the helper focused (ping/pong/close +
  the data-frame hand-off); the backpressure wait belongs in the loop.
- `src/lib.rs`: change the `CHANNEL_SIZE` default in the `Socketeer` struct
  declaration from `4` to `256`. Update the type-parameter doc comment.
- Possibly `src/config.rs` doc: note the keepalive dependency for backpressure.

## Testing

Integration tests (mocking feature):

1. **No-loss-under-backpressure / no-disconnect.** Connect with a small
   `CHANNEL_SIZE` (e.g. `2`) and a short keepalive interval. Have the server
   emit a burst of N messages (N > buffer). On the client, deliberately do not
   read for longer than the keepalive interval (so a keepalive must fire during
   backpressure), then drain all N. Assert: all N received **in order**, none
   lost, and the connection is still alive afterward (round-trip one more
   message, then close cleanly).
2. **Outgoing sends still flow during backpressure** (optional, if cleanly
   testable): while the consumer is paused and the channel full, a `send` from
   another task still reaches the server.
3. **Dead peer during backpressure is detected via keepalive** (keepalive
   enabled). While the consumer is paused and the channel is full, the server
   drops the connection abruptly. The next keepalive `sink.send` fails, so the
   loop terminates with a `WebsocketError` (surfaced via the terminal-error
   path) rather than hanging. Document the inherent limitation that with
   `keepalive_interval = None`, a dead peer is not detected until the consumer
   drains and the loop resumes reading — do not test that as a guarantee.

A new mock-server capability may be needed: a server mode that emits a burst of
M messages on request (so the client can overflow its buffer deterministically).
Reuse `EchoControlMessage` with a new variant (e.g. `Burst(u32)`) or a dedicated
burst server.

## Acceptance criteria

- A consumer that pauses past the keepalive interval and then resumes loses no
  messages and keeps the connection alive (covered by Test 1).
- `cargo test --all-features` green (existing + new tests).
- `cargo clippy --all-features --tests --benches -- -Dclippy::all
  -Dclippy::pedantic` clean; clean across `--no-default-features` and
  `--features tracing`.
- `cargo fmt --check` clean.
- Public API signatures unchanged except the `CHANNEL_SIZE` default value.
- `#![deny(missing_docs)]` satisfied.

## Open/Deferred

- Drop/latest-only overflow policy: deferred (different philosophy; revisit only
  if a concrete feed needs it).
- Ping-through bounded buffering during sustained backpressure: deferred in
  favor of the larger default channel.
- Migrating buffer sizing from the const generic to a runtime `ConnectOptions`
  field: deferred (would be a larger API change; the const generic plus a saner
  default suffices).
