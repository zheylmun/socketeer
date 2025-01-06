#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
mod error;
#[cfg(feature = "mocking")]
mod mock_server;
#[cfg(feature = "mocking")]
pub use mock_server::{echo_server, get_mock_address, EchoControlMessage};

use bytes::Bytes;
pub use error::Error;
use futures::{stream::SplitSink, SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{fmt::Debug, time::Duration};
use tokio::{
    net::TcpStream,
    select,
    sync::{mpsc, oneshot},
    time::sleep,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, protocol::CloseFrame, Message, Utf8Bytes},
    MaybeTlsStream, WebSocketStream,
};
#[cfg(feature = "tracing")]
use tracing::{debug, error, info, instrument};
use url::Url;

#[derive(Debug)]
struct TxChannelPayload {
    message: Message,
    response_tx: oneshot::Sender<Result<(), Error>>,
}

/// A WebSocket client that manages the connection to a WebSocket server.
/// The client can send and receive messages, and will transparently handle protocol messages.
/// # Type Parameters
/// - `RxMessage`: The type of message that the client will receive from the server.
/// - `TxMessage`: The type of message that the client will send to the server.
/// - `CHANNEL_SIZE`: The size of the internal channels used to communicate between
///     the task managing the WebSocket connection and the client.
#[derive(Debug)]
pub struct Socketeer<
    RxMessage: for<'a> Deserialize<'a> + Debug,
    TxMessage: Serialize + Debug,
    const CHANNEL_SIZE: usize = 4,
> {
    url: Url,
    receiever: mpsc::Receiver<Message>,
    sender: mpsc::Sender<TxChannelPayload>,
    socket_handle: tokio::task::JoinHandle<Result<(), Error>>,
    _rx_message: std::marker::PhantomData<RxMessage>,
    _tx_message: std::marker::PhantomData<TxMessage>,
}

impl<
        RxMessage: for<'a> Deserialize<'a> + Debug,
        TxMessage: Serialize + Debug,
        const CHANNEL_SIZE: usize,
    > Socketeer<RxMessage, TxMessage, CHANNEL_SIZE>
{
    /// Create a `Socketeer` connected to the provided URL.
    /// Once connected, Socketeer manages the underlying WebSocket connection, transparently handling protocol messages.
    /// # Errors
    /// - If the URL cannot be parsed
    /// - If the WebSocket connection to the requested URL fails
    #[cfg_attr(feature = "tracing", instrument)]
    pub async fn connect(
        url: &str,
    ) -> Result<Socketeer<RxMessage, TxMessage, CHANNEL_SIZE>, Error> {
        let url = Url::parse(url).map_err(|source| Error::UrlParse {
            url: url.to_string(),
            source,
        })?;
        #[allow(unused_variables)]
        let (socket, response) = connect_async(url.as_str()).await?;
        #[cfg(feature = "tracing")]
        info!("Connection Successful, connection info: \n{:#?}", response);

        let (tx_tx, tx_rx) = mpsc::channel::<TxChannelPayload>(CHANNEL_SIZE);
        let (rx_tx, rx_rx) = mpsc::channel::<Message>(CHANNEL_SIZE);

        let socket_handle = tokio::spawn(async move { socket_loop(tx_rx, rx_tx, socket).await });
        Ok(Socketeer {
            url,
            receiever: rx_rx,
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
    #[cfg_attr(feature = "tracing", instrument)]
    pub async fn next_message(&mut self) -> Result<RxMessage, Error> {
        let Some(message) = self.receiever.recv().await else {
            return Err(Error::WebsocketClosed);
        };
        match message {
            Message::Text(text) => {
                #[cfg(feature = "tracing")]
                debug!("Received message: {:?}", text);
                let message = serde_json::from_str(&text)?;
                Ok(message)
            }
            Message::Binary(message) => {
                #[cfg(feature = "tracing")]
                debug!("Received message: {:?}", message);
                let message = serde_json::from_slice(&message)?;
                Ok(message)
            }
            _ => Err(Error::UnexpectedMessageType(message)),
        }
    }

    /// Send a message to the WebSocket connection.
    /// This function will wait for the message to be sent before returning.
    ///
    /// # Errors
    ///
    /// - If the message cannot be serialized
    /// - If the WebSocket connection is closed, or otherwise errored
    #[cfg_attr(feature = "tracing", instrument)]
    pub async fn send(&self, message: TxMessage) -> Result<(), Error> {
        #[cfg(feature = "tracing")]
        debug!("Sending message: {:?}", message);

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

    /// Consume self, closing down any remaining send/recieve, and return a new Socketeer instance if successful
    /// This function attempts to close the connection gracefully before returning,
    /// but will not return an error if the connection is already closed,
    /// as its intended use is to re-establish a failed connection.
    /// # Errors
    /// - If a new connection cannot be established
    #[cfg_attr(feature = "tracing", instrument)]
    pub async fn reconnect(self) -> Result<Self, Error> {
        let url = self.url.as_str().to_owned();
        #[cfg(feature = "tracing")]
        info!("Reconnecting");
        match self.close_connection().await {
            Ok(()) => (),
            #[allow(unused_variables)]
            Err(e) => {
                #[cfg(feature = "tracing")]
                info!("Socket Loop already stopped: {}", e);
            }
        }
        #[cfg(feature = "tracing")]
        info!("Reconnecting");
        Self::connect(&url).await
    }

    /// Close the WebSocket connection gracefully.
    /// This function will wait for the connection to close before returning.
    /// # Errors
    /// - If the WebSocket connection is already closed
    /// - If the WebSocket connection cannot be closed
    #[cfg_attr(feature = "tracing", instrument)]
    pub async fn close_connection(self) -> Result<(), Error> {
        #[cfg(feature = "tracing")]
        debug!("Closing Connection");
        let (tx, rx) = oneshot::channel::<Result<(), Error>>();
        self.sender
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
        }?;
        match self.socket_handle.await {
            Ok(result) => result,
            Err(_) => unreachable!("Socket loop does not panic, and is not cancelled"),
        }
    }
}

pub(crate) type WebSocketStreamType = WebSocketStream<MaybeTlsStream<TcpStream>>;
type SocketSink = SplitSink<WebSocketStreamType, Message>;

enum LoopState {
    Running,
    Error(Error),
    Closed,
}

#[cfg_attr(feature = "tracing", instrument)]
async fn socket_loop(
    mut receiver: mpsc::Receiver<TxChannelPayload>,
    mut sender: mpsc::Sender<Message>,
    socket: WebSocketStreamType,
) -> Result<(), Error> {
    let mut state = LoopState::Running;
    let (mut sink, mut stream) = socket.split();
    while matches!(state, LoopState::Running) {
        state = select! {
            outgoing_message = receiver.recv() => send_socket_message(outgoing_message, &mut sink).await,
            incoming_message = stream.next() => socket_message_received( incoming_message,&mut sender, &mut sink).await,
            () = sleep(Duration::from_secs(2)) => send_ping(&mut sink).await,
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
async fn send_ping(sink: &mut SocketSink) -> LoopState {
    #[cfg(feature = "tracing")]
    info!("Timeout waiting for message, sending Ping");
    let result = sink
        .send(Message::Ping(Bytes::new()))
        .await
        .map_err(Error::from);
    match result {
        Ok(()) => LoopState::Running,
        Err(e) => {
            #[cfg(feature = "tracing")]
            error!("Error sending Ping: {:?}", e);
            LoopState::Error(e)
        }
    }
}

#[cfg(test)]
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
