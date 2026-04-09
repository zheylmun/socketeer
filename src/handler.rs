use futures::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message;

use crate::{Error, WebSocketStreamType};

/// Context available during the WebSocket handshake phase.
///
/// Provides methods to send and receive messages before the main socket loop starts.
/// Use this in [`ConnectionHandler::on_connected`] for authentication handshakes
/// and initial subscriptions.
pub struct HandshakeContext<'a> {
    sink: &'a mut SplitSink<WebSocketStreamType, Message>,
    stream: &'a mut SplitStream<WebSocketStreamType>,
}

impl<'a> HandshakeContext<'a> {
    pub(crate) fn new(
        sink: &'a mut SplitSink<WebSocketStreamType, Message>,
        stream: &'a mut SplitStream<WebSocketStreamType>,
    ) -> Self {
        Self { sink, stream }
    }

    /// Send a text message during the handshake.
    /// # Errors
    /// - If the WebSocket connection fails
    pub async fn send_text(&mut self, text: &str) -> Result<(), Error> {
        self.sink
            .send(Message::Text(text.into()))
            .await
            .map_err(Error::from)
    }

    /// Receive the next text message during the handshake.
    ///
    /// Skips non-text protocol messages (ping/pong). Returns an error if a
    /// binary or close frame is received, or if the connection closes.
    /// # Errors
    /// - If the WebSocket connection is closed
    /// - If a non-text message is received
    pub async fn recv_text(&mut self) -> Result<String, Error> {
        loop {
            let Some(msg) = self.stream.next().await else {
                return Err(Error::WebsocketClosed);
            };
            match msg.map_err(Error::WebsocketError)? {
                Message::Text(text) => return Ok(text.to_string()),
                Message::Ping(_) | Message::Pong(_) => {},
                other => return Err(Error::UnexpectedMessageType(Box::new(other))),
            }
        }
    }

    /// Serialize and send a JSON message during the handshake.
    /// # Errors
    /// - If the message cannot be serialized
    /// - If the WebSocket connection fails
    pub async fn send_json<T: Serialize>(&mut self, msg: &T) -> Result<(), Error> {
        let text = serde_json::to_string(msg)?;
        self.send_text(&text).await
    }

    /// Receive and deserialize a JSON message during the handshake.
    /// # Errors
    /// - If the WebSocket connection is closed
    /// - If the message cannot be deserialized
    pub async fn recv_json<T: for<'de> Deserialize<'de>>(&mut self) -> Result<T, Error> {
        let text = self.recv_text().await?;
        serde_json::from_str(&text).map_err(Error::from)
    }
}

/// Trait for handling WebSocket connection lifecycle events.
///
/// Implement this trait to run custom logic during connection setup (e.g., authentication
/// handshakes, subscriptions) and teardown. The handler is called both on initial
/// connection and on reconnect.
///
/// # Example
///
/// ```rust,no_run
/// use socketeer::{ConnectionHandler, HandshakeContext, Error};
///
/// struct AlpacaAuth {
///     api_key: String,
///     api_secret: String,
/// }
///
/// impl ConnectionHandler for AlpacaAuth {
///     async fn on_connected(&mut self, ctx: &mut HandshakeContext<'_>) -> Result<(), Error> {
///         // Wait for "connected" message
///         let _ = ctx.recv_text().await?;
///         // Send auth
///         ctx.send_text(&format!(
///             r#"{{"action":"auth","key":"{}","secret":"{}"}}"#,
///             self.api_key, self.api_secret
///         )).await?;
///         // Wait for "authenticated" response
///         let _ = ctx.recv_text().await?;
///         Ok(())
///     }
/// }
/// ```
pub trait ConnectionHandler: Send + 'static {
    /// Called after the WebSocket upgrade completes, before the socket loop starts.
    ///
    /// Use this for authentication handshakes, initial subscriptions, or any
    /// setup that must happen before normal message processing begins.
    /// This is also called after a reconnect.
    fn on_connected(
        &mut self,
        ctx: &mut HandshakeContext<'_>,
    ) -> impl std::future::Future<Output = Result<(), Error>> + Send {
        let _ = ctx;
        async { Ok(()) }
    }

    /// Called when the connection is lost, before any reconnect attempt.
    fn on_disconnected(&mut self) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }
}

/// Default no-op connection handler.
///
/// Used when no lifecycle hooks are needed (the simple case).
#[derive(Debug, Clone, Copy)]
pub struct NoopHandler;

impl ConnectionHandler for NoopHandler {}
