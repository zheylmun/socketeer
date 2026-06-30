# Public API cleanup for the pre-1.0 breaking change

**Status:** Approved design — ready for implementation planning
**Date:** 2026-06-29
**Scope:** A single breaking-change branch tightening the public API surface
before 1.0. Three themes: stop leaking foreign types, future-proof the
enums/config, and clean the `mocking` surface.

## Problem

A public-API review of socketeer (currently v0.4.0) found the boundary leaks
implementation details and is not future-proofed for additive changes:

1. **Foreign crate types appear in public signatures without being
   re-exported.** `tungstenite::Message`, `tungstenite::Error`,
   `http::HeaderMap`, and `bytes::Bytes` all surface through the public API
   (`send_raw`, `next_raw_message`, `RawCodec`, `Error::WebsocketError`,
   `Error::UnexpectedMessageType`, `ConnectOptions::custom_keepalive_message`,
   `ConnectOptions::extra_headers`, `HandshakeContext::{recv_raw, send_binary}`).
   None is re-exported, so a downstream user must add direct dependencies on
   those crates and keep versions in lockstep with socketeer. A tungstenite
   bump silently breaks them at the type level.
2. **`Error` and `ConnectOptions` are not `#[non_exhaustive]`.** Adding an
   `Error` variant or a `ConnectOptions` field is a breaking change, and
   downstream `match`/struct-literal use can't be forced to tolerate growth.
3. **The `mocking` surface leaks an internal fixture and hides the type needed
   to use its generic entry point.** `backpressure_probe_server` is a
   socketeer-specific test fixture (emits `"burst-0".."burst-4"` / `"alive"`)
   that became public only because the integration tests reach it through the
   crate root. Separately, `get_mock_address<F: Fn(WebSocketStreamType) -> R>`
   requires naming `WebSocketStreamType`, which is `pub(crate)` and never
   exported — so the generic entry point is unusable for downstream custom
   servers.

## Goals

- No foreign crate type appears in the public API without a socketeer
  re-export providing a coherent dependency story.
- `Error` and `ConnectOptions` can gain variants/options later without a
  breaking change.
- The `mocking` feature exposes only general-purpose utilities, and its
  generic entry point is actually usable downstream.
- All existing behavior is preserved; this is an API-shape change, not a
  behavior change.

## Non-goals

- No change to the socket loop, backpressure, codec, or handler behavior.
- No new connection features; `ConnectOptions` keeps exactly today's three
  settings, only its construction changes.
- Not removing the `CHANNEL_SIZE` const generic (the zero-capacity guard is a
  deferred nicety, see Deferred).

## Design

### A. Re-export the foreign types the API already exposes

Add to the crate root:

```rust
pub use bytes::Bytes;
pub use tokio_tungstenite::tungstenite::{self, Message};
pub use tokio_tungstenite::tungstenite::http;
```

`tungstenite::Error` is reachable through the re-exported `tungstenite`
module, and `http::HeaderMap` through the re-exported `http`. Downstream code
then names `socketeer::Message`, `socketeer::http::HeaderMap`,
`socketeer::Bytes`, `socketeer::tungstenite::Error` — no direct dependency or
version-matching required. No signatures change; only re-exports are added.

### B. Future-proof `Error` and `ConnectOptions`

- Mark `Error` `#[non_exhaustive]`. Existing variants and their data are
  unchanged.
- Convert `ConnectOptions` to **private fields + a builder**, and mark it
  `#[non_exhaustive]`:
  - Fields (`extra_headers`, `keepalive_interval`, `custom_keepalive_message`)
    become private.
  - `ConnectOptions::default()` still yields today's defaults (2s keepalive,
    empty headers, no custom message).
  - A `ConnectOptionsBuilder` (obtained via `ConnectOptions::builder()`)
    exposes:
    - `keepalive_interval(Option<Duration>) -> Self`
    - `header(name, value) -> Self` (append one header)
    - `headers(HeaderMap) -> Self` (replace the header map)
    - `custom_keepalive_message(Option<Message>) -> Self`
    - `build() -> ConnectOptions`
  - Read access for the internals stays `pub(crate)` (the crate reads the
    fields directly today in `lib.rs`/`config.rs`); no public getters are
    added unless a consumer needs them (YAGNI).

  This removes the public-fields semver hazard and makes future options
  additive (a new builder method, no call-site break).

### C. Clean the `mocking` surface

- **Move `backpressure_probe_server` out of `src/mock_server.rs` into a
  `tests/` helper module** (e.g. `tests/support/mod.rs` or an inline module in
  `tests/integration.rs`). It is consumed only by the backpressure integration
  tests. It leaves the published API entirely.
- **Export `WebSocketStreamType`**: change `pub(crate) use
  socket_loop::WebSocketStreamType` to a public re-export so downstream users
  can write `get_mock_address(|ws: WebSocketStreamType| async { ... })`. The
  generic `get_mock_address` and the bundled `echo_server` / `auth_echo_server`
  / `msgpack_echo_server` stay as they are.

## Components touched

- `src/lib.rs`: add the foreign-type re-exports (A); make
  `WebSocketStreamType` a public re-export (C). Update any internal field
  reads of `ConnectOptions` to use the new accessors/fields (B).
- `src/error.rs`: add `#[non_exhaustive]` to `Error` (B).
- `src/config.rs`: private fields, `#[non_exhaustive]`, builder (B). Internal
  reads in `lib.rs` use `pub(crate)` field access.
- `src/mock_server.rs`: remove `backpressure_probe_server` (C).
- `tests/integration.rs` (+ a `tests/` support module): host
  `backpressure_probe_server`; update `ConnectOptions` construction to the
  builder (B).
- `README.md` and any doctests: update `ConnectOptions` construction to the
  builder (B).
- `CLAUDE.md` (gitignored locally): note the builder + re-exports if it lists
  API specifics — not committed.

## Testing

This is a refactor of the API shape; the existing suite (15 unit + 40
integration + 5 doc) is the safety net and must stay green after every task.
Add targeted tests only where new surface appears:

1. **`ConnectOptions` builder** unit tests: `builder().build()` equals
   `default()`; each setter overrides exactly its field; `header` appends and
   `headers` replaces.
2. **Re-export smoke**: a doc/integration test (or doctest) that names
   `socketeer::Message`, `socketeer::Bytes`, `socketeer::http::HeaderMap`,
   `socketeer::tungstenite::Error`, and `socketeer::WebSocketStreamType` to
   prove the re-exports resolve.
3. Existing backpressure tests keep passing after `backpressure_probe_server`
   moves into the test support module.

## Acceptance criteria

- No foreign crate type is reachable in the public API without a socketeer
  re-export (verified by the smoke test + manual scan).
- `Error` and `ConnectOptions` are `#[non_exhaustive]`; `ConnectOptions`
  fields are private and only constructible via `default()`/builder.
- `backpressure_probe_server` is no longer part of the published API;
  `WebSocketStreamType` is.
- `cargo test --all-features` green (existing + new).
- `cargo clippy --all-features --tests --benches -- -Dclippy::all
  -Dclippy::pedantic` clean, and clean across `--no-default-features` and
  `--features tracing`.
- `cargo fmt --check` clean.
- `#![deny(missing_docs)]` satisfied (builder methods documented).

## Deferred

- Compile-time `CHANNEL_SIZE > 0` guard (`const { assert!(...) }` in
  `connect_with_codec`): a real correctness-by-construction nicety, but
  orthogonal to the boundary cleanup; revisit separately.
- Public getters on `ConnectOptions`: add only when a downstream consumer
  needs to read options back (YAGNI for now).
