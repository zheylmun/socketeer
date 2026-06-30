# Public API Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tighten socketeer's public API before 1.0 — re-export leaked foreign types, future-proof `Error`/`ConnectOptions`, and clean the `mocking` surface — as one breaking-change branch.

**Architecture:** Four independent tasks. Task 1 adds re-exports (non-breaking). Task 2 converts `ConnectOptions` to a builder with private fields. Task 3 marks `Error` non-exhaustive. Task 4 moves an internal test fixture out of the published API. Tasks 2 and 4 also update in-repo call sites (README, doctests, integration tests).

**Tech Stack:** Rust 2024, `tokio-tungstenite`, `http`, `bytes`, `serde`/`serde_json`.

## Global Constraints

- Rust edition 2024, MSRV 1.85.
- `#![deny(missing_docs)]` — every new public item (builder, builder methods) must have a doc comment.
- CI gate per task: `cargo test --all-features` green; `cargo clippy --all-features --tests --benches -- -Dclippy::all -Dclippy::pedantic` clean, and clean under `--no-default-features` and `--features tracing`; `cargo fmt --check` clean.
- Conventional commits (feat/fix/refactor/test/docs). End commit messages with the trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- This is intentionally a breaking change; no need to preserve `ConnectOptions` public fields or pre-1.0 source compatibility.
- Behavior is unchanged throughout — only API shape changes. The existing suite (15 unit + 40 integration + 5 doc) is the regression net and must stay green after every task.

---

## File Structure

