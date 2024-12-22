mod error;
use std::fmt::Debug;

use bytes::Bytes;
pub use error::Error;
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};

use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpStream,
    sync::{broadcast, mpsc},
};

use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use tracing::{debug, error, info, instrument};

pub type WebSocketStreamType = WebSocketStream<MaybeTlsStream<TcpStream>>;
type SocketStream = SplitStream<WebSocketStreamType>;
type SocketSink = SplitSink<WebSocketStreamType, Message>;

#[derive(Debug)]
pub struct Socketeer<RxMessage: for<'a> Deserialize<'a> + Debug, TxMessage: Serialize + Debug> {
    receiever: broadcast::Receiver<Message>,
    sender: mpsc::Sender<Message>,
    _rx_message: std::marker::PhantomData<RxMessage>,
    _tx_message: std::marker::PhantomData<TxMessage>,
}

impl<RxMessage: for<'a> Deserialize<'a> + Debug, TxMessage: Serialize + Debug>
    Socketeer<RxMessage, TxMessage>
{
    #[instrument]
    pub async fn connect(url: &str) -> Result<Socketeer<RxMessage, TxMessage>, Error> {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install rustls crypto provider");
        let (socket, response) = connect_async(url).await?;
        info!("Connection Successful, connection info: \n{:#?}", response);
        let (sink, stream) = socket.split();
        let (tx_tx, tx_rx) = mpsc::channel::<Message>(8);
        let (rx_tx, rx_rx) = broadcast::channel::<Message>(8);

        tokio::spawn(async move {
            tx_loop(tx_rx, sink).await;
        });
        let cross_tx = tx_tx.clone();
        tokio::spawn(async move { rx_loop(rx_tx, stream, cross_tx).await });
        Ok(Socketeer {
            receiever: rx_rx,
            sender: tx_tx,
            _rx_message: std::marker::PhantomData,
            _tx_message: std::marker::PhantomData,
        })
    }

    #[instrument]
    pub async fn next_message(&mut self) -> Result<RxMessage, Error> {
        let message = self.receiever.recv().await.map_err(|err| match err {
            broadcast::error::RecvError::Closed => Error::WebSocketClosed,
            broadcast::error::RecvError::Lagged(_) => Error::ChannelFull,
        })?;
        match message {
            Message::Text(text) => {
                debug!("Received message: {:?}", text);
                let message = serde_json::from_str(&text).unwrap();
                Ok(message)
            }
            _ => Err(Error::UnexpectedMessage(message)),
        }
    }

    #[instrument]
    pub async fn send(&self, message: TxMessage) -> Result<(), Error> {
        let message = serde_json::to_string(&message).unwrap();
        debug!("Sending message: {:?}", message);
        self.sender
            .send(Message::Text(message.into()))
            .await
            .unwrap();
        Ok(())
    }
}

#[instrument]
async fn tx_loop(mut receiver: mpsc::Receiver<Message>, mut sink: SocketSink) {
    loop {
        match receiver.recv().await {
            Some(message) => {
                debug!("Sending message: {:?}", message);
                sink.send(message).await.unwrap();
            }
            None => {
                break;
            }
        }
    }
}

#[instrument]
async fn rx_loop(
    sender: broadcast::Sender<Message>,
    mut stream: SocketStream,
    tx_sender: mpsc::Sender<Message>,
) {
    const PONG_BYTES: Bytes = Bytes::from_static(b"pong");
    info!("Starting RX loop");
    loop {
        let message = stream.next().await;
        debug!("Received message: {:?}", message);
        match message {
            Some(Ok(message)) => match message {
                Message::Ping(_) => {
                    debug!("Ping message received, sending Pong");
                    tx_sender.send(Message::Pong(PONG_BYTES)).await.unwrap();
                }
                Message::Close(_) => {
                    info!("Close message received, closing RX channel");
                    break;
                }
                Message::Text(_) | Message::Binary(_) => {
                    sender.send(message).unwrap();
                }
                _ => {}
            },
            Some(Err(e)) => {
                error!("Error receiving message: {:?}", e);
                break;
            }
            None => {
                info!("Websocket Closed, closing rx channel");
                break;
            }
        }
    }
}
