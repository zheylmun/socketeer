# socketeer

[![Crates.io Version](https://img.shields.io/crates/v/socketeer?style=for-the-badge)](https://crates.io/crates/socketeer)
[![docs.rs](https://img.shields.io/docsrs/socketeer?style=for-the-badge)](https://docs.rs/socketeer)
[![GitHub branch status](https://img.shields.io/github/checks-status/zheylmun/socketeer/main?style=for-the-badge&logo=GitHub)](https://github.com/zheylmun/socketeer/actions)
[![Codecov](https://img.shields.io/codecov/c/github/zheylmun/socketeer?style=for-the-badge&logo=CodeCov)](https://app.codecov.io/gh/zheylmun/socketeer)

`socketeer` is a simplified async WebSocket client built on tokio-tungstenite. It manages the underlying connection and exposes a clean API for sending and receiving messages, with support for:

- Automatic connection management with configurable keepalive
- Type-safe JSON message serialization/deserialization via serde
- Raw message support for non-JSON protocols
- Custom HTTP headers on the WebSocket upgrade request
- Connection lifecycle hooks for auth handshakes and subscriptions
- Transparent handling of WebSocket protocol messages (ping/pong/close)
- Reconnection with automatic re-authentication

## Usage

### Simple JSON messages

```rust no_run
use socketeer::Socketeer;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SocketMessage {
    message: String,
}

#[tokio::main]
async fn main() {
    let mut socketeer: Socketeer<SocketMessage, SocketMessage> =
        Socketeer::connect("ws://127.0.0.1:80")
            .await
            .unwrap();
    socketeer
        .send(SocketMessage {
            message: "Hello, world!".to_string(),
        })
        .await
        .unwrap();
    let response = socketeer.next_message().await.unwrap();
    println!("{response:#?}");
    socketeer.close_connection().await.unwrap();
}
```

### Custom headers and connection options

```rust no_run
use socketeer::{Socketeer, ConnectOptions};
use std::time::Duration;

# #[derive(Debug, serde::Serialize, serde::Deserialize)]
# struct Msg { text: String }
# #[tokio::main]
# async fn main() {
let mut options = ConnectOptions::default();
options.extra_headers.insert("Authorization", "Bearer my-token".parse().unwrap());
options.keepalive_interval = Some(Duration::from_secs(10));

let socketeer: Socketeer<Msg, Msg> =
    Socketeer::connect_with("wss://api.example.com/ws", options)
        .await
        .unwrap();
# }
```

### Connection lifecycle hooks

```rust no_run
use socketeer::{Socketeer, ConnectOptions, ConnectionHandler, HandshakeContext, Error};

struct MyAuthHandler { api_key: String }

impl ConnectionHandler for MyAuthHandler {
    async fn on_connected(&mut self, ctx: &mut HandshakeContext<'_>) -> Result<(), Error> {
        ctx.send_text(&format!(r#"{{"action":"auth","key":"{}"}}"#, self.api_key)).await?;
        let _response = ctx.recv_text().await?;
        Ok(())
    }
}

# #[derive(Debug, serde::Serialize, serde::Deserialize)]
# struct Msg { text: String }
# #[tokio::main]
# async fn main() {
let handler = MyAuthHandler { api_key: "secret".into() };
let socketeer: Socketeer<Msg, Msg, MyAuthHandler> =
    Socketeer::connect_with_handler(
        "wss://stream.example.com",
        ConnectOptions::default(),
        handler,
    )
    .await
    .unwrap();
// Handler's on_connected runs again automatically on reconnect
# }
```