- `src/lib.rs` — crate root: add foreign-type re-exports; make `WebSocketStreamType` a public re-export; drop `backpressure_probe_server` from the `mocking` re-export (Task 4). Reconcile the existing private `Message` import with the new `pub use`.
- `src/config.rs` — `ConnectOptions` becomes `#[non_exhaustive]` with `pub(crate)` fields plus a `ConnectOptionsBuilder`.
- `src/error.rs` — `Error` becomes `#[non_exhaustive]`.
- `src/mock_server.rs` — remove `backpressure_probe_server` and its now-unused imports.
- `tests/support/mod.rs` — **new** shared test helper hosting `backpressure_probe_server` (built against socketeer's public re-exports).
- `tests/integration.rs` — `mod support;`; update `backpressure_probe_server` references; rewrite all 7 `ConnectOptions { .. }` literals to the builder.
- `tests/public_api.rs` — **new** smoke test that the re-exports resolve.
- `README.md` — rewrite the `ConnectOptions` example to the builder.

---

## Task 1: Re-export leaked foreign types and `WebSocketStreamType`

**Files:**
- Modify: `src/lib.rs:26` (the `pub(crate) use ... WebSocketStreamType`), `src/lib.rs:35` (existing `Message` import), and the re-export block near `src/lib.rs:11-24`.
- Test: `tests/public_api.rs` (create).

**Interfaces:**
- Produces: `socketeer::Message`, `socketeer::Bytes`, `socketeer::http` (module, providing `HeaderMap`/`HeaderName`/`HeaderValue`), `socketeer::tungstenite` (module, providing `Error`), and `socketeer::WebSocketStreamType` — all usable by Tasks 2 and 4 and by downstream crates.

- [ ] **Step 1: Write the failing smoke test**

Create `tests/public_api.rs`:

```rust
//! Compile-time proof that the foreign types socketeer exposes are re-exported,
//! so downstream users need no direct dependency on tungstenite/http/bytes.

#[test]
fn reexports_resolve() {
    // Each path must resolve purely through `socketeer::`.
    fn _takes_message(_: socketeer::Message) {}
    fn _takes_bytes(_: socketeer::Bytes) {}
    fn _takes_header_map(_: socketeer::http::HeaderMap) {}
    fn _takes_ws_error(_: socketeer::tungstenite::Error) {}
    fn _takes_ws_stream(_: socketeer::WebSocketStreamType) {}
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --all-features --test public_api`
Expected: FAIL to compile — `socketeer::Message`, `socketeer::Bytes`, `socketeer::http`, and `socketeer::WebSocketStreamType` are not yet public (`tungstenite` may already resolve via dependency, but the others will not).

- [ ] **Step 3: Add the re-exports**

In `src/lib.rs`, change the private `WebSocketStreamType` re-export at line 26 from:

```rust
pub(crate) use socket_loop::WebSocketStreamType;
```

to:

```rust
/// The concrete `WebSocketStream` type the mock-server handlers operate on.
/// Re-exported so downstream code can write custom servers for
/// [`get_mock_address`].
pub use socket_loop::WebSocketStreamType;
```

Add these re-exports alongside the other `pub use` lines (after line 24):

```rust
pub use bytes::Bytes;
pub use tokio_tungstenite::tungstenite::{self, Message, http};
```

Then reconcile the existing import at line 35. Change:

```rust
use tokio_tungstenite::{connect_async, tungstenite::Message};
```

to (the `pub use` above now brings `Message` into module scope):

```rust
use tokio_tungstenite::connect_async;
```

- [ ] **Step 4: Run the smoke test and the full suite**

Run: `cargo test --all-features --test public_api`
Expected: PASS.

Run: `cargo test --all-features`
Expected: all existing tests still pass (15 unit + 40 integration + 5 doc + the new smoke test).

- [ ] **Step 5: Clippy + fmt**

Run: `cargo clippy --all-features --tests --benches -- -Dclippy::all -Dclippy::pedantic`
Expected: clean (no `unused_imports`, no `private_interfaces`).
Run: `cargo fmt`

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs tests/public_api.rs
git commit -m "$(printf 'feat: re-export Message, Bytes, http, tungstenite, and WebSocketStreamType\n\nThese foreign types already appeared in the public API (send_raw,\nRawCodec, Error, ConnectOptions, HandshakeContext, get_mock_address) but\nwere not re-exported, forcing downstream version-matched direct deps.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

## Task 2: `ConnectOptions` builder with private fields and `#[non_exhaustive]`

**Files:**
- Modify: `src/config.rs` (struct, add builder; the `build_request` method and `Default` impl stay, reading fields directly in-crate).
- Modify: `src/lib.rs:165-166` (reads `options.keepalive_interval` / `options.custom_keepalive_message` — these are `pub(crate)` field reads and keep working unchanged; verify they compile).
- Modify: `README.md:77-79` (the `ConnectOptions::default()` + field-mutation example).
- Modify: `tests/integration.rs` — the 7 `ConnectOptions { .. }` literals (see Step 5).
- Test: inline `#[cfg(test)] mod tests` in `src/config.rs`.

**Interfaces:**
- Consumes: `socketeer::Message`, `socketeer::http` (from Task 1) — already in `config.rs` via `tokio_tungstenite::tungstenite::{Message, http}`.
- Produces:
  - `ConnectOptions` is `#[non_exhaustive]`; fields `extra_headers`, `keepalive_interval`, `custom_keepalive_message` become `pub(crate)`.
  - `ConnectOptions::builder() -> ConnectOptionsBuilder`
  - `ConnectOptionsBuilder::keepalive_interval(self, Option<Duration>) -> Self`
  - `ConnectOptionsBuilder::header<K: http::header::IntoHeaderName>(self, K, http::HeaderValue) -> Self` (appends one header)
  - `ConnectOptionsBuilder::headers(self, http::HeaderMap) -> Self` (replaces the map)
  - `ConnectOptionsBuilder::custom_keepalive_message(self, Option<Message>) -> Self`
  - `ConnectOptionsBuilder::build(self) -> ConnectOptions`

- [ ] **Step 1: Write the failing builder unit tests**

Add to the bottom of `src/config.rs` (in-crate test reads `pub(crate)` fields directly):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_default_matches_default() {
        let built = ConnectOptions::builder().build();
        let default = ConnectOptions::default();
        assert_eq!(built.keepalive_interval, default.keepalive_interval);
        assert_eq!(built.custom_keepalive_message, default.custom_keepalive_message);
        assert_eq!(built.extra_headers, default.extra_headers);
    }

    #[test]
    fn builder_sets_each_field() {
        let opts = ConnectOptions::builder()
            .keepalive_interval(None)
            .custom_keepalive_message(Some(Message::text("ping")))
            .header("x-test", "v".parse().unwrap())
            .build();
        assert_eq!(opts.keepalive_interval, None);
        assert_eq!(opts.custom_keepalive_message, Some(Message::text("ping")));
        assert_eq!(opts.extra_headers.get("x-test").unwrap(), "v");
    }

    #[test]
    fn headers_replaces_whole_map() {
        let mut map = http::HeaderMap::new();
        map.insert("a", "1".parse().unwrap());
        map.insert("b", "2".parse().unwrap());
        let opts = ConnectOptions::builder().headers(map).build();
        assert_eq!(opts.extra_headers.len(), 2);
        assert_eq!(opts.extra_headers.get("a").unwrap(), "1");
    }
}
```

> Note: `Message::text("ping")` constructs a `Message::Text`. If the installed
> tungstenite version lacks that constructor, use
> `Message::Text("ping".into())` instead.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --all-features --lib config`
Expected: FAIL to compile — `ConnectOptions::builder` does not exist.

