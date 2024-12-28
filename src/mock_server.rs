use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio_tungstenite::{
    tungstenite::{self, Message},
    MaybeTlsStream,
};

use crate::WebSocketStreamType;

/// Control messages for testing with the echo server.
#[derive(Debug, Serialize, Deserialize)]
pub enum EchoControlMessage {
    Message(String),
    SendPing,
    Close,
}

/// Basic echo server that sends back messages it receives.
/// It can also respond to pings and close the connection.
/// # Errors
/// - If the socket is closed unexpectedly
/// - If the server cannot send a message
/// # Panics
/// - If a received message fails to deserialize
pub async fn echo_server(ws: WebSocketStreamType) -> Result<(), tungstenite::Error> {
    let (mut sink, mut stream) = ws.split();
    while let Some(message) = stream.next().await {
        match message {
            Ok(Message::Text(text)) => {
                let message: EchoControlMessage = serde_json::from_str(&text).unwrap();
                match message {
                    EchoControlMessage::Message(text) => {
                        sink.send(Message::Text(text.into())).await?;
                    }
                    EchoControlMessage::SendPing => {
                        sink.send(Message::Ping(Bytes::new())).await?;
                    }
                    EchoControlMessage::Close => {
                        sink.send(Message::Close(None)).await?;
                        break;
                    }
                }
            }
            Ok(Message::Ping(ping)) => {
                sink.send(Message::Pong(ping)).await.unwrap();
            }
            Ok(Message::Close(_)) => {
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Create a WebSocket server that handles a customizable set of
/// requests and exits.
/// # Panics
/// - If the server cannot bind to a port
/// - If the server cannot accept a connection
/// - If the provided handler returns an error
pub async fn create_mock_server<F, R>(socket_handler: F) -> SocketAddr
where
    F: FnOnce(WebSocketStreamType) -> R + Send + Sync + 'static,
    R: Future<Output = Result<(), tungstenite::Error>> + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (tcp_stream, _socket_addr) = listener.accept().await.unwrap();
        let ws = tokio_tungstenite::accept_async(MaybeTlsStream::Plain(tcp_stream))
            .await
            .unwrap();
        socket_handler(ws).await.unwrap();
    });
    server_address
}
