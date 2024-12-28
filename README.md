# Socketeer

[![Crates.io Version](https://img.shields.io/crates/v/compass_data?style=for-the-badge)](https://crates.io/crates/socketeer)
[![docs.rs](https://img.shields.io/docsrs/compass_data?style=for-the-badge)](https://docs.rs/socketeer)
[![GitHub branch status](https://img.shields.io/github/checks-status/zheylmun/compass_data/main?style=for-the-badge&logo=GitHub)](https://github.com/zheylmun/socketeer/actions)
[![Codecov](https://img.shields.io/codecov/c/github/zheylmun/compass_data?style=for-the-badge&logo=CodeCov)](https://app.codecov.io/gh/zheylmun/socketeer)

Socketeer is a simple wrapper which creates a tokio-tungstenite websocket connection and provides a simple async API to send and receive messages.
It automatically handles the underlying websocket and allows for reconnection, as well as immediate handling of errors in client code.
Currently supports only json encoded binary and text messages.
