# Changelog

All notable changes to this project will be documented in this file.

## [0.4.0] - 2026-05-08

### Features

- [**breaking**] Add Codec abstraction with JSON, MessagePack, and Raw codecs([efc1d5a](https://github.com/zheylmun/socketeer/commit/efc1d5a22e12a40d2584626f49e42930e9725902))

### Bug Fixes

- Mark Error::Codec inner as #[source]([ffdca32](https://github.com/zheylmun/socketeer/commit/ffdca32e38aed73c8b656590264da4337f83d848))
- Surface peer close as WebsocketClosed in HandshakeContext::recv([cd67288](https://github.com/zheylmun/socketeer/commit/cd6728804ae8229a0f7a62172c460022c59acb35))

### Testing

- Add coverage for Codec trait and new HandshakeContext paths([aeb3c89](https://github.com/zheylmun/socketeer/commit/aeb3c8964ae9a8d21fc67a4b31fb4a9700c7c525))
- Push coverage to 98% lines / 96% regions([09d7364](https://github.com/zheylmun/socketeer/commit/09d7364f9e0ee018c5d111335c912544a96eabed))
- Clarify raw-API test names and cover send_raw directly([b10d987](https://github.com/zheylmun/socketeer/commit/b10d98707a3a140daba6dc0fd8d0d41a3cd64ef7))

## [0.3.0] - 2026-04-10

### Features

- Add ConnectOptions, configurable keepalive, and connect_with([5467716](https://github.com/zheylmun/socketeer/commit/546771611b7dd83f8addd17ae1394a562a4c418b))
- Add send_raw and next_raw_message methods([3652f25](https://github.com/zheylmun/socketeer/commit/3652f25056e2aac9505fe767336ab2da080cb452))
- Add ConnectionHandler trait for lifecycle hooks([d3d1048](https://github.com/zheylmun/socketeer/commit/d3d1048f13e785ee9dd7d3acf04faeab73b8cbdb))

### Bug Fixes

- *(ci)* Enable all features for coverage so tests are included([142abe8](https://github.com/zheylmun/socketeer/commit/142abe8d17cf624da5cf29856a1783d6ecdd3b80))

### Testing

- Add auth_echo_server and tests for new features([7f9cbfe](https://github.com/zheylmun/socketeer/commit/7f9cbfea0d35c1059560925e9ac78b97a30da489))

### Miscellaneous Tasks

- Bump version to 0.3.0, update README([4c74ff7](https://github.com/zheylmun/socketeer/commit/4c74ff72cf1f6585883992952bb8b3636c15746a))
