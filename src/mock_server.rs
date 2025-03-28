use crate::WebSocketStreamType;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::{fmt::Debug, future::Future};
use tokio::net::TcpListener;
use tokio_tungstenite::{
    MaybeTlsStream,
    tungstenite::{
        self, Message,
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};
#[cfg(feature = "tracing")]
use tracing::debug;

/// Control messages for testing with the echo server.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, PartialOrd)]
pub enum EchoControlMessage {
    /// Send a message which the server should echo back
    Message(String),
    /// Request that the server send the client a ping
    SendPing,
    /// Request that the server close the connection
    Close,
}

/// Basic echo server that sends back messages it receives.
/// It will also respond to pings and close the connection upon request.
/// # Errors
/// - If the socket is closed unexpectedly
/// - If the server cannot send a message
/// # Panics
/// - If a received message fails to deserialize
pub async fn echo_server(ws: WebSocketStreamType) -> Result<bool, tungstenite::Error> {
    let (mut sink, mut stream) = ws.split();
    let mut shutting_down = false;
    while let Some(message) = stream.next().await {
        match &message {
            Ok(Message::Text(text_bytes)) => {
                let control_message: EchoControlMessage = serde_json::from_str(text_bytes).unwrap();
                match control_message {
                    EchoControlMessage::Message(_) => {
                        sink.send(message.unwrap()).await?;
                    }
                    EchoControlMessage::SendPing => {
                        sink.send(Message::Ping(Bytes::new())).await?;
                    }
                    EchoControlMessage::Close => {
                        sink.send(Message::Close(Some(CloseFrame {
                            code: CloseCode::Normal,
                            reason: "".into(),
                        })))
                        .await?;
                        shutting_down = true;
                    }
                }
            }
            Ok(Message::Ping(_)) => {
                sink.send(Message::Pong(Bytes::new())).await.unwrap();
            }
            Ok(Message::Pong(_)) => {
                #[cfg(feature = "tracing")]
                debug!("Received Pong");
            }
            Ok(Message::Close(_)) => {
                if !shutting_down {
                    sink.close().await.unwrap();
                    drop(stream);
                }
                #[cfg(feature = "tracing")]
                debug!("Server received close request");
                break;
            }
            _ => {}
        }
    }
    Ok(shutting_down)
}

/// Create a WebSocket server that handles a customizable set of
/// requests and exits.
/// If the spawned socket handler returns true, the server will exit.
/// # Panics
/// - If the server cannot bind to a port
/// - If the server cannot accept a connection
/// - If the provided handler returns an error
pub async fn get_mock_address<F, R>(socket_handler: F) -> SocketAddr
where
    F: Fn(WebSocketStreamType) -> R + Send + Sync + 'static,
    R: Future<Output = Result<bool, tungstenite::Error>> + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (tcp_stream, _socket_addr) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(MaybeTlsStream::Plain(tcp_stream))
                .await
                .unwrap();
            if socket_handler(ws).await.unwrap() {
                break;
            }
        }
    });
    server_address
}
