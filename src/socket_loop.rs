//! Internal background task that owns the split WebSocket stream and mediates
//! between the public [`crate::Socketeer`] handle and the network.
//!
//! The handle never touches the socket directly. It communicates with this loop
//! exclusively through the mpsc channels set up in
//! [`crate::Socketeer::connect_with_codec`]: outgoing frames arrive as
//! [`TxChannelPayload`] on the tx channel, and incoming data frames are pushed
//! onto the rx channel. Protocol frames (ping/pong/close) are handled here and
//! never surface to the consumer.

use bytes::Bytes;
use futures::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::{
    net::TcpStream,
    select,
    sync::{mpsc, oneshot},
    time::sleep,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{self, Message, Utf8Bytes, protocol::CloseFrame},
};

#[cfg(feature = "tracing")]
use tracing::{error, info, instrument, trace};

use crate::Error;

/// A frame to send, paired with a one-shot the loop uses to report the result
/// of the underlying `sink.send`.
#[derive(Debug)]
pub(crate) struct TxChannelPayload {
    pub(crate) message: Message,
    pub(crate) response_tx: oneshot::Sender<Result<(), Error>>,
}

pub(crate) type WebSocketStreamType = WebSocketStream<MaybeTlsStream<TcpStream>>;
type SocketSink = SplitSink<WebSocketStreamType, Message>;
type SocketStream = SplitStream<WebSocketStreamType>;

enum LoopState {
    Running,
    Error(Error),
    Closed,
}

/// Shared slot where the loop records the error that terminated it, so the
/// handle can surface the real cause instead of a generic closure once the
/// channels drop.
pub(crate) type TerminalError = Arc<Mutex<Option<Error>>>;

/// Send a close frame via the tx channel and wait for confirmation.
pub(crate) async fn send_close(sender: &mpsc::Sender<TxChannelPayload>) -> Result<(), Error> {
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
    instrument(skip(keepalive_interval, keepalive_message, terminal_error))
)]
pub(crate) async fn socket_loop_split(
    mut receiver: mpsc::Receiver<TxChannelPayload>,
    mut sender: mpsc::Sender<Message>,
    mut sink: SocketSink,
    mut stream: SocketStream,
    keepalive_interval: Option<Duration>,
    keepalive_message: Option<Message>,
    terminal_error: TerminalError,
) {
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
    // Record the terminal cause before returning. The `receiver`/`sender`
    // channels are owned here and only drop once this function returns, so the
    // handle cannot observe the closed channel until after this write lands —
    // no race between recording the error and a consumer reading it.
    match state {
        LoopState::Error(e) => {
            *terminal_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(e);
        }
        LoopState::Closed => {}
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
        trace!("Sending message: {:?}", message);
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
        trace!("Timeout waiting for message, sending custom keepalive");
        custom.clone()
    } else {
        #[cfg(feature = "tracing")]
        trace!("Timeout waiting for message, sending Ping");
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
