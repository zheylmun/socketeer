#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
mod config;
mod error;
mod handler;
#[cfg(feature = "mocking")]
mod mock_server;

pub use config::ConnectOptions;
pub use error::Error;
pub use handler::{ConnectionHandler, HandshakeContext, NoopHandler};
#[cfg(feature = "mocking")]
pub use mock_server::{EchoControlMessage, echo_server, get_mock_address};

use bytes::Bytes;
use futures::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use serde::{Deserialize, Serialize};
use std::{fmt::Debug, time::Duration};
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
/// - `RxMessage`: The type of message that the client will receive from the server.
/// - `TxMessage`: The type of message that the client will send to the server.
/// - `Handler`: A [`ConnectionHandler`] for lifecycle hooks (auth, subscriptions).
///   Defaults to [`NoopHandler`] for the simple case.
/// - `CHANNEL_SIZE`: The size of the internal channels used to communicate between
///   the task managing the WebSocket connection and the client.
pub struct Socketeer<
    RxMessage,
    TxMessage,
    Handler = NoopHandler,
    const CHANNEL_SIZE: usize = 4,
> {
    url: Url,
    options: ConnectOptions,
    handler: Handler,
    receiver: mpsc::Receiver<Message>,
    sender: mpsc::Sender<TxChannelPayload>,
    socket_handle: tokio::task::JoinHandle<Result<(), Error>>,
    _rx_message: std::marker::PhantomData<RxMessage>,
    _tx_message: std::marker::PhantomData<TxMessage>,
}

impl<RxMessage, TxMessage, Handler, const CHANNEL_SIZE: usize> std::fmt::Debug
    for Socketeer<RxMessage, TxMessage, Handler, CHANNEL_SIZE>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Socketeer")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl<
    RxMessage: for<'a> Deserialize<'a> + Debug,
    TxMessage: Serialize + Debug,
    const CHANNEL_SIZE: usize,
> Socketeer<RxMessage, TxMessage, NoopHandler, CHANNEL_SIZE>
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
        Socketeer::connect_with_handler(url, options, NoopHandler).await
    }
}

impl<
    RxMessage: for<'a> Deserialize<'a> + Debug,
    TxMessage: Serialize + Debug,
    Handler: ConnectionHandler,
    const CHANNEL_SIZE: usize,
> Socketeer<RxMessage, TxMessage, Handler, CHANNEL_SIZE>
{
    /// Create a `Socketeer` with a custom [`ConnectionHandler`] for lifecycle hooks.
    ///
    /// The handler's [`ConnectionHandler::on_connected`] method is called after the
    /// WebSocket upgrade completes, before the socket loop starts. This is where
    /// you should perform authentication handshakes and initial subscriptions.
    /// # Errors
    /// - If the URL cannot be parsed
    /// - If the WebSocket connection to the requested URL fails
    /// - If the handler's `on_connected` returns an error
    #[cfg_attr(feature = "tracing", instrument(skip(options, handler)))]
    pub async fn connect_with_handler(
        url: &str,
        options: ConnectOptions,
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
            let mut ctx = HandshakeContext::new(&mut sink, &mut stream);
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
            handler,
            receiver: rx_rx,
            sender: tx_tx,
            socket_handle,
            _rx_message: std::marker::PhantomData,
            _tx_message: std::marker::PhantomData,
        })
    }

    /// Wait for the next parsed message from the WebSocket connection.
    ///
    /// # Errors
    ///
    /// - If the WebSocket connection is closed or otherwise errored
    /// - If the message cannot be deserialized
    #[cfg_attr(feature = "tracing", instrument(skip(self)))]
    pub async fn next_message(&mut self) -> Result<RxMessage, Error> {
        let Some(message) = self.receiver.recv().await else {
            return Err(Error::WebsocketClosed);
        };
        match message {
            Message::Text(text) => {
                #[cfg(feature = "tracing")]
                trace!("Received text message: {:?}", text);
                let message = serde_json::from_str(&text)?;
                Ok(message)
            }
            Message::Binary(message) => {
                #[cfg(feature = "tracing")]
                trace!("Received binary message: {:?}", message);
                let message = serde_json::from_slice(&message)?;
                Ok(message)
            }
            _ => Err(Error::UnexpectedMessageType(Box::new(message))),
        }
    }

    /// Send a message to the WebSocket connection.
    /// This function will wait for the message to be sent before returning.
    ///
    /// # Errors
    ///
    /// - If the message cannot be serialized
    /// - If the WebSocket connection is closed, or otherwise errored
    #[cfg_attr(feature = "tracing", instrument(skip(self)))]
    pub async fn send(&self, message: TxMessage) -> Result<(), Error> {
        #[cfg(feature = "tracing")]
        trace!("Sending message: {:?}", message);

        let (tx, rx) = oneshot::channel::<Result<(), Error>>();
        let message = serde_json::to_string(&message)?;

        self.sender
            .send(TxChannelPayload {
                message: Message::Text(message.into()),
                response_tx: tx,
            })
            .await
            .map_err(|_| Error::WebsocketClosed)?;
        // We'll ensure that we always respond before dropping the tx channel
        match rx.await {
            Ok(result) => result,
            Err(_) => unreachable!("Socket loop always sends response before dropping one-shot"),
        }
    }

    /// Receive the next raw [`Message`] from the WebSocket connection without deserialization.
    ///
    /// This is useful for protocols that don't use JSON or need to inspect the raw message.
    ///
    /// # Errors
    ///
    /// - If the WebSocket connection is closed or otherwise errored
    pub async fn next_raw_message(&mut self) -> Result<Message, Error> {
        self.receiver.recv().await.ok_or(Error::WebsocketClosed)
    }

    /// Send a raw [`Message`] to the WebSocket connection without serialization.
    ///
    /// This is useful for sending non-JSON messages (e.g., plain text keepalives)
    /// or binary data that is already encoded.
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
        Self::connect_with_handler(&url, options, handler).await
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