- [ ] **Step 3: Implement the builder and lock down the struct**

Replace the struct definition and `impl ConnectOptions` block in `src/config.rs` (lines 11-56). The struct keeps its doc comments; fields become `pub(crate)`; add `#[non_exhaustive]`:

```rust
/// Configuration options for a WebSocket connection.
///
/// Controls HTTP headers on the upgrade request, keepalive behavior,
/// and other connection-level settings. Construct via
/// [`ConnectOptions::default`] or [`ConnectOptions::builder`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ConnectOptions {
    /// Extra HTTP headers for the WebSocket upgrade request (e.g., cookies, auth tokens).
    pub(crate) extra_headers: http::HeaderMap,
    /// Idle timeout before sending a keepalive. `None` disables keepalives entirely.
    /// Defaults to 2 seconds.
    ///
    /// Keepalives also keep the connection alive while the receive buffer is
    /// full and the reader is applying backpressure; with keepalives disabled,
    /// while the receive buffer is full the loop is not reading inbound frames
    /// (so it will not observe a server close), and a consumer that stops
    /// reading entirely can leave the connection parked until it resumes — in
    /// addition to the server potentially dropping the idle connection.
    pub(crate) keepalive_interval: Option<Duration>,
    /// If set, send this message as the keepalive instead of a WebSocket Ping frame.
    /// Useful for APIs that expect a custom keepalive payload — e.g. Interactive
    /// Brokers' literal text `tic`, or a binary heartbeat in a msgpack protocol.
    pub(crate) custom_keepalive_message: Option<Message>,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            extra_headers: http::HeaderMap::new(),
            keepalive_interval: Some(Duration::from_secs(2)),
            custom_keepalive_message: None,
        }
    }
}

impl ConnectOptions {
    /// Start building a [`ConnectOptions`] from the defaults.
    #[must_use]
    pub fn builder() -> ConnectOptionsBuilder {
        ConnectOptionsBuilder {
            options: ConnectOptions::default(),
        }
    }

    /// Build an HTTP request from the URL and configured headers.
    ///
    /// Uses tungstenite's `IntoClientRequest` to generate a properly formed
    /// WebSocket upgrade request, then appends any extra headers.
    pub(crate) fn build_request(&self, url: &Url) -> Result<http::Request<()>, Error> {
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(Error::WebsocketError)?;
        for (key, value) in &self.extra_headers {
            request.headers_mut().insert(key, value.clone());
        }
        Ok(request)
    }
}

/// Builder for [`ConnectOptions`]. Obtain one with [`ConnectOptions::builder`],
/// chain setters, and finish with [`ConnectOptionsBuilder::build`].
#[derive(Clone, Debug)]
pub struct ConnectOptionsBuilder {
    options: ConnectOptions,
}

impl ConnectOptionsBuilder {
    /// Set the idle timeout before a keepalive is sent. `None` disables keepalives.
    #[must_use]
    pub fn keepalive_interval(mut self, interval: Option<Duration>) -> Self {
        self.options.keepalive_interval = interval;
        self
    }

    /// Append a single HTTP header to the WebSocket upgrade request.
    #[must_use]
    pub fn header<K: http::header::IntoHeaderName>(
        mut self,
        name: K,
        value: http::HeaderValue,
    ) -> Self {
        self.options.extra_headers.insert(name, value);
        self
    }

    /// Replace the entire set of extra HTTP headers.
    #[must_use]
    pub fn headers(mut self, headers: http::HeaderMap) -> Self {
        self.options.extra_headers = headers;
        self
    }

    /// Send this message as the keepalive instead of a WebSocket Ping frame.
    #[must_use]
    pub fn custom_keepalive_message(mut self, message: Option<Message>) -> Self {
        self.options.custom_keepalive_message = message;
        self
    }

    /// Finish building, producing the configured [`ConnectOptions`].
    #[must_use]
    pub fn build(self) -> ConnectOptions {
        self.options
    }
}
```

