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
use std::task::{Context, Poll};
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
/// of the underlying `sink.send`. `None` for fire-and-forget sends.
#[derive(Debug)]
pub(crate) struct TxChannelPayload {
    pub(crate) message: Message,
    pub(crate) response_tx: Option<oneshot::Sender<Result<(), Error>>>,
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

/// Take the recorded terminal error, if any. The cause is surfaced once: the
/// first caller consumes it, later callers see `None`. A graceful close records
/// nothing, so this returns `None`.
pub(crate) fn take_terminal_error_opt(slot: &TerminalError) -> Option<Error> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// Like [`take_terminal_error_opt`] but falls back to [`Error::WebsocketClosed`]
/// when no specific cause was recorded.
pub(crate) fn take_terminal_error(slot: &TerminalError) -> Error {
    take_terminal_error_opt(slot).unwrap_or(Error::WebsocketClosed)
}

/// Enqueue a frame and await the loop's result for this specific send.
pub(crate) async fn send_confirmed(
    sender: &mpsc::Sender<TxChannelPayload>,
    terminal_error: &TerminalError,
    message: Message,
) -> Result<(), Error> {
    let (tx, rx) = oneshot::channel::<Result<(), Error>>();
    sender
        .send(TxChannelPayload {
            message,
            response_tx: Some(tx),
        })
        .await
        .map_err(|_| take_terminal_error(terminal_error))?;
    match rx.await {
        Ok(result) => result,
        Err(_) => unreachable!("Socket loop always sends response before dropping one-shot"),
    }
}

/// Enqueue a frame without awaiting the loop's per-send result. Applies
/// backpressure (awaits channel capacity) but returns `Ok` as soon as the
/// frame is queued; a socket-send failure surfaces via the terminal error.
pub(crate) async fn send_unconfirmed(
    sender: &mpsc::Sender<TxChannelPayload>,
    terminal_error: &TerminalError,
    message: Message,
) -> Result<(), Error> {
    sender
        .send(TxChannelPayload {
            message,
            response_tx: None,
        })
        .await
        .map_err(|_| take_terminal_error(terminal_error))
}

/// Send a close frame via the tx channel and wait for confirmation.
pub(crate) async fn send_close(sender: &mpsc::Sender<TxChannelPayload>) -> Result<(), Error> {
    let (tx, rx) = oneshot::channel::<Result<(), Error>>();
    sender
        .send(TxChannelPayload {
            message: Message::Close(Some(CloseFrame {
                code: tungstenite::protocol::frame::coding::CloseCode::Normal,
                reason: Utf8Bytes::from_static("Closing Connection"),
            })),
            response_tx: Some(tx),
        })
        .await
        .map_err(|_| Error::WebsocketClosed)?;
    match rx.await {
        Ok(result) => result,
        Err(_) => unreachable!("Socket loop always sends response before dropping one-shot"),
    }
}

/// Await the next inbound data frame, mapping a closed channel to the loop's
/// recorded terminal cause. The async counterpart to [`send_confirmed`]: both
/// honor the same terminal-error contract so the handle and its split half share
/// one receive path.
pub(crate) async fn recv_raw(
    receiver: &mut mpsc::Receiver<Message>,
    terminal_error: &TerminalError,
) -> Result<Message, Error> {
    match receiver.recv().await {
        Some(message) => {
            #[cfg(feature = "tracing")]
            trace!("Received message: {:?}", message);
            Ok(message)
        }
        None => Err(take_terminal_error(terminal_error)),
    }
}

/// Poll for the next inbound data frame, for `Stream` implementations. A closed
/// channel surfaces the recorded terminal cause once (subsequent polls see the
/// stream end). The polling counterpart to [`recv_raw`].
pub(crate) fn poll_recv_raw(
    receiver: &mut mpsc::Receiver<Message>,
    terminal_error: &TerminalError,
    cx: &mut Context<'_>,
) -> Poll<Option<Result<Message, Error>>> {
    match receiver.poll_recv(cx) {
        Poll::Ready(Some(message)) => Poll::Ready(Some(Ok(message))),
        Poll::Ready(None) => match take_terminal_error_opt(terminal_error) {
            Some(error) => Poll::Ready(Some(Err(error))),
            None => Poll::Ready(None),
        },
        Poll::Pending => Poll::Pending,
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
                incoming_message = stream.next() => match handle_incoming(incoming_message, &mut sink).await {
                    Incoming::State(s) => s,
                    Incoming::Data(frame) => deliver_with_backpressure(
                        frame,
                        &mut sender,
                        &mut receiver,
                        &mut sink,
                        keepalive_interval,
                        keepalive_message.as_ref(),
                    ).await,
                },
                () = sleep(interval) => send_keepalive(&mut sink, keepalive_message.as_ref()).await,
            }
        } else {
            select! {
                outgoing_message = receiver.recv() => send_socket_message(outgoing_message, &mut sink).await,
                incoming_message = stream.next() => match handle_incoming(incoming_message, &mut sink).await {
                    Incoming::State(s) => s,
                    Incoming::Data(frame) => deliver_with_backpressure(
                        frame,
                        &mut sender,
                        &mut receiver,
                        &mut sink,
                        keepalive_interval,
                        keepalive_message.as_ref(),
                    ).await,
                },
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
        if let Some(response_tx) = message.response_tx {
            if response_tx.send(send_result).is_err() {
                return LoopState::Error(Error::SocketeerDroppedWithoutClosing);
            }
        }
        if socket_error {
            LoopState::Error(Error::WebsocketClosed)
        } else {
            LoopState::Running
        }
    } else {
        #[cfg(feature = "tracing")]
        error!("Socketeer dropped without closing connection");
        LoopState::Error(Error::SocketeerDroppedWithoutClosing)
    }
}

enum Incoming {
    State(LoopState),
    Data(Message),
}

#[cfg_attr(feature = "tracing", instrument)]
async fn handle_incoming(
    message: Option<Result<Message, tungstenite::Error>>,
    sink: &mut SocketSink,
) -> Incoming {
    const PONG_BYTES: Bytes = Bytes::from_static(b"pong");
    match message {
        Some(Ok(message)) => match message {
            Message::Ping(_) => {
                let send_result = sink
                    .send(Message::Pong(PONG_BYTES))
                    .await
                    .map_err(Error::from);
                Incoming::State(match send_result {
                    Ok(()) => LoopState::Running,
                    Err(e) => {
                        #[cfg(feature = "tracing")]
                        error!("Error sending Pong: {:?}", e);
                        LoopState::Error(e)
                    }
                })
            }
            Message::Close(_) => {
                let close_result = sink.close().await;
                Incoming::State(match close_result {
                    Ok(()) => LoopState::Closed,
                    Err(e) => {
                        #[cfg(feature = "tracing")]
                        error!("Error sending Close: {:?}", e);
                        LoopState::Error(Error::from(e))
                    }
                })
            }
            Message::Text(_) | Message::Binary(_) => Incoming::Data(message),
            _ => Incoming::State(LoopState::Running),
        },
        Some(Err(e)) => {
            #[cfg(feature = "tracing")]
            error!("Error receiving message: {:?}", e);
            Incoming::State(LoopState::Error(Error::WebsocketError(e)))
        }
        None => {
            #[cfg(feature = "tracing")]
            info!("Websocket Closed, closing rx channel");
            Incoming::State(LoopState::Error(Error::WebsocketClosed))
        }
    }
}

/// Deliver one inbound data frame to the consumer.
///
/// Fast path: `try_send`. If the receive channel is full, hold the single frame
/// and wait for capacity via `reserve()` while still servicing outgoing sends
/// and firing keepalives — but do NOT read the inbound stream. Unread bytes stay
/// in the TCP buffer (end-to-end backpressure), memory is bounded to this one
/// frame, and ordering is preserved (this frame is delivered before the loop
/// reads the next).
async fn deliver_with_backpressure(
    frame: Message,
    sender: &mut mpsc::Sender<Message>,
    receiver: &mut mpsc::Receiver<TxChannelPayload>,
    sink: &mut SocketSink,
    keepalive_interval: Option<Duration>,
    keepalive_message: Option<&Message>,
) -> LoopState {
    let frame = match sender.try_send(frame) {
        Ok(()) => return LoopState::Running,
        Err(mpsc::error::TrySendError::Closed(_)) => {
            return LoopState::Error(Error::SocketeerDroppedWithoutClosing);
        }
        Err(mpsc::error::TrySendError::Full(frame)) => frame,
    };
    #[cfg(feature = "tracing")]
    trace!("Receive buffer full; holding frame and applying backpressure");
    loop {
        if let Some(interval) = keepalive_interval {
            select! {
                permit = sender.reserve() => match permit {
                    Ok(permit) => {
                        permit.send(frame);
                        return LoopState::Running;
                    }
                    Err(_) => return LoopState::Error(Error::SocketeerDroppedWithoutClosing),
                },
                outgoing = receiver.recv() => match send_socket_message(outgoing, sink).await {
                    LoopState::Running => {}
                    other => return other,
                },
                () = sleep(interval) => match send_keepalive(sink, keepalive_message).await {
                    LoopState::Running => {}
                    other => return other,
                },
            }
        } else {
            select! {
                permit = sender.reserve() => match permit {
                    Ok(permit) => {
                        permit.send(frame);
                        return LoopState::Running;
                    }
                    Err(_) => return LoopState::Error(Error::SocketeerDroppedWithoutClosing),
                },
                outgoing = receiver.recv() => match send_socket_message(outgoing, sink).await {
                    LoopState::Running => {}
                    other => return other,
                },
            }
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
