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

#[derive(Debug)]
pub struct Socketeer<RxMessage: for<'a> Deserialize<'a> + Debug, TxMessage: Serialize + Debug> {
    _url: Url,
    receiever: mpsc::Receiver<Message>,
    sender: mpsc::Sender<TxChannelPayload>,
    socket_handle: tokio::task::JoinHandle<Result<(), Error>>,
    _rx_message: std::marker::PhantomData<RxMessage>,
    _tx_message: std::marker::PhantomData<TxMessage>,
}

impl<RxMessage: for<'a> Deserialize<'a> + Debug, TxMessage: Serialize + Debug>
    Socketeer<RxMessage, TxMessage>
{
    /// Create a `Socketeer` connected to the provided URL.
    /// Once connected, Socketeer manages the underlying WebSocket connection, transparently handling protocol messages.
    /// # Errors
    /// - If the URL cannot be parsed
    /// - If the WebSocket connection to the requested URL fails
    #[cfg_attr(feature = "tracing", instrument)]
    pub async fn connect(url: &str) -> Result<Socketeer<RxMessage, TxMessage>, Error> {
        let url = Url::parse(url).map_err(|source| Error::UrlParse {
            url: url.to_string(),
            source,
        })?;
        #[allow(unused_variables)]
        let (socket, response) = connect_async(url.as_str()).await?;
        #[cfg(feature = "tracing")]
        info!("Connection Successful, connection info: \n{:#?}", response);
        //let (sink, stream) = socket.split();
        let (tx_tx, tx_rx) = mpsc::channel::<TxChannelPayload>(8);
        let (rx_tx, rx_rx) = mpsc::channel::<Message>(8);

        let socket_handle = tokio::spawn(async move { socket_loop(tx_rx, rx_tx, socket).await });
        //let cross_tx = tx_tx.clone();
        //let rx_handle = tokio::spawn(async move { rx_loop(rx_tx, stream, cross_tx).await });
        Ok(Socketeer {
            _url: url,
            receiever: rx_rx,
            sender: tx_tx,
            socket_handle,
            _rx_message: std::marker::PhantomData,
            _tx_message: std::marker::PhantomData,
        })
    }

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
            _ => Err(Error::UnexpectedMessage(message)),
        }
    }

    #[cfg_attr(feature = "tracing", instrument)]
    pub async fn send(&self, message: TxMessage) -> Result<(), Error> {
        #[cfg(feature = "tracing")]
        debug!("Sending message: {:?}", message);

        let (tx, rx) = oneshot::channel::<Result<(), Error>>();
        let message = serde_json::to_string(&message).unwrap();

        self.sender
            .send(TxChannelPayload {
                message: Message::Text(message.into()),
                response_tx: tx,
            })
            .await
            .map_err(|_| Error::WebsocketClosed)?;
        // We'll ensure that we always respond before dropping the tx channel
        rx.await.unwrap()
    }

    #[cfg_attr(feature = "tracing", instrument)]
    pub async fn close_connection(self) -> Result<(), Error> {
        #[cfg(feature = "tracing")]
        debug!("Closing Connection");
        let (tx, rx) = oneshot::channel::<Result<(), Error>>();
        self.sender
            .send(TxChannelPayload {
                message: Message::Close(None),
                response_tx: tx,
            })
            .await
            .map_err(|_| Error::WebsocketClosed)?;
        rx.await.unwrap()?;
        self.socket_handle.await.unwrap().unwrap();
        Ok(())
    }
}

pub(crate) type WebSocketStreamType = WebSocketStream<MaybeTlsStream<TcpStream>>;
type SocketSink = SplitSink<WebSocketStreamType, Message>;

async fn socket_loop(
    mut receiver: mpsc::Receiver<TxChannelPayload>,
    mut sender: mpsc::Sender<Message>,
    socket: WebSocketStreamType,
) -> Result<(), Error> {
    let mut should_run = true;
    let mut result = Ok(());
    let (mut sink, mut stream) = socket.split();
    while should_run {
        result = select! {
            outgoing_message = receiver.recv() => send_socket_message(outgoing_message, &mut sink).await,
            incoming_message = stream.next() => socket_message_received(&mut should_run, incoming_message,&mut sender, &mut sink).await,
            _ = sleep(Duration::from_secs(2)) => send_ping(&mut sink).await,
        };
        if result.is_err() {
            should_run = false;
        }
    }
    result
}

async fn send_socket_message(
    message: Option<TxChannelPayload>,
    sink: &mut SocketSink,
) -> Result<(), Error> {
    match message {
        Some(message) => {
            #[cfg(feature = "tracing")]
            debug!("Sending message: {:?}", message);
            let send_result = sink.send(message.message).await.map_err(Error::from);
            let socket_error = send_result.is_err();
            match message.response_tx.send(send_result) {
                Ok(()) => {
                    if socket_error {
                        Err(Error::WebsocketClosed)
                    } else {
                        Ok(())
                    }
                }
                Err(_) => Err(Error::SocketeerDropped),
            }
        }
        None => {
            #[cfg(feature = "tracing")]
            error!("Socketeer dropped without closing connection");
            Err(Error::SocketeerDropped)
        }
    }
}

async fn socket_message_received(
    should_run: &mut bool,
    message: Option<Result<Message, tungstenite::Error>>,
    sender: &mut mpsc::Sender<Message>,
    sink: &mut SocketSink,
) -> Result<(), Error> {
    const PONG_BYTES: Bytes = Bytes::from_static(b"pong");
    match message {
        Some(Ok(message)) => match message {
            Message::Ping(_) => sink
                .send(Message::Pong(PONG_BYTES))
                .await
                .map_err(Error::from),
            Message::Close(_) => {
                *should_run = false;
                Ok(())
            }
            Message::Text(_) | Message::Binary(_) => match sender.send(message).await {
                Ok(()) => Ok(()),
                Err(_) => Err(Error::SocketeerDropped),
            },
            _ => Ok(()),
        },
        Some(Err(e)) => {
            #[cfg(feature = "tracing")]
            error!("Error receiving message: {:?}", e);
            Err(Error::WebsocketError(e))
        }
        None => {
            #[cfg(feature = "tracing")]
            info!("Websocket Closed, closing rx channel");
            Err(Error::WebsocketClosed)
        }
    }
}

async fn send_ping(sink: &mut SocketSink) -> Result<(), Error> {
    #[cfg(feature = "tracing")]
    info!("Timeout waiting for message, sending Ping");
    sink.send(Message::Ping(Bytes::new()))
        .await
        .map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use tokio::time::sleep;

    use super::*;

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
        // TODO: Send needs to pass a one-shot and actually wait for the result
        let send_result = socketeer.send(close_request).await;
        assert!(send_result.is_err());
        assert!(matches!(
            send_result.unwrap_err(),
            Error::WebsocketError(..)
        ));
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
