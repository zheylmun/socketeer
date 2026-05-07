#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
mod codec;
mod config;
mod error;
mod handler;
#[cfg(feature = "mocking")]
mod mock_server;

#[cfg(feature = "msgpack")]
pub use codec::MsgPackCodec;
pub use codec::{Codec, JsonCodec, RawCodec};
pub use config::ConnectOptions;
pub use error::Error;
pub use handler::{ConnectionHandler, HandshakeContext, NoopHandler};
#[cfg(all(feature = "mocking", feature = "msgpack"))]
pub use mock_server::msgpack_echo_server;
#[cfg(feature = "mocking")]
pub use mock_server::{EchoControlMessage, auth_echo_server, echo_server, get_mock_address};

use bytes::Bytes;
use futures::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use std::time::Duration;
use tokio::{
    net::TcpStream,
    select,
    sync::{mpsc, oneshot},
    time::sleep,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{self, Message, Utf8Bytes, protocol::CloseFrame},
};

#[cfg(feature = "tracing")]
use tracing::{debug, error, info, instrument, trace};
use url::Url;

#[derive(Debug)]
struct TxChannelPayload {
    message: Message,
    response_tx: oneshot::Sender<Result<(), Error>>,
}

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
/// - `CHANNEL_SIZE`: The size of the internal channels used to communicate between
///   the task managing the WebSocket connection and the client.
pub struct Socketeer<C: Codec, Handler = NoopHandler, const CHANNEL_SIZE: usize = 4>
where
    Handler: ConnectionHandler<C>,
{
    url: Url,
    options: ConnectOptions,
    codec: C,
    handler: Handler,
    receiver: mpsc::Receiver<Message>,
    sender: mpsc::Sender<TxChannelPayload>,
    socket_handle: tokio::task::JoinHandle<Result<(), Error>>,
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

        let socket_handle = tokio::spawn(async move {
            socket_loop_split(
                tx_rx,
                rx_tx,
                sink,
                stream,
                keepalive_interval,
                keepalive_message,
            )
            .await
        });
        Ok(Socketeer {
            url,
            options,
            codec,
            handler,
            receiver: rx_rx,
            sender: tx_tx,
            socket_handle,
        })
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
        let Some(message) = self.receiver.recv().await else {
            return Err(Error::WebsocketClosed);
        };
        #[cfg(feature = "tracing")]
        trace!("Received message: {:?}", message);
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
        self.receiver.recv().await.ok_or(Error::WebsocketClosed)
    }

    /// Send a raw [`Message`] to the WebSocket connection without running the codec.
    ///
    /// Useful for sending control frames or pre-encoded payloads.
    ///
    /// # Errors
    ///
    /// - If the WebSocket connection is closed, or otherwise errored
    pub async fn send_raw(&self, message: Message) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel::<Result<(), Error>>();
        self.sender
            .send(TxChannelPayload {
                message,
                response_tx: tx,
            })
            .await
            .map_err(|_| Error::WebsocketClosed)?;
        match rx.await {
            Ok(result) => result,
            Err(_) => unreachable!("Socket loop always sends response before dropping one-shot"),
        }
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
                error!("Socket Loop already stopped: {}", e);
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
        send_close(&self.sender).await?;
        match self.socket_handle.await {
            Ok(result) => result,
            Err(_) => unreachable!("Socket loop does not panic, and is not cancelled"),
        }
    }
}

pub(crate) type WebSocketStreamType = WebSocketStream<MaybeTlsStream<TcpStream>>;
type SocketSink = SplitSink<WebSocketStreamType, Message>;
type SocketStream = SplitStream<WebSocketStreamType>;

enum LoopState {
    Running,
    Error(Error),
    Closed,
}

/// Send a close frame via the tx channel and wait for confirmation.
async fn send_close(sender: &mpsc::Sender<TxChannelPayload>) -> Result<(), Error> {
    let (tx, rx) = oneshot::channel::<Result<(), Error>>();
    sender
        .send(TxChannelPayload {
            message: Message::Close(Some(CloseFrame {
                code: tungstenite::protocol::frame::coding::CloseCode::Normal,
                reason: Utf8Bytes::from_static("Closing Connection"),
            })),
            response_tx: tx,
        })
        .await
        .map_err(|_| Error::WebsocketClosed)?;
    match rx.await {
        Ok(result) => result,
        Err(_) => unreachable!("Socket loop always sends response before dropping one-shot"),
    }
}

