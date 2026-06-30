# Changelog

All notable changes to this project will be documented in this file.

## [0.5.0] - 2026-06-30

### Features

- Add Socketeer::split into cloneable Tx and receive Rx halves([dc7f9ff](https://github.com/zheylmun/socketeer/commit/dc7f9ff7a4c5bcca8b2beb7f49fe603bcd9c2801))
- Add fire-and-forget send_unconfirmed to SocketeerTx([35a9b8b](https://github.com/zheylmun/socketeer/commit/35a9b8bddc0a42ade8f1e8cce58bb25092b587e8))
- Implement Stream for SocketeerRx([7a3f80e](https://github.com/zheylmun/socketeer/commit/7a3f80e34e0609a6468d7c598e3a4e642f612294))
- Add SocketeerRx::reunite and ReuniteError([5eb9aa1](https://github.com/zheylmun/socketeer/commit/5eb9aa152014e688def240f425c7fad731cee1f5))
- Implement Stream for Socketeer([27f16d8](https://github.com/zheylmun/socketeer/commit/27f16d81e27a9ce94f07e7e15548fce3a5a609ca))
- Keep protocol alive under receive backpressure([d102d7a](https://github.com/zheylmun/socketeer/commit/d102d7af9492de32a2c7b6285f31ccbaa4bc8433))
- Raise default channel size to 256 and document backpressure([cc74966](https://github.com/zheylmun/socketeer/commit/cc749665456f8a61db318fadbb6dd9dd7bae148d))
- Re-export Message, Bytes, http, tungstenite, and WebSocketStreamType([f4b5453](https://github.com/zheylmun/socketeer/commit/f4b54532e1a7b237d1e576b78dc98718b5bb7369))
- [**breaking**] ConnectOptions builder with private fields, non_exhaustive([978b36a](https://github.com/zheylmun/socketeer/commit/978b36af0cbc041254cda55012f6aceef538cdce))
- [**breaking**] Mark Error #[non_exhaustive]([590d98e](https://github.com/zheylmun/socketeer/commit/590d98ea620098d6601962887580e776783b1df5))

### Bug Fixes

- Surface the real disconnect cause to the consumer([3b5c08f](https://github.com/zheylmun/socketeer/commit/3b5c08f0023390ba7009a07a658fca4ba42d288d))
- Align split-half CHANNEL_SIZE defaults to 256 and tighten docs([8caa0aa](https://github.com/zheylmun/socketeer/commit/8caa0aad7cf2fdf523d6061712f6c88108b1e58f))
- Export ConnectOptionsBuilder, correct header() doc, and strengthen test([12c034a](https://github.com/zheylmun/socketeer/commit/12c034a0f4e8e9bfb985ffc316e3d6315b76fbd5))

### Refactor

- Extract socket loop into its own module, move e2e tests to tests/([b7ec286](https://github.com/zheylmun/socketeer/commit/b7ec2861c151257bb6fdfb7d4a6a518e0a178215))
- Make TxChannelPayload response channel optional([7dc850c](https://github.com/zheylmun/socketeer/commit/7dc850c1cd857058219644431c23e527d5bbdf17))
- Share receive path between handle and split halves([4d84f9c](https://github.com/zheylmun/socketeer/commit/4d84f9c65eea9fb8ecb6e74858daf3123c8798eb))
- [**breaking**] Move backpressure_probe_server into the test suite([84b42fa](https://github.com/zheylmun/socketeer/commit/84b42fa15c47c082df3906538a4f33ec6d481a35))

### Documentation

- Design spec for backpressure without protocol starvation (PR 2)([d9c7088](https://github.com/zheylmun/socketeer/commit/d9c708860821e7247bf006ba2082dfb3b065b28c))
- Implementation plan for backpressure resilience (PR 2)([6a5dc4f](https://github.com/zheylmun/socketeer/commit/6a5dc4fbf3af3bb6f2f5825bba5fb90739582085))
- Design for public API cleanup (breaking)([049f0e0](https://github.com/zheylmun/socketeer/commit/049f0e00349f6a003f05c41fa8f19c8aac20f145))
- Implementation plan for public API cleanup([9dc41cb](https://github.com/zheylmun/socketeer/commit/9dc41cbcbadeb0de25a0db649282ba5f365d7419))

### Testing

- Cover concurrent cloned-Tx sends and ReuniteError Display/Error([9f0aaab](https://github.com/zheylmun/socketeer/commit/9f0aaabf0988746b22b3b18eea74e722e902cc8c))
- Add backpressure probe mock server([2420313](https://github.com/zheylmun/socketeer/commit/24203136d04083b69f3c20691d4f9ebae39c0c75))
- Cover backpressure delivery without keepalives([62fa77d](https://github.com/zheylmun/socketeer/commit/62fa77db47b770afdebc44ff25c80f7b0790eb28))
- Cover outgoing sends during backpressure([48d57c9](https://github.com/zheylmun/socketeer/commit/48d57c9f2cdbc3454fa79104c80121743f063907))

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
