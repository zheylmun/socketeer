//! Owned, splittable handles for concurrent send/receive use.
//!
//! [`Socketeer::split`](crate::Socketeer::split) divides a connected client
//! into a cloneable [`SocketeerTx`] send half and a [`SocketeerRx`] receive
//! half that can move into separate tasks. [`SocketeerRx::reunite`] recombines
//! them to regain the full [`Socketeer`](crate::Socketeer) handle.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::socket_loop::{
    TerminalError, TxChannelPayload, send_close, send_confirmed, send_unconfirmed,
    take_terminal_error,
};
use crate::{Codec, ConnectOptions, ConnectionHandler, Error, NoopHandler};

/// The cloneable send half of a [`Socketeer`](crate::Socketeer), produced by
/// [`Socketeer::split`](crate::Socketeer::split). Clone it to send from
/// multiple tasks concurrently.
pub struct SocketeerTx<C: Codec> {
    pub(crate) sender: mpsc::Sender<TxChannelPayload>,
    pub(crate) codec: C,
    pub(crate) terminal_error: TerminalError,
}

impl<C: Codec + Clone> Clone for SocketeerTx<C> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            codec: self.codec.clone(),
            terminal_error: Arc::clone(&self.terminal_error),
        }
    }
}

impl<C: Codec> std::fmt::Debug for SocketeerTx<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocketeerTx").finish_non_exhaustive()
    }
}

impl<C: Codec> SocketeerTx<C> {
    /// Encode and send a message, awaiting confirmation that this specific send
    /// reached the socket. See [`Socketeer::send`](crate::Socketeer::send).
    /// # Errors
    /// - If the codec fails to encode the value
    /// - If the connection is closed or errored
    pub async fn send(&self, message: C::Tx) -> Result<(), Error> {
        let encoded = self.codec.encode(&message)?;
        self.send_raw(encoded).await
    }

    /// Send a raw [`Message`], awaiting confirmation.
    /// # Errors
    /// - If the connection is closed or errored
    pub async fn send_raw(&self, message: Message) -> Result<(), Error> {
        send_confirmed(&self.sender, &self.terminal_error, message).await
    }

    /// Encode and send a message fire-and-forget: applies backpressure but does
    /// not await the loop's per-send result, skipping the round-trip. Returns
    /// `Ok(())` once enqueued; a later socket-send failure surfaces via the
    /// receive stream / terminal error.
    /// # Errors
    /// - If the codec fails to encode the value
    /// - If the connection is already closed
    pub async fn send_unconfirmed(&self, message: C::Tx) -> Result<(), Error> {
        let encoded = self.codec.encode(&message)?;
        self.send_raw_unconfirmed(encoded).await
    }

    /// Send a raw [`Message`] fire-and-forget. See [`Self::send_unconfirmed`].
    /// # Errors
    /// - If the connection is already closed
    pub async fn send_raw_unconfirmed(&self, message: Message) -> Result<(), Error> {
        send_unconfirmed(&self.sender, &self.terminal_error, message).await
    }

    /// Send a graceful close frame. The receive half observes its stream
    /// ending as the signal that the connection has closed.
    /// # Errors
    /// - If the connection is already closed
    pub async fn close(&self) -> Result<(), Error> {
        send_close(&self.sender).await
    }
}

/// The receive half of a [`Socketeer`](crate::Socketeer), produced by
/// [`Socketeer::split`](crate::Socketeer::split). Implements
/// [`Stream`](futures::Stream) and can be recombined with a [`SocketeerTx`] via
/// [`reunite`](Self::reunite).
pub struct SocketeerRx<C: Codec, Handler = NoopHandler, const CHANNEL_SIZE: usize = 4>
where
    Handler: ConnectionHandler<C>,
{
    pub(crate) receiver: mpsc::Receiver<Message>,
    pub(crate) codec: C,
    pub(crate) terminal_error: TerminalError,
    pub(crate) socket_handle: JoinHandle<()>,
    pub(crate) url: Url,
    pub(crate) options: ConnectOptions,
    pub(crate) handler: Handler,
}

impl<C: Codec, Handler, const CHANNEL_SIZE: usize> std::fmt::Debug
    for SocketeerRx<C, Handler, CHANNEL_SIZE>
where
    Handler: ConnectionHandler<C>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocketeerRx")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl<C: Codec, Handler, const CHANNEL_SIZE: usize> SocketeerRx<C, Handler, CHANNEL_SIZE>
where
    Handler: ConnectionHandler<C>,
{
    /// Wait for the next decoded message. See
    /// [`Socketeer::next_message`](crate::Socketeer::next_message).
    /// # Errors
    /// - If the connection is closed or errored
    /// - If the codec fails to decode the frame
    pub async fn next_message(&mut self) -> Result<C::Rx, Error> {
        match self.receiver.recv().await {
            Some(message) => self.codec.decode(&message),
            None => Err(take_terminal_error(&self.terminal_error)),
        }
    }

    /// Wait for the next raw [`Message`] without decoding.
    /// # Errors
    /// - If the connection is closed or errored
    pub async fn next_raw_message(&mut self) -> Result<Message, Error> {
        match self.receiver.recv().await {
            Some(message) => Ok(message),
            None => Err(take_terminal_error(&self.terminal_error)),
        }
    }
}
