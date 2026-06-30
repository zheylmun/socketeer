#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
mod codec;
mod config;
mod error;
mod handler;
#[cfg(feature = "mocking")]
mod mock_server;
mod socket_loop;
mod split;

pub use bytes::Bytes;
#[cfg(feature = "msgpack")]
pub use codec::MsgPackCodec;
pub use codec::{Codec, JsonCodec, RawCodec};
pub use config::{ConnectOptions, ConnectOptionsBuilder};
pub use error::Error;
pub use handler::{ConnectionHandler, HandshakeContext, NoopHandler};
#[cfg(all(feature = "mocking", feature = "msgpack"))]
pub use mock_server::msgpack_echo_server;
#[cfg(feature = "mocking")]
pub use mock_server::{EchoControlMessage, auth_echo_server, echo_server, get_mock_address};
pub use split::{ReuniteError, SocketeerRx, SocketeerTx};
pub use tokio_tungstenite::tungstenite::{self, Message, http};

/// The concrete `WebSocketStream` type the mock-server handlers operate on.
/// Re-exported so downstream code can write custom servers for
/// [`get_mock_address`]. Primarily useful with the `mocking` feature, which
/// gates `get_mock_address` and the built-in test servers.
pub use socket_loop::WebSocketStreamType;
use socket_loop::{
    TerminalError, TxChannelPayload, poll_recv_raw, recv_raw, send_close, send_confirmed,
    socket_loop_split,
};

use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use futures::{Stream, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;

#[cfg(feature = "tracing")]
use tracing::{debug, info, instrument};
use url::Url;

/// A WebSocket client that manages the connection to a WebSocket server.
/// The client can send and receive messages, and will transparently handle protocol messages.
///
/// # Type Parameters
///
/// - `C`: A [`Codec`] that defines the connection's `Tx` and `Rx` types and how
///   they map to WebSocket frames. Use [`JsonCodec`] for the common case,
///   [`MsgPackCodec`] (behind the `msgpack` feature) for `MessagePack`, or
///   [`RawCodec`] for direct [`Message`] access.
/// - `Handler`: A [`ConnectionHandler`] for lifecycle hooks (auth, subscriptions).
///   Defaults to [`NoopHandler`] for the simple case.
/// - `CHANNEL_SIZE`: Capacity of the internal mpsc channels between the
///   connection task and the handle. Defaults to 256. A larger buffer absorbs
///   transient consumer slowdowns so backpressure (which holds one frame and
///   relies on keepalives to stay alive) engages only under sustained overload;
///   tune it for your feed's burstiness.
pub struct Socketeer<C: Codec, Handler = NoopHandler, const CHANNEL_SIZE: usize = 256>
where
    Handler: ConnectionHandler<C>,
{
    url: Url,
    options: ConnectOptions,
    codec: C,
    handler: Handler,
    receiver: mpsc::Receiver<Message>,
    sender: mpsc::Sender<TxChannelPayload>,
    socket_handle: tokio::task::JoinHandle<()>,
    terminal_error: TerminalError,
}

impl<C: Codec, Handler, const CHANNEL_SIZE: usize> std::fmt::Debug
    for Socketeer<C, Handler, CHANNEL_SIZE>
where
    Handler: ConnectionHandler<C>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Socketeer")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl<C: Codec + Unpin, Handler, const CHANNEL_SIZE: usize> Stream
    for Socketeer<C, Handler, CHANNEL_SIZE>
where
    Handler: ConnectionHandler<C> + Unpin,
{
    type Item = Result<C::Rx, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        poll_recv_raw(&mut this.receiver, &this.terminal_error, cx)
            .map(|frame| frame.map(|result| result.and_then(|message| this.codec.decode(&message))))
    }
}

impl<C, const CHANNEL_SIZE: usize> Socketeer<C, NoopHandler, CHANNEL_SIZE>
where
    C: Codec + Default,
{
    /// Create a `Socketeer` connected to the provided URL with default options.
    /// Once connected, Socketeer manages the underlying WebSocket connection, transparently handling protocol messages.
    /// # Errors
    /// - If the URL cannot be parsed
    /// - If the WebSocket connection to the requested URL fails
    #[cfg_attr(feature = "tracing", instrument)]
    pub async fn connect(url: &str) -> Result<Self, Error> {
        Self::connect_with(url, ConnectOptions::default()).await
    }

    /// Create a `Socketeer` connected to the provided URL with custom connection options.
    /// # Errors
    /// - If the URL cannot be parsed
    /// - If the WebSocket connection to the requested URL fails
    #[cfg_attr(feature = "tracing", instrument(skip(options)))]
    pub async fn connect_with(url: &str, options: ConnectOptions) -> Result<Self, Error> {
        Socketeer::connect_with_codec(url, options, C::default(), NoopHandler).await
    }
}