- [ ] **Step 4: Run the unit tests**

Run: `cargo test --all-features --lib config`
Expected: PASS (3 new tests).

- [ ] **Step 5: Update all in-repo construction sites to the builder**

In `README.md`, replace lines 77-79:

```rust
let mut options = ConnectOptions::default();
options.extra_headers.insert("Authorization", "Bearer my-token".parse().unwrap());
options.keepalive_interval = Some(Duration::from_secs(10));
```

with:

```rust
let options = ConnectOptions::builder()
    .header("Authorization", "Bearer my-token".parse().unwrap())
    .keepalive_interval(Some(Duration::from_secs(10)))
    .build();
```

In `tests/integration.rs`, rewrite each literal:

- `test_*` at ~line 169 — `ConnectOptions { keepalive_interval: None, ..ConnectOptions::default() }` →
  ```rust
  let options = ConnectOptions::builder().keepalive_interval(None).build();
  ```
- `test_extra_headers_used` at ~line 391 — replace the `let mut headers = ...; headers.insert(...); let options = ConnectOptions { extra_headers: headers, ..default() };` block with:
  ```rust
  let options = ConnectOptions::builder()
      .header("X-Test-Header", "socketeer".parse().unwrap())
      .build();
  ```
- ~line 765 — `keepalive_interval: Some(Duration::from_millis(100))` →
  ```rust
  let options = ConnectOptions::builder()
      .keepalive_interval(Some(Duration::from_millis(100)))
      .build();
  ```
- ~line 799 — keepalive + custom message →
  ```rust
  let options = ConnectOptions::builder()
      .keepalive_interval(Some(Duration::from_millis(100)))
      .custom_keepalive_message(Some(Message::Binary(Bytes::from_static(b"keepalive"))))
      .build();
  ```
- ~line 827 — keepalive 100ms → same builder form as line 765.
- ~line 870 — `keepalive_interval: None` → `ConnectOptions::builder().keepalive_interval(None).build();`
- ~line 909 — keepalive 100ms → same builder form as line 765.

> The `Message` / `Bytes` referenced in the line-799 case resolve via the
> existing test imports; if they currently come from `tokio_tungstenite`,
> prefer the `socketeer::{Message, Bytes}` re-exports added in Task 1.

- [ ] **Step 6: Run the full suite, clippy, fmt**

Run: `cargo test --all-features`
Expected: all pass (including the rewritten integration tests and the README doctest).
Run: `cargo clippy --all-features --tests --benches -- -Dclippy::all -Dclippy::pedantic`
Expected: clean.
Run: `cargo fmt`

- [ ] **Step 7: Commit**

```bash
git add src/config.rs README.md tests/integration.rs
git commit -m "$(printf 'feat!: ConnectOptions builder with private fields, non_exhaustive\n\nReplace public-field construction with ConnectOptions::builder() and make\nthe struct non_exhaustive so future options are additive. Fields become\npub(crate); internal reads are unchanged.\n\nBREAKING CHANGE: ConnectOptions fields are no longer public; construct via\nConnectOptions::builder() or ::default().\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

## Task 3: Mark `Error` `#[non_exhaustive]`