#[cfg_attr(feature = "tracing", instrument(skip(keepalive_interval, keepalive_message)))]
async fn socket_loop_split(
    mut receiver: mpsc::Receiver<TxChannelPayload>,
    mut sender: mpsc::Sender<Message>,
    mut sink: SocketSink,
    mut stream: SocketStream,
    keepalive_interval: Option<Duration>,
    keepalive_message: Option<String>,
) -> Result<(), Error> {
    let mut state = LoopState::Running;
    while matches!(state, LoopState::Running) {
        state = if let Some(interval) = keepalive_interval {
            select! {
                outgoing_message = receiver.recv() => send_socket_message(outgoing_message, &mut sink).await,
                incoming_message = stream.next() => socket_message_received(incoming_message, &mut sender, &mut sink).await,
                () = sleep(interval) => send_keepalive(&mut sink, keepalive_message.as_deref()).await,
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
async fn send_keepalive(sink: &mut SocketSink, custom_message: Option<&str>) -> LoopState {
    let message = if let Some(text) = custom_message {
        #[cfg(feature = "tracing")]
        info!("Timeout waiting for message, sending custom keepalive");
        Message::Text(text.into())
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

    #[tokio::test]
    async fn test_server_startup() {
        let _server_address = get_mock_address(echo_server).await;
    }

    #[tokio::test]
    async fn test_connection() {
        let server_address = get_mock_address(echo_server).await;
        let _socketeer: Socketeer<EchoControlMessage, EchoControlMessage> =
            Socketeer::connect(&format!("ws://{server_address}",))
                .await
                .unwrap();
    }

    #[tokio::test]
    async fn test_bad_url() {
        let error: Result<Socketeer<EchoControlMessage, EchoControlMessage>, Error> =
            Socketeer::connect("Not a URL").await;
        assert!(matches!(error.unwrap_err(), Error::UrlParse { .. }));
    }

    #[tokio::test]
    async fn test_send_receive() {
        let server_address = get_mock_address(echo_server).await;
        let mut socketeer: Socketeer<EchoControlMessage, EchoControlMessage> =
            Socketeer::connect(&format!("ws://{server_address}",))
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
        let mut socketeer: Socketeer<EchoControlMessage, EchoControlMessage> =
            Socketeer::connect(&format!("ws://{server_address}",))
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
        let mut socketeer: Socketeer<EchoControlMessage, EchoControlMessage> =
            Socketeer::connect(&format!("ws://{server_address}",))
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
        let mut socketeer: Socketeer<EchoControlMessage, EchoControlMessage> =
            Socketeer::connect(&format!("ws://{server_address}",))
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
        let socketeer: Socketeer<EchoControlMessage, EchoControlMessage> =
            Socketeer::connect(&format!("ws://{server_address}",))
                .await
                .unwrap();
        socketeer.close_connection().await.unwrap();
    }
}
