mod error;
#[cfg(feature = "mocking")]
mod mock_server;
#[cfg(feature = "mocking")]
pub use mock_server::{echo_server, get_mock_address, EchoControlMessage};

use bytes::Bytes;
pub use error::Error;
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use tokio::{
    net::TcpStream,
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
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
    _rx_message: std::marker::PhantomData<RxMessage>,
    _tx_message: std::marker::PhantomData<TxMessage>,
}

impl<RxMessage: for<'a> Deserialize<'a> + Debug, TxMessage: Serialize + Debug>
    Socketeer<RxMessage, TxMessage>
{
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
        let (sink, stream) = socket.split();
        let (tx_tx, tx_rx) = mpsc::channel::<TxChannelPayload>(8);
        let (rx_tx, rx_rx) = mpsc::channel::<Message>(8);

        tokio::spawn(async move {
            tx_loop(tx_rx, sink).await;
        });
        let cross_tx = tx_tx.clone();
        tokio::spawn(async move { rx_loop(rx_tx, stream, cross_tx).await });
        Ok(Socketeer {
            _url: url,
            receiever: rx_rx,
            sender: tx_tx,
            _rx_message: std::marker::PhantomData,
            _tx_message: std::marker::PhantomData,
        })
    }

    #[cfg_attr(feature = "tracing", instrument)]
    pub async fn next_message(&mut self) -> Result<RxMessage, Error> {
        let Some(message) = self.receiever.recv().await else {
            return Err(Error::WebSocketClosed);
        };
        match message {
            Message::Text(text) => {
                #[cfg(feature = "tracing")]
                debug!("Received message: {:?}", text);
                let message = serde_json::from_str(&text).unwrap();
                Ok(message)
            }
            Message::Binary(message) => {
                #[cfg(feature = "tracing")]
                debug!("Received message: {:?}", message);
                let message = serde_json::from_slice(&message).unwrap();
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
            .unwrap();
        // We'll ensure that we always respond before dropping the tx channel
        rx.await.unwrap()
    }

    #[cfg_attr(feature = "tracing", instrument)]
    pub async fn close_connection(&self) -> Result<(), Error> {
        #[cfg(feature = "tracing")]
        debug!("Closing Connection");
        let (tx, rx) = oneshot::channel::<Result<(), Error>>();
        self.sender
            .send(TxChannelPayload {
                message: Message::Close(None),
                response_tx: tx,
            })
            .await
            .unwrap();
        rx.await.unwrap()
    }
}

pub(crate) type WebSocketStreamType = WebSocketStream<MaybeTlsStream<TcpStream>>;
type SocketStream = SplitStream<WebSocketStreamType>;
type SocketSink = SplitSink<WebSocketStreamType, Message>;

#[cfg_attr(feature = "tracing", instrument)]
async fn tx_loop(mut receiver: mpsc::Receiver<TxChannelPayload>, mut sink: SocketSink) {
    while let Some(message) = receiver.recv().await {
        #[cfg(feature = "tracing")]
        debug!("Sending message: {:?}", message);

        message
            .response_tx
            .send(sink.send(message.message).await.map_err(Error::from))
            .unwrap();
    }
}

#[cfg_attr(feature = "tracing", instrument)]
async fn rx_loop(
    sender: mpsc::Sender<Message>,
    mut stream: SocketStream,
    tx_sender: mpsc::Sender<TxChannelPayload>,
) {
    const PONG_BYTES: Bytes = Bytes::from_static(b"pong");
    #[cfg(feature = "tracing")]
    info!("Starting RX loop");
    loop {
        let message = stream.next().await;
        #[cfg(feature = "tracing")]
        debug!("Received message: {:?}", message);
        match message {
            Some(Ok(message)) => match message {
                Message::Ping(_) => {
                    #[cfg(feature = "tracing")]
                    debug!("Ping message received, sending Pong");
                    let (tx, rx) = oneshot::channel();
                    let payload = TxChannelPayload {
                        message: Message::Pong(PONG_BYTES),
                        response_tx: tx,
                    };
                    #[allow(unused_variables)]
                    if let Err(error) = tx_sender.send(payload).await {
                        // If the tx task has been dropped, we can't send a response, terminate the loop
                        #[cfg(feature = "tracing")]
                        error!("Error sending pong: {:?}", error);
                        break;
                    }
                    #[allow(unused_variables)]
                    if let Err(error) = rx.await.unwrap() {
                        // If the actual tx failed, we should also break the loop
                        #[cfg(feature = "tracing")]
                        error!("Error sending pong: {:?}", error);
                        break;
                    }
                }
                Message::Close(_) => {
                    #[cfg(feature = "tracing")]
                    info!("Close message received, closing RX channel");
                    break;
                }
                Message::Text(_) | Message::Binary(_) => {
                    sender.send(message).await.unwrap();
                }
                _ => {}
            },
            #[allow(unused_variables)]
            Some(Err(e)) => {
                #[cfg(feature = "tracing")]
                error!("Error receiving message: {:?}", e);
                break;
            }
            None => {
                #[cfg(feature = "tracing")]
                info!("Websocket Closed, closing rx channel");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
        assert!(matches!(response.unwrap_err(), Error::WebSocketClosed));
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
        let mut socketeer: Socketeer<EchoControlMessage, EchoControlMessage> =
            Socketeer::connect(&format!("ws://{server_address}",))
                .await
                .unwrap();
        socketeer.close_connection().await.unwrap();
        // The server will respond with a ping request, which Socketeer will transparently respond to
        let response = socketeer.next_message().await;
        assert!(matches!(response.unwrap_err(), Error::WebSocketClosed));
        // TODO: Send needs to pass a one-shot and actually wait for the result
        // let send_result = socketeer.send(close_request).await;
        // assert!(matches!(send_result.unwrap_err(), Error::WebSocketClosed));
    }
}