#[cfg_attr(
    feature = "tracing",
    instrument(skip(keepalive_interval, keepalive_message))
)]
async fn socket_loop_split(
    mut receiver: mpsc::Receiver<TxChannelPayload>,
    mut sender: mpsc::Sender<Message>,
    mut sink: SocketSink,
    mut stream: SocketStream,
    keepalive_interval: Option<Duration>,
    keepalive_message: Option<Message>,
) -> Result<(), Error> {
    let mut state = LoopState::Running;
    while matches!(state, LoopState::Running) {
        state = if let Some(interval) = keepalive_interval {
            select! {
                outgoing_message = receiver.recv() => send_socket_message(outgoing_message, &mut sink).await,
                incoming_message = stream.next() => socket_message_received(incoming_message, &mut sender, &mut sink).await,
                () = sleep(interval) => send_keepalive(&mut sink, keepalive_message.as_ref()).await,
            }
        } else {
            select! {
                outgoing_message = receiver.recv() => send_socket_message(outgoing_message, &mut sink).await,
                incoming_message = stream.next() => socket_message_received(incoming_message, &mut sender, &mut sink).await,
            }
        };
    }
    match state {
        LoopState::Error(e) => Err(e),
        LoopState::Closed => Ok(()),
        LoopState::Running => unreachable!("We only exit when closed or errored"),
    }
}

#[cfg_attr(feature = "tracing", instrument)]
async fn send_socket_message(
    message: Option<TxChannelPayload>,
    sink: &mut SocketSink,
) -> LoopState {
    if let Some(message) = message {
        #[cfg(feature = "tracing")]
        debug!("Sending message: {:?}", message);
        let send_result = sink.send(message.message).await.map_err(Error::from);
        let socket_error = send_result.is_err();
        match message.response_tx.send(send_result) {
            Ok(()) => {
                if socket_error {
                    LoopState::Error(Error::WebsocketClosed)
                } else {
                    LoopState::Running
                }
            }
            Err(_) => LoopState::Error(Error::SocketeerDroppedWithoutClosing),
        }
    } else {
        #[cfg(feature = "tracing")]
        error!("Socketeer dropped without closing connection");
        LoopState::Error(Error::SocketeerDroppedWithoutClosing)
    }
}

#[cfg_attr(feature = "tracing", instrument)]
async fn socket_message_received(
    message: Option<Result<Message, tungstenite::Error>>,
    sender: &mut mpsc::Sender<Message>,
    sink: &mut SocketSink,
) -> LoopState {
    const PONG_BYTES: Bytes = Bytes::from_static(b"pong");
    match message {
        Some(Ok(message)) => match message {
            Message::Ping(_) => {
                let send_result = sink
                    .send(Message::Pong(PONG_BYTES))
                    .await
                    .map_err(Error::from);
                match send_result {
                    Ok(()) => LoopState::Running,
                    Err(e) => {
                        #[cfg(feature = "tracing")]
                        error!("Error sending Pong: {:?}", e);
                        LoopState::Error(e)
                    }
                }
            }
            Message::Close(_) => {
                let close_result = sink.close().await;
                match close_result {
                    Ok(()) => LoopState::Closed,
                    Err(e) => {
                        #[cfg(feature = "tracing")]
                        error!("Error sending Close: {:?}", e);
                        LoopState::Error(Error::from(e))
                    }
                }
            }
            Message::Text(_) | Message::Binary(_) => match sender.send(message).await {
                Ok(()) => LoopState::Running,
                Err(_) => LoopState::Error(Error::SocketeerDroppedWithoutClosing),
            },
            _ => LoopState::Running,
        },
        Some(Err(e)) => {
            #[cfg(feature = "tracing")]
            error!("Error receiving message: {:?}", e);
            LoopState::Error(Error::WebsocketError(e))
        }
        None => {
            #[cfg(feature = "tracing")]
            info!("Websocket Closed, closing rx channel");
            LoopState::Error(Error::WebsocketClosed)
        }
    }
}

