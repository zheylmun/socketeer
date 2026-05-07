use bytes::Bytes;
use futures::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use tokio_tungstenite::tungstenite::Message;

use crate::{Codec, Error, WebSocketStreamType};

/// Context available during the WebSocket handshake phase.
///
/// Provides methods to send and receive messages before the main socket loop starts.
/// Use this in [`ConnectionHandler::on_connected`] for authentication handshakes
/// and initial subscriptions.
///
/// The context is generic over the connection's [`Codec`], so the typed
/// [`Self::send`] and [`Self::recv`] methods speak the same encoding the rest of
/// the connection will use. Raw text/binary helpers are provided for protocols
/// (e.g. IBKR's `api={session}` step) that don't fit the codec's `Tx` shape.
pub struct HandshakeContext<'a, C: Codec> {
    sink: &'a mut SplitSink<WebSocketStreamType, Message>,
    stream: &'a mut SplitStream<WebSocketStreamType>,
    codec: &'a C,
}

impl<'a, C: Codec> HandshakeContext<'a, C> {
    pub(crate) fn new(
        sink: &'a mut SplitSink<WebSocketStreamType, Message>,
        stream: &'a mut SplitStream<WebSocketStreamType>,
        codec: &'a C,
    ) -> Self {
        Self {
            sink,
            stream,
            codec,
        }
    }

    /// Encode and send a value using the connection's [`Codec`].
    /// # Errors
    /// - If the codec fails to encode the value
    /// - If the WebSocket connection fails
    pub async fn send(&mut self, value: &C::Tx) -> Result<(), Error> {
        let message = self.codec.encode(value)?;
        self.sink.send(message).await.map_err(Error::from)
    }

    /// Receive and decode the next message using the connection's [`Codec`].
    ///
    /// Skips ping/pong protocol frames. Returns an error on close or codec failure.
    /// # Errors
    /// - If the WebSocket connection is closed
    /// - If the codec fails to decode the frame
    pub async fn recv(&mut self) -> Result<C::Rx, Error> {
        let frame = self.recv_raw().await?;
        self.codec.decode(&frame)
    }

    /// Send a raw text frame during the handshake.
    ///
    /// Useful for protocols whose handshake messages don't fit the codec's `Tx`
    /// type (e.g. IBKR's literal `api={session_id}` auth string).
    /// # Errors
    /// - If the WebSocket connection fails
    pub async fn send_text(&mut self, text: &str) -> Result<(), Error> {
        self.sink
            .send(Message::Text(text.into()))
            .await
            .map_err(Error::from)
    }

    /// Send a raw binary frame during the handshake.
    /// # Errors
    /// - If the WebSocket connection fails
    pub async fn send_binary(&mut self, bytes: impl Into<Bytes>) -> Result<(), Error> {
        self.sink
            .send(Message::Binary(bytes.into()))
            .await
            .map_err(Error::from)
    }

    /// Receive the next text message during the handshake.
    ///
    /// Skips ping/pong protocol messages. Returns an error if a binary or close
    /// frame is received, or if the connection closes.
    /// # Errors
    /// - If the WebSocket connection is closed
    /// - If a non-text message is received
    pub async fn recv_text(&mut self) -> Result<String, Error> {
        match self.recv_raw().await? {
            Message::Text(text) => Ok(text.to_string()),
            other => Err(Error::UnexpectedMessageType(Box::new(other))),
        }
    }

    /// Receive the next data frame during the handshake without decoding.
    ///
    /// Skips ping/pong protocol messages, returning the next `Text`, `Binary`,
    /// or `Close` frame as-is.
    /// # Errors
    /// - If the WebSocket connection is closed
    pub async fn recv_raw(&mut self) -> Result<Message, Error> {
        loop {
            let Some(msg) = self.stream.next().await else {
                return Err(Error::WebsocketClosed);
            };
            match msg.map_err(Error::WebsocketError)? {
                Message::Ping(_) | Message::Pong(_) => {}
                other => return Ok(other),
            }
        }
    }
}

/// Trait for handling WebSocket connection lifecycle events.
///
/// Implement this trait to run custom logic during connection setup (e.g., authentication
/// handshakes, subscriptions) and teardown. The handler is called both on initial
/// connection and on reconnect.
///
/// The trait is generic over the connection's [`Codec`] so that
/// [`Self::on_connected`] sees a [`HandshakeContext`] specialized to that codec.
/// A handler that doesn't care about the codec can implement
/// `ConnectionHandler<C>` for all `C: Codec` (see [`NoopHandler`]).
///
/// # Example
///
/// ```rust,no_run
/// use socketeer::{Codec, ConnectionHandler, HandshakeContext, Error};
///
/// struct AlpacaAuth {
///     api_key: String,
///     api_secret: String,
/// }
///
/// impl<C: Codec> ConnectionHandler<C> for AlpacaAuth {
///     async fn on_connected(&mut self, ctx: &mut HandshakeContext<'_, C>) -> Result<(), Error> {
///         // Wait for the welcome frame
///         let _ = ctx.recv_raw().await?;
///         // Send auth as plain text — works regardless of the negotiated codec
///         ctx.send_text(&format!(
///             r#"{{"action":"auth","key":"{}","secret":"{}"}}"#,
///             self.api_key, self.api_secret
///         )).await?;
///         let _ = ctx.recv_raw().await?;
///         Ok(())
///     }
/// }
/// ```
pub trait ConnectionHandler<C: Codec>: Send + 'static {
    /// Called after the WebSocket upgrade completes, before the socket loop starts.
    ///
    /// Use this for authentication handshakes, initial subscriptions, or any
    /// setup that must happen before normal message processing begins.
    /// This is also called after a reconnect.
    fn on_connected(
        &mut self,
        ctx: &mut HandshakeContext<'_, C>,
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
/// Used when no lifecycle hooks are needed (the simple case). Implements
/// [`ConnectionHandler<C>`] for every codec.
#[derive(Debug, Clone, Copy)]
pub struct NoopHandler;

impl<C: Codec> ConnectionHandler<C> for NoopHandler {}