impl<C, Handler, const CHANNEL_SIZE: usize> Socketeer<C, Handler, CHANNEL_SIZE>
where
    C: Codec,
    Handler: ConnectionHandler<C>,
{
    /// Create a `Socketeer` with an explicit codec and [`ConnectionHandler`].
    ///
    /// The handler's [`ConnectionHandler::on_connected`] method is called after the
    /// WebSocket upgrade completes, before the socket loop starts. This is where
    /// you should perform authentication handshakes and initial subscriptions.
    /// # Errors
    /// - If the URL cannot be parsed
    /// - If the WebSocket connection to the requested URL fails
    /// - If the handler's `on_connected` returns an error
    #[cfg_attr(feature = "tracing", instrument(skip(options, codec, handler)))]
    pub async fn connect_with_codec(
        url: &str,
        options: ConnectOptions,
        codec: C,
        mut handler: Handler,
    ) -> Result<Self, Error> {
        let url = Url::parse(url).map_err(|source| Error::UrlParse {
            url: url.to_string(),
            source,
        })?;

        let request = options.build_request(&url)?;
        #[allow(unused_variables)]
        let (socket, response) = connect_async(request).await?;
        #[cfg(feature = "tracing")]
        debug!("Connection Successful, connection info: \n{:#?}", response);

        let (mut sink, mut stream) = socket.split();
        {
            let mut ctx = HandshakeContext::new(&mut sink, &mut stream, &codec);
            handler.on_connected(&mut ctx).await?;
        }

        let keepalive_interval = options.keepalive_interval;
        let keepalive_message = options.custom_keepalive_message.clone();

        let (tx_tx, tx_rx) = mpsc::channel::<TxChannelPayload>(CHANNEL_SIZE);
        let (rx_tx, rx_rx) = mpsc::channel::<Message>(CHANNEL_SIZE);

        let terminal_error: TerminalError = Arc::new(Mutex::new(None));
        let loop_terminal_error = Arc::clone(&terminal_error);
        let socket_handle = tokio::spawn(async move {
            socket_loop_split(
                tx_rx,
                rx_tx,
                sink,
                stream,
                keepalive_interval,
                keepalive_message,
                loop_terminal_error,
            )
            .await;
        });
        Ok(Socketeer {
            url,
            options,
            codec,
            handler,
            receiver: rx_rx,
            sender: tx_tx,
            socket_handle,
            terminal_error,
        })
    }

    /// Reassemble a `Socketeer` from its parts. Used by
    /// [`SocketeerRx::reunite`](crate::SocketeerRx::reunite).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_parts(
        url: Url,
        options: ConnectOptions,
        codec: C,
        handler: Handler,
        receiver: mpsc::Receiver<Message>,
        sender: mpsc::Sender<TxChannelPayload>,
        socket_handle: tokio::task::JoinHandle<()>,
        terminal_error: TerminalError,
    ) -> Self {
        Self {
            url,
            options,
            codec,
            handler,
            receiver,
            sender,
            socket_handle,
            terminal_error,
        }
    }

    /// Wait for the next message from the WebSocket connection, decoded by the
    /// connection's [`Codec`].
    ///
    /// # Errors
    ///
    /// - If the WebSocket connection is closed or otherwise errored
    /// - If the codec fails to decode the frame
    #[cfg_attr(feature = "tracing", instrument(skip(self)))]
    pub async fn next_message(&mut self) -> Result<C::Rx, Error> {
        let message = recv_raw(&mut self.receiver, &self.terminal_error).await?;
        self.codec.decode(&message)
    }

    /// Encode and send a message via the connection's [`Codec`].
    /// This function will wait for the message to be sent before returning.
    ///
    /// # Errors
    ///
    /// - If the codec fails to encode the value
    /// - If the WebSocket connection is closed, or otherwise errored
    #[cfg_attr(feature = "tracing", instrument(skip(self, message)))]
    pub async fn send(&self, message: C::Tx) -> Result<(), Error> {
        let encoded = self.codec.encode(&message)?;
        self.send_raw(encoded).await
    }

    /// Receive the next raw [`Message`] from the WebSocket connection without
    /// running the codec.
    ///
    /// Useful when you need to inspect the underlying frame type or handle a
    /// message that the codec would reject.
    ///
    /// # Errors
    ///
    /// - If the WebSocket connection is closed or otherwise errored
    pub async fn next_raw_message(&mut self) -> Result<Message, Error> {
        recv_raw(&mut self.receiver, &self.terminal_error).await
    }

    /// Send a raw [`Message`] to the WebSocket connection without running the codec.
    ///
    /// Useful for sending control frames or pre-encoded payloads.
    ///
    /// # Errors
    ///
    /// - If the WebSocket connection is closed, or otherwise errored
    pub async fn send_raw(&self, message: Message) -> Result<(), Error> {
        send_confirmed(&self.sender, &self.terminal_error, message).await
    }

    /// Consume self, closing down any remaining send/receive, and return a new Socketeer instance if successful.
    /// This function attempts to close the connection gracefully before returning,
    /// but will not return an error if the connection is already closed,
    /// as its intended use is to re-establish a failed connection.
    ///
    /// The handler's [`ConnectionHandler::on_disconnected`] is called before closing,
    /// and [`ConnectionHandler::on_connected`] is called after reconnecting.
    /// # Errors
    /// - If a new connection cannot be established
    /// - If the handler's `on_connected` returns an error
    pub async fn reconnect(self) -> Result<Self, Error> {
        let url = self.url.as_str().to_owned();
        let options = self.options.clone();
        let codec = self.codec;
        let mut handler = self.handler;
        #[cfg(feature = "tracing")]
        info!("Reconnecting");
        handler.on_disconnected().await;
        // Attempt graceful close, but don't fail if already closed
        match send_close(&self.sender).await {
            Ok(()) => (),
            #[allow(unused_variables)]
            Err(e) => {
                #[cfg(feature = "tracing")]
                debug!("Socket loop already stopped during reconnect: {}", e);
            }
        }
        Self::connect_with_codec(&url, options, codec, handler).await
    }

    /// Close the WebSocket connection gracefully.
    /// This function will wait for the connection to close before returning.
    /// # Errors
    /// - If the WebSocket connection is already closed
    /// - If the WebSocket connection cannot be closed
    #[cfg_attr(feature = "tracing", instrument(skip(self)))]
    pub async fn close_connection(self) -> Result<(), Error> {
        #[cfg(feature = "tracing")]
        debug!("Closing Connection");
        let close_sent = send_close(&self.sender).await;
        // Wait for the loop to finish so any terminal error has been recorded.
        if self.socket_handle.await.is_err() {
            unreachable!("Socket loop does not panic, and is not cancelled");
        }
        // Prefer the loop's recorded cause (it errored during or before the
        // close); otherwise propagate whether the close frame was sent.
        // Access the field directly: `self` is partially moved by the await
        // above, so a `&self` helper can't be called here.
        let terminal = self
            .terminal_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match terminal {
            Some(error) => Err(error),
            None => close_sent,
        }
    }
}

impl<C, Handler, const CHANNEL_SIZE: usize> Socketeer<C, Handler, CHANNEL_SIZE>
where
    C: Codec + Clone,
    Handler: ConnectionHandler<C>,
{
    /// Split into an owned, cloneable send half and an owned receive half.
    ///
    /// A pure move — no new tasks are spawned and the existing channels are
    /// reused. The codec is cloned into both halves (the send half encodes,
    /// the receive half decodes). Recombine with
    /// [`SocketeerRx::reunite`](crate::SocketeerRx::reunite).
    #[must_use]
    pub fn split(self) -> (SocketeerTx<C>, SocketeerRx<C, Handler, CHANNEL_SIZE>) {
        let tx = SocketeerTx {
            sender: self.sender,
            codec: self.codec.clone(),
            terminal_error: Arc::clone(&self.terminal_error),
        };
        let rx = SocketeerRx {
            receiver: self.receiver,
            codec: self.codec,
            terminal_error: self.terminal_error,
            socket_handle: self.socket_handle,
            url: self.url,
            options: self.options,
            handler: self.handler,
        };
        (tx, rx)
    }
}
