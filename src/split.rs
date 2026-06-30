//! Owned, splittable handles for concurrent send/receive use.
//!
//! [`Socketeer::split`](crate::Socketeer::split) divides a connected client
//! into a cloneable [`SocketeerTx`] send half and a [`SocketeerRx`] receive
//! half that can move into separate tasks. [`SocketeerRx::reunite`] recombines
//! them to regain the full [`Socketeer`](crate::Socketeer) handle.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::Stream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::socket_loop::{
    TerminalError, TxChannelPayload, poll_recv_raw, recv_raw, send_close, send_confirmed,
    send_unconfirmed,
};
use crate::{Codec, ConnectOptions, ConnectionHandler, Error, NoopHandler, Socketeer};

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
pub struct SocketeerRx<C: Codec, Handler = NoopHandler, const CHANNEL_SIZE: usize = 256>
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
        let message = recv_raw(&mut self.receiver, &self.terminal_error).await?;
        self.codec.decode(&message)
    }

    /// Wait for the next raw [`Message`] without decoding.
    /// # Errors
    /// - If the connection is closed or errored
    pub async fn next_raw_message(&mut self) -> Result<Message, Error> {
        recv_raw(&mut self.receiver, &self.terminal_error).await
    }

    /// Recombine with a [`SocketeerTx`] to restore the full
    /// [`Socketeer`](crate::Socketeer) handle (and thus `close_connection` /
    /// `reconnect`).
    ///
    /// # Errors
    /// Returns [`ReuniteError`] (carrying both halves back) if `tx` did not come
    /// from the same [`split`](crate::Socketeer::split) as `self`.
    // ReuniteError intentionally carries both halves back to the caller; boxing
    // it would change the public API and force callers to dereference to destructure.
    #[allow(clippy::result_large_err)]
    pub fn reunite(
        self,
        tx: SocketeerTx<C>,
    ) -> Result<Socketeer<C, Handler, CHANNEL_SIZE>, ReuniteError<C, Handler, CHANNEL_SIZE>> {
        if Arc::ptr_eq(&self.terminal_error, &tx.terminal_error) {
            Ok(Socketeer::from_parts(
                self.url,
                self.options,
                self.codec,
                self.handler,
                self.receiver,
                tx.sender,
                self.socket_handle,
                self.terminal_error,
            ))
        } else {
            Err(ReuniteError { tx, rx: self })
        }
    }
}

/// Error returned by [`SocketeerRx::reunite`] when the two halves did not come
/// from the same [`Socketeer::split`](crate::Socketeer::split). Carries both
/// halves back so the caller can recover them.
pub struct ReuniteError<C: Codec, Handler = NoopHandler, const CHANNEL_SIZE: usize = 256>
where
    Handler: ConnectionHandler<C>,
{
    /// The send half passed to `reunite`.
    pub tx: SocketeerTx<C>,
    /// The receive half `reunite` was called on.
    pub rx: SocketeerRx<C, Handler, CHANNEL_SIZE>,
}

impl<C: Codec, Handler, const CHANNEL_SIZE: usize> std::fmt::Debug
    for ReuniteError<C, Handler, CHANNEL_SIZE>
where
    Handler: ConnectionHandler<C>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReuniteError").finish_non_exhaustive()
    }
}

impl<C: Codec, Handler, const CHANNEL_SIZE: usize> std::fmt::Display
    for ReuniteError<C, Handler, CHANNEL_SIZE>
where
    Handler: ConnectionHandler<C>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tried to reunite a SocketeerTx and SocketeerRx from different connections")
    }
}

impl<C: Codec, Handler, const CHANNEL_SIZE: usize> std::error::Error
    for ReuniteError<C, Handler, CHANNEL_SIZE>
where
    Handler: ConnectionHandler<C>,
{
}

impl<C: Codec + Unpin, Handler, const CHANNEL_SIZE: usize> Stream
    for SocketeerRx<C, Handler, CHANNEL_SIZE>
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