**Files:**
- Modify: `src/error.rs:6` (add the attribute).

**Interfaces:**
- Produces: `Error` is `#[non_exhaustive]` — downstream `match` blocks must carry a wildcard arm. No variant data changes.

- [ ] **Step 1: Add the attribute**

In `src/error.rs`, change:

```rust
#[derive(Debug, Error)]
pub enum Error {
```

to:

```rust
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
```

> This is an attribute-only semver change with no behavior to assert in a unit
> test (in-crate matches are unaffected by `#[non_exhaustive]`). The gate is a
> clean compile plus the existing suite; the smoke test from Task 1 already
> names `socketeer::tungstenite::Error`, and `Error` is exercised throughout
> the integration suite.

- [ ] **Step 2: Run the full suite**

Run: `cargo test --all-features`
Expected: all pass (no behavior change).

- [ ] **Step 3: Clippy + fmt**

Run: `cargo clippy --all-features --tests --benches -- -Dclippy::all -Dclippy::pedantic`
Expected: clean.
Run: `cargo fmt`

- [ ] **Step 4: Commit**

```bash
git add src/error.rs
git commit -m "$(printf 'feat!: mark Error #[non_exhaustive]\n\nAllows new error variants to be added without a breaking change; downstream\nmatches must now include a wildcard arm.\n\nBREAKING CHANGE: Error is now non_exhaustive.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

## Task 4: Move `backpressure_probe_server` out of the published API

**Files:**
- Create: `tests/support/mod.rs` (shared helper, not its own test binary).
- Modify: `src/mock_server.rs` — remove `backpressure_probe_server` (lines ~207-265) and its now-unused `use std::time::Duration;` and `use tokio::time::timeout;` imports (lines 6 and 9). Verify no other item in the file uses them.
- Modify: `src/lib.rs` — drop `backpressure_probe_server` from the `mocking` re-export list (line ~22).
- Modify: `tests/integration.rs` — add `mod support;` and change the import/usages from the crate's `backpressure_probe_server` to `support::backpressure_probe_server`.

**Interfaces:**
- Consumes: `socketeer::{WebSocketStreamType, EchoControlMessage, Message, Bytes}` (Task 1 + existing) and `socketeer::tungstenite::Error` for the return type.
- Produces: `support::backpressure_probe_server` for the integration tests; `backpressure_probe_server` is no longer part of the published API.

- [ ] **Step 1: Create the shared test helper**

Create `tests/support/mod.rs` (a `tests/<dir>/mod.rs` file is compiled as a module of including test crates, not as its own test binary):

```rust
//! Shared helpers for the integration tests.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use socketeer::{Bytes, EchoControlMessage, Message, WebSocketStreamType, tungstenite};
use tokio::time::timeout;

