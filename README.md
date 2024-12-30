# socketeer

[![Crates.io Version](https://img.shields.io/crates/v/socketeer?style=for-the-badge)](https://crates.io/crates/socketeer)
[![docs.rs](https://img.shields.io/docsrs/socketeer?style=for-the-badge)](https://docs.rs/socketeer)
[![GitHub branch status](https://img.shields.io/github/checks-status/zheylmun/socketeer/main?style=for-the-badge&logo=GitHub)](https://github.com/zheylmun/socketeer/actions)
[![Codecov](https://img.shields.io/codecov/c/github/zheylmun/socketeer?style=for-the-badge&logo=CodeCov)](https://app.codecov.io/gh/zheylmun/socketeer)

`socketeer` is a simple wrapper which creates a tokio-tungstenite websocket connection and exposes a simple async API to send and receive application messages.
It automatically handles the underlying websocket and allows for reconnection, as well as immediate handling of errors in client code.
Currently supports only json encoded binary and text messages.

## Usage

```rust no_run
use socketeer::Socketeer;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SocketMessage {
    message: String,
}

#[tokio::main]
async fn main() {
    // Create a Socketeer instance that connects to a fictional local server.
    let mut socketeer: Socketeer<SocketMessage, SocketMessage> =
        Socketeer::connect("ws://127.0.0.1:80")
            .await
            .unwrap();
    // Send a message to the server
    socketeer
        .send(SocketMessage {
            message: "Hello, world!".to_string(),
        })
        .await
        .unwrap();
    // Wait for the server to respond
    let response = socketeer.next_message().await.unwrap();
    println!("{response:#?}");
    // Shut everything down and close the connection
    socketeer.close_connection().await.unwrap();
}
```