#[cfg_attr(feature = "tracing", instrument)]
async fn send_keepalive(sink: &mut SocketSink, custom_message: Option<&Message>) -> LoopState {
    let message = if let Some(custom) = custom_message {
        #[cfg(feature = "tracing")]
        info!("Timeout waiting for message, sending custom keepalive");
        custom.clone()
    } else {
        #[cfg(feature = "tracing")]
        info!("Timeout waiting for message, sending Ping");
        Message::Ping(Bytes::new())
    };
    let result = sink.send(message).await.map_err(Error::from);
    match result {
        Ok(()) => LoopState::Running,
        Err(e) => {
            #[cfg(feature = "tracing")]
            error!("Error sending keepalive: {:?}", e);
            LoopState::Error(e)
        }
    }
}

#[cfg(all(test, feature = "mocking"))]
mod tests {
    use super::*;
    use tokio::time::sleep;

    type EchoJson = JsonCodec<EchoControlMessage, EchoControlMessage>;

    #[tokio::test]
    async fn test_server_startup() {
        let _server_address = get_mock_address(echo_server).await;
    }

    #[tokio::test]
    async fn test_connection() {
        let server_address = get_mock_address(echo_server).await;
        let _socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_bad_url() {
        let error: Result<Socketeer<EchoJson>, Error> = Socketeer::connect("Not a URL").await;
        assert!(matches!(error.unwrap_err(), Error::UrlParse { .. }));
    }

    #[tokio::test]
    async fn test_send_receive() {
        let server_address = get_mock_address(echo_server).await;
        let mut socketeer: Socketeer<EchoJson> =
            Socketeer::connect(&format!("ws://{server_address}"))
                .await
                .unwrap();
        let message = EchoControlMessage::Message("Hello".to_string());
        socketeer.send(message.clone()).await.unwrap();
        let received_message = socketeer.next_message().await.unwrap();
        assert_eq!(message, received_message);
    }

    #[tokio::test]
    async fn test_ping_request() {
        let server_address = get_mock_address(echo_server).await;
        let mut socketeer: Socketeer<EchoJson> =
            Socketeer::connect(&format!("ws://{server_address}"))
                .await
                .unwrap();
        let ping_request = EchoControlMessage::SendPing;
        socketeer.send(ping_request).await.unwrap();
        // The server will respond with a ping request, which Socketeer will transparently respond to
        let message = EchoControlMessage::Message("Hello".to_string());
        socketeer.send(message.clone()).await.unwrap();
        let received_message = socketeer.next_message().await.unwrap();
        assert_eq!(received_message, message);
        // We should send a ping in here
        sleep(Duration::from_millis(2200)).await;
        // Ensure everything shuts down so we exercize the ping functionality fully
        socketeer.close_connection().await.unwrap();
    }

    #[tokio::test]
    async fn test_reconnection() {
        let server_address = get_mock_address(echo_server).await;
        let mut socketeer: Socketeer<EchoJson> =
            Socketeer::connect(&format!("ws://{server_address}"))
                .await
                .unwrap();
        let message = EchoControlMessage::Message("Hello".to_string());
        socketeer.send(message.clone()).await.unwrap();
        let received_message = socketeer.next_message().await.unwrap();
        assert_eq!(message, received_message);
        socketeer = socketeer.reconnect().await.unwrap();
        let message = EchoControlMessage::Message("Hello".to_string());
        socketeer.send(message.clone()).await.unwrap();
        let received_message = socketeer.next_message().await.unwrap();
        assert_eq!(message, received_message);
        socketeer.close_connection().await.unwrap();
    }

    #[tokio::test]
    async fn test_closed_socket() {
        let server_address = get_mock_address(echo_server).await;
        let mut socketeer: Socketeer<EchoJson> =
            Socketeer::connect(&format!("ws://{server_address}"))
                .await
                .unwrap();
        let close_request = EchoControlMessage::Close;
        socketeer.send(close_request.clone()).await.unwrap();
        let response = socketeer.next_message().await;
        assert!(matches!(response.unwrap_err(), Error::WebsocketClosed));
        let send_result = socketeer.send(close_request).await;
        assert!(send_result.is_err());
        let error = send_result.unwrap_err();
        println!("Actual Error: {error:#?}");
        assert!(matches!(error, Error::WebsocketClosed));
    }

    #[tokio::test]
    async fn test_close_request() {
        let server_address = get_mock_address(echo_server).await;
        let socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
            .await
            .unwrap();
        socketeer.close_connection().await.unwrap();
    }

    #[tokio::test]
    async fn test_connect_with_default_options() {
        let server_address = get_mock_address(echo_server).await;
        let mut socketeer: Socketeer<EchoJson> =
            Socketeer::connect_with(&format!("ws://{server_address}"), ConnectOptions::default())
                .await
                .unwrap();
        let message = EchoControlMessage::Message("Hello".to_string());
        socketeer.send(message.clone()).await.unwrap();
        let received_message = socketeer.next_message().await.unwrap();
        assert_eq!(message, received_message);
    }

    #[tokio::test]
    async fn test_send_raw_receive_raw() {
        let server_address = get_mock_address(echo_server).await;
        let mut socketeer: Socketeer<RawCodec> =
            Socketeer::connect(&format!("ws://{server_address}"))
                .await
                .unwrap();
        let raw_text = r#"{"Message":"raw hello"}"#;
        socketeer
            .send(Message::Text(raw_text.into()))
            .await
            .unwrap();
        let received = socketeer.next_message().await.unwrap();
        assert_eq!(received, Message::Text(raw_text.into()));
    }

    #[tokio::test]
    async fn test_disabled_keepalive() {
        let server_address = get_mock_address(echo_server).await;
        let options = ConnectOptions {
            keepalive_interval: None,
            ..ConnectOptions::default()
        };
        let mut socketeer: Socketeer<EchoJson> =
            Socketeer::connect_with(&format!("ws://{server_address}"), options)
                .await
                .unwrap();
        let message = EchoControlMessage::Message("Hello".to_string());
        socketeer.send(message.clone()).await.unwrap();
        let received_message = socketeer.next_message().await.unwrap();
        assert_eq!(message, received_message);
    }

    #[tokio::test]
    async fn test_handler_on_connected() {
        use serde::{Deserialize, Serialize};
        use std::sync::Arc;
        use tokio::sync::Mutex;

        #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
        struct AuthResponse {
            status: String,
        }

        struct TestAuthHandler {
            connected_count: Arc<Mutex<u32>>,
        }

        impl<C: Codec> ConnectionHandler<C> for TestAuthHandler {
            async fn on_connected(
                &mut self,
                ctx: &mut HandshakeContext<'_, C>,
            ) -> Result<(), Error> {
                ctx.send_text(r#"{"action":"auth","token":"test-token"}"#)
                    .await?;
                let text = ctx.recv_text().await?;
                let response: AuthResponse = serde_json::from_str(&text).unwrap();
                assert_eq!(response.status, "authenticated");
                let mut count = self.connected_count.lock().await;
                *count += 1;
                Ok(())
            }
        }

        let connected_count = Arc::new(Mutex::new(0u32));
        let handler = TestAuthHandler {
            connected_count: connected_count.clone(),
        };

        let server_address = get_mock_address(auth_echo_server).await;
        let mut socketeer: Socketeer<EchoJson, TestAuthHandler> = Socketeer::connect_with_codec(
            &format!("ws://{server_address}"),
            ConnectOptions::default(),
            JsonCodec::new(),
            handler,
        )
        .await
        .unwrap();

        assert_eq!(*connected_count.lock().await, 1);

        let message = EchoControlMessage::Message("after auth".to_string());
        socketeer.send(message.clone()).await.unwrap();
        let received = socketeer.next_message().await.unwrap();
        assert_eq!(message, received);
    }

    #[tokio::test]
    async fn test_handler_reconnect() {
        use std::sync::Arc;
        use tokio::sync::Mutex;

        struct ReconnectHandler {
            connected_count: Arc<Mutex<u32>>,
            disconnected_count: Arc<Mutex<u32>>,
        }

        impl<C: Codec> ConnectionHandler<C> for ReconnectHandler {
            async fn on_connected(
                &mut self,
                ctx: &mut HandshakeContext<'_, C>,
            ) -> Result<(), Error> {
                ctx.send_text(r#"{"action":"auth","token":"test-token"}"#)
                    .await?;
                let _response = ctx.recv_text().await?;
                let mut count = self.connected_count.lock().await;
                *count += 1;
                Ok(())
            }

            async fn on_disconnected(&mut self) {
                let mut count = self.disconnected_count.lock().await;
                *count += 1;
            }
        }

        let connected_count = Arc::new(Mutex::new(0u32));
        let disconnected_count = Arc::new(Mutex::new(0u32));
        let handler = ReconnectHandler {
            connected_count: connected_count.clone(),
            disconnected_count: disconnected_count.clone(),
        };

        let server_address = get_mock_address(auth_echo_server).await;
        let mut socketeer = Socketeer::<EchoJson, ReconnectHandler>::connect_with_codec(
            &format!("ws://{server_address}"),
            ConnectOptions::default(),
            JsonCodec::new(),
            handler,
        )
        .await
        .unwrap();

        assert_eq!(*connected_count.lock().await, 1);
        assert_eq!(*disconnected_count.lock().await, 0);

        // Send a message to verify connection works
        let message = EchoControlMessage::Message("before reconnect".to_string());
        socketeer.send(message.clone()).await.unwrap();
        let received = socketeer.next_message().await.unwrap();
        assert_eq!(message, received);

        // Reconnect — handler should fire again
        socketeer = socketeer.reconnect().await.unwrap();

        assert_eq!(*connected_count.lock().await, 2);
        assert_eq!(*disconnected_count.lock().await, 1);

        // Verify connection still works after reconnect
        let message = EchoControlMessage::Message("after reconnect".to_string());
        socketeer.send(message.clone()).await.unwrap();
        let received = socketeer.next_message().await.unwrap();
        assert_eq!(message, received);

        socketeer.close_connection().await.unwrap();
    }

    #[cfg(feature = "msgpack")]
    #[tokio::test]
    async fn test_msgpack_send_receive() {
        type EchoMsgPack = MsgPackCodec<EchoControlMessage, EchoControlMessage>;

        let server_address = get_mock_address(msgpack_echo_server).await;
        let mut socketeer: Socketeer<EchoMsgPack> =
            Socketeer::connect(&format!("ws://{server_address}"))
                .await
                .unwrap();
        let message = EchoControlMessage::Message("msgpack hello".to_string());
        socketeer.send(message.clone()).await.unwrap();
        let received = socketeer.next_message().await.unwrap();
        assert_eq!(message, received);
        socketeer.close_connection().await.unwrap();
    }

    #[tokio::test]
    async fn test_handler_uses_codec_driven_send_recv() {
        // Exercises HandshakeContext::send / recv (the codec-driven path).
        // Other handler tests only cover the raw send_text / recv_text helpers.
        struct TypedHandshakeHandler;

        impl ConnectionHandler<EchoJson> for TypedHandshakeHandler {
            async fn on_connected(
                &mut self,
                ctx: &mut HandshakeContext<'_, EchoJson>,
            ) -> Result<(), Error> {
                ctx.send(&EchoControlMessage::Message("handshake".into()))
                    .await?;
                let echoed = ctx.recv().await?;
                assert_eq!(echoed, EchoControlMessage::Message("handshake".into()));
                Ok(())
            }
        }

        let server_address = get_mock_address(echo_server).await;
        let mut socketeer: Socketeer<EchoJson, TypedHandshakeHandler> =
            Socketeer::connect_with_codec(
                &format!("ws://{server_address}"),
                ConnectOptions::default(),
                JsonCodec::new(),
                TypedHandshakeHandler,
            )
            .await
            .unwrap();

        // Confirm normal traffic still flows after the typed handshake.
        let message = EchoControlMessage::Message("after handshake".into());
        socketeer.send(message.clone()).await.unwrap();
        assert_eq!(socketeer.next_message().await.unwrap(), message);
        socketeer.close_connection().await.unwrap();
    }

    #[tokio::test]
    async fn test_extra_headers_used() {
        // Cover ConnectOptions::build_request's loop body that copies
        // `extra_headers` onto the upgrade request.
        let server_address = get_mock_address(echo_server).await;
        let mut headers = tokio_tungstenite::tungstenite::http::HeaderMap::new();
        headers.insert("X-Test-Header", "socketeer".parse().unwrap());
        let options = ConnectOptions {
            extra_headers: headers,
            ..ConnectOptions::default()
        };
        let mut socketeer: Socketeer<EchoJson> =
            Socketeer::connect_with(&format!("ws://{server_address}"), options)
                .await
                .unwrap();
        let message = EchoControlMessage::Message("hi".into());
        socketeer.send(message.clone()).await.unwrap();
        assert_eq!(socketeer.next_message().await.unwrap(), message);
        socketeer.close_connection().await.unwrap();
    }

    #[tokio::test]
    async fn test_auth_handler_bad_token() {
        // Covers auth_echo_server's bad-token branch (sends {"status":"error"}
        // and shuts down). The handler observes the error response, returns
        // Ok, then a subsequent send fails because the server has closed.
        struct BadTokenHandler;

        impl<C: Codec> ConnectionHandler<C> for BadTokenHandler {
            async fn on_connected(
                &mut self,
                ctx: &mut HandshakeContext<'_, C>,
            ) -> Result<(), Error> {
                ctx.send_text(r#"{"action":"auth","token":"WRONG"}"#)
                    .await?;
                let resp = ctx.recv_text().await?;
                assert!(resp.contains("error"));
                Ok(())
            }
        }

        let server_address = get_mock_address(auth_echo_server).await;
        let _socketeer: Socketeer<EchoJson, BadTokenHandler> = Socketeer::connect_with_codec(
            &format!("ws://{server_address}"),
            ConnectOptions::default(),
            JsonCodec::new(),
            BadTokenHandler,
        )
        .await
        .unwrap();
    }

    #[cfg(feature = "msgpack")]
    #[tokio::test]
    async fn test_msgpack_send_ping() {
        // Covers the SendPing arm of msgpack_echo_server.
        type EchoMsgPack = MsgPackCodec<EchoControlMessage, EchoControlMessage>;

        let server_address = get_mock_address(msgpack_echo_server).await;
        let mut socketeer: Socketeer<EchoMsgPack> =
            Socketeer::connect(&format!("ws://{server_address}"))
                .await
                .unwrap();
        socketeer.send(EchoControlMessage::SendPing).await.unwrap();
        // Server replies with a Ping; Socketeer auto-Pongs. Round-trip a real
        // message to confirm the connection is still alive.
        let message = EchoControlMessage::Message("after ping".into());
        socketeer.send(message.clone()).await.unwrap();
        assert_eq!(socketeer.next_message().await.unwrap(), message);
        socketeer.close_connection().await.unwrap();
    }

    #[cfg(feature = "msgpack")]
    #[tokio::test]
    async fn test_msgpack_close_request() {
        // Covers the Close arm of msgpack_echo_server.
        type EchoMsgPack = MsgPackCodec<EchoControlMessage, EchoControlMessage>;

        let server_address = get_mock_address(msgpack_echo_server).await;
        let mut socketeer: Socketeer<EchoMsgPack> =
            Socketeer::connect(&format!("ws://{server_address}"))
                .await
                .unwrap();
        socketeer.send(EchoControlMessage::Close).await.unwrap();
        let result = socketeer.next_message().await;
        assert!(matches!(result.unwrap_err(), Error::WebsocketClosed));
    }

    #[tokio::test]
    async fn test_socketeer_debug_format() {
        let server_address = get_mock_address(echo_server).await;
        let socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
            .await
            .unwrap();
        let formatted = format!("{socketeer:?}");
        assert!(formatted.starts_with("Socketeer"));
        assert!(formatted.contains("url"));
    }

    #[tokio::test]
    async fn test_next_raw_message() {
        // Cover Socketeer::next_raw_message (the raw-receive escape hatch).
        let server_address = get_mock_address(echo_server).await;
        let mut socketeer: Socketeer<EchoJson> =
            Socketeer::connect(&format!("ws://{server_address}"))
                .await
                .unwrap();
        let message = EchoControlMessage::Message("raw recv".into());
        socketeer.send(message).await.unwrap();
        let frame = socketeer.next_raw_message().await.unwrap();
        assert!(matches!(frame, Message::Text(_)));
        socketeer.close_connection().await.unwrap();
    }

    #[cfg(feature = "msgpack")]
    #[tokio::test]
    async fn test_handshake_send_binary_recv_raw() {
        // Cover HandshakeContext::send_binary by sending a pre-encoded
        // msgpack frame from on_connected and reading the binary echo back
        // via recv_raw.
        struct BinaryHandshake;

        type EchoMsgPack = MsgPackCodec<EchoControlMessage, EchoControlMessage>;

        impl ConnectionHandler<EchoMsgPack> for BinaryHandshake {
            async fn on_connected(
                &mut self,
                ctx: &mut HandshakeContext<'_, EchoMsgPack>,
            ) -> Result<(), Error> {
                let payload =
                    rmp_serde::to_vec_named(&EchoControlMessage::Message("binary".into())).unwrap();
                ctx.send_binary(payload).await?;
                let echo = ctx.recv_raw().await?;
                assert!(matches!(echo, Message::Binary(_)));
                Ok(())
            }
        }

        let server_address = get_mock_address(msgpack_echo_server).await;
        let socketeer: Socketeer<EchoMsgPack, BinaryHandshake> = Socketeer::connect_with_codec(
            &format!("ws://{server_address}"),
            ConnectOptions::default(),
            MsgPackCodec::new(),
            BinaryHandshake,
        )
        .await
        .unwrap();
        socketeer.close_connection().await.unwrap();
    }

    #[cfg(feature = "msgpack")]
    #[tokio::test]
    async fn test_handshake_recv_text_rejects_binary() {
        // Cover the non-Text branch of HandshakeContext::recv_text by pointing
        // it at a server that only speaks binary frames.
        struct ExpectsTextOnBinary;

        type EchoMsgPack = MsgPackCodec<EchoControlMessage, EchoControlMessage>;

        impl ConnectionHandler<EchoMsgPack> for ExpectsTextOnBinary {
            async fn on_connected(
                &mut self,
                ctx: &mut HandshakeContext<'_, EchoMsgPack>,
            ) -> Result<(), Error> {
                let payload =
                    rmp_serde::to_vec_named(&EchoControlMessage::Message("hi".into())).unwrap();
                ctx.send_binary(payload).await?;
                // recv_text must reject the echoed Binary frame.
                let err = ctx.recv_text().await.unwrap_err();
                assert!(matches!(err, Error::UnexpectedMessageType(_)));
                Ok(())
            }
        }

        let server_address = get_mock_address(msgpack_echo_server).await;
        let socketeer: Socketeer<EchoMsgPack, ExpectsTextOnBinary> = Socketeer::connect_with_codec(
            &format!("ws://{server_address}"),
            ConnectOptions::default(),
            MsgPackCodec::new(),
            ExpectsTextOnBinary,
        )
        .await
        .unwrap();
        socketeer.close_connection().await.unwrap();
    }

    #[tokio::test]
    async fn test_binary_custom_keepalive() {
        // The widening of custom_keepalive_message from Option<String> to
        // Option<Message> is otherwise unexercised. echo_server silently
        // ignores Binary frames, so the receive queue stays clean and we can
        // verify the connection survives a binary keepalive cycle.
        let server_address = get_mock_address(echo_server).await;
        let options = ConnectOptions {
            keepalive_interval: Some(Duration::from_millis(100)),
            custom_keepalive_message: Some(Message::Binary(Bytes::from_static(b"keepalive"))),
            ..ConnectOptions::default()
        };
        let mut socketeer: Socketeer<EchoJson> =
            Socketeer::connect_with(&format!("ws://{server_address}"), options)
                .await
                .unwrap();

        // Wait long enough for at least a couple of keepalive ticks to fire.
        sleep(Duration::from_millis(350)).await;

        let message = EchoControlMessage::Message("post-keepalive".into());
        socketeer.send(message.clone()).await.unwrap();
        assert_eq!(socketeer.next_message().await.unwrap(), message);
        socketeer.close_connection().await.unwrap();
    }
}
