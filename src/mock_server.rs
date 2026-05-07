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

/// MessagePack-flavored echo server.
///
/// Decodes each incoming binary frame as an [`EchoControlMessage`] via msgpack
/// and behaves like [`echo_server`] for the embedded control message.
/// # Errors
/// - If the socket is closed unexpectedly
/// - If the server cannot send a message
/// # Panics
/// - If a received message fails to deserialize
#[cfg(feature = "msgpack")]
pub async fn msgpack_echo_server(ws: WebSocketStreamType) -> Result<bool, tungstenite::Error> {
    let (mut sink, mut stream) = ws.split();
    let mut shutting_down = false;
    while let Some(message) = stream.next().await {
        match message {
            Ok(Message::Binary(bytes)) => {
                let control_message: EchoControlMessage = rmp_serde::from_slice(&bytes).unwrap();
                match control_message {
                    EchoControlMessage::Message(_) => {
                        sink.send(Message::Binary(bytes)).await?;
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
            Ok(Message::Close(_)) => {
                if !shutting_down {
                    sink.close().await.unwrap();
                    drop(stream);
                }
                break;
            }
            _ => {}
        }
    }
    Ok(shutting_down)
}

/// Echo server that requires an auth handshake before echoing.
///
/// Expects the client to send `{"action":"auth","token":"test-token"}` as the first message.
/// Responds with `{"status":"authenticated"}` on success or `{"status":"error"}` on failure.
/// After auth, behaves like [`echo_server`].
/// # Errors
/// - If the socket is closed unexpectedly
/// - If the server cannot send a message
/// # Panics
/// - If a received message fails to deserialize
pub async fn auth_echo_server(ws: WebSocketStreamType) -> Result<bool, tungstenite::Error> {
    let (mut sink, mut stream) = ws.split();

    // Wait for auth message
    if let Some(Ok(Message::Text(text))) = stream.next().await {
        #[derive(serde::Deserialize)]
        struct AuthMsg {
            action: String,
            token: String,
        }
        if let Ok(auth) = serde_json::from_str::<AuthMsg>(&text) {
            if auth.action == "auth" && auth.token == "test-token" {
                sink.send(Message::Text(r#"{"status":"authenticated"}"#.into()))
                    .await?;
            } else {
                sink.send(Message::Text(r#"{"status":"error"}"#.into()))
                    .await?;
                return Ok(true);
            }
        }
    }

    // After auth, behave like echo_server
    let ws = sink.reunite(stream).unwrap();
    echo_server(ws).await
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