/// Probe server for the backpressure tests.
///
/// On connect it immediately sends a burst of five `EchoControlMessage::Message`
/// frames (`"burst-0"`..`"burst-4"`), then waits up to 500ms for a client
/// keepalive [`Message::Ping`]. It sends a final `"alive"` frame ONLY if that
/// ping arrives — so a client whose socket loop is stalled by a full receive
/// buffer (and therefore cannot send keepalives) never receives the marker.
/// After that it pongs and drains until the client closes.
///
/// # Errors
/// - If sending a burst frame, the marker, or a pong fails
/// # Panics
/// - If serializing an [`EchoControlMessage`] to JSON fails (infallible in practice)
pub async fn backpressure_probe_server(
    ws: WebSocketStreamType,
) -> Result<bool, tungstenite::Error> {
    let (mut sink, mut stream) = ws.split();

    for i in 0..5u32 {
        let msg = EchoControlMessage::Message(format!("burst-{i}"));
        let text = serde_json::to_string(&msg).unwrap();
        sink.send(Message::Text(text.into())).await?;
    }

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

    while let Some(Ok(frame)) = stream.next().await {
        match frame {
            Message::Ping(_) => sink.send(Message::Pong(Bytes::new())).await?,
            Message::Close(_) => {
                sink.close().await?;
                break;
            }
            _ => {}
        }
    }
    Ok(true)
}
```

> `serde_json` and `futures` are already dev-dependencies (the integration tests
> use them). If `cargo test` reports either as missing for the test target,
> add it under `[dev-dependencies]` in `Cargo.toml` — do not add a non-dev dep.

- [ ] **Step 2: Remove the fixture from the published API**

In `src/mock_server.rs`, delete the entire `backpressure_probe_server` function and its doc comment (lines ~207-265). Then delete the now-unused imports `use std::time::Duration;` (line 6) and `use tokio::time::timeout;` (line 9) — confirm via `grep -n "Duration\|timeout" src/mock_server.rs` that no other item references them.

In `src/lib.rs`, change the `mocking` re-export (line ~20-23) from:

```rust
pub use mock_server::{
    EchoControlMessage, auth_echo_server, backpressure_probe_server, echo_server, get_mock_address,
};
```

to:

```rust
pub use mock_server::{
    EchoControlMessage, auth_echo_server, echo_server, get_mock_address,
};
```

- [ ] **Step 3: Point the integration tests at the helper**

In `tests/integration.rs`:

- Remove `backpressure_probe_server` from the `use socketeer::{...}` import (line ~15).
- Add a module declaration near the top, after the imports: `mod support;`
- Replace each `get_mock_address(backpressure_probe_server)` (4 call sites) with `get_mock_address(support::backpressure_probe_server)`.

- [ ] **Step 4: Run the full suite**

Run: `cargo test --all-features`
Expected: all pass — the four backpressure tests now drive `support::backpressure_probe_server` through the public `get_mock_address`/`WebSocketStreamType`, exercising the very capability Task 1 exported.

Run: `cargo test --no-default-features` and `cargo test --features tracing`
Expected: compile and pass (the `mocking`-gated tests are skipped without the feature; ensure nothing references the removed symbol unconditionally).

- [ ] **Step 5: Clippy + fmt**

Run: `cargo clippy --all-features --tests --benches -- -Dclippy::all -Dclippy::pedantic`
Expected: clean (no unused imports in `mock_server.rs`).
Run: `cargo fmt`

- [ ] **Step 6: Commit**

```bash
git add src/mock_server.rs src/lib.rs tests/integration.rs tests/support/mod.rs
git commit -m "$(printf 'refactor!: move backpressure_probe_server into the test suite\n\nIt is a socketeer-specific test fixture, not a general-purpose mock; it\nonly reached the public API via the crate root. It now lives in\ntests/support and drives the public get_mock_address/WebSocketStreamType\nentry point.\n\nBREAKING CHANGE: backpressure_probe_server is no longer re-exported.\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

## Self-Review

**1. Spec coverage:**
- A (re-export foreign types) → Task 1 (`Message`, `Bytes`, `http`, `tungstenite`) ✅
- C-export (`WebSocketStreamType`) → Task 1 ✅
- B-config (builder + private fields + non_exhaustive) → Task 2 ✅
- B-error (non_exhaustive) → Task 3 ✅
- C-move (`backpressure_probe_server` → tests) → Task 4 ✅
- Testing items 1 (builder tests) → Task 2 Step 1; 2 (re-export smoke) → Task 1 Step 1; 3 (backpressure tests still pass after move) → Task 4 Step 4 ✅
- Deferred items (CHANNEL_SIZE guard, public getters) correctly excluded ✅

**2. Placeholder scan:** No TBD/TODO; all code blocks are complete; the two notes (Message constructor variant, dev-dependency check) are contingency guidance, not placeholders.

**3. Type consistency:** `ConnectOptions::builder()`, `ConnectOptionsBuilder`, and the four setters + `build` are named identically in the interface block, the implementation (Task 2 Step 3), and the call-site rewrites (Step 5). `WebSocketStreamType`, `Message`, `Bytes`, `http`, `tungstenite` re-export names match between Task 1 and their consumers in Tasks 2 and 4.
