//! Codec abstraction for serializing outgoing messages and deserializing incoming
//! messages on a [`crate::Socketeer`] connection.
//!
//! A [`Codec`] owns the `Tx` and `Rx` types for a connection and decides how each
//! is mapped to a WebSocket [`Message`]. This lets a single connection use different
//! framing on the send and receive sides — useful for protocols like Interactive
//! Brokers' Client Portal stream, which sends prefixed text strings (`smd+...`) but
//! receives JSON.
//!
//! Three stock codecs are provided:
//!
//! - [`JsonCodec`] — `serde_json`, sends as `Message::Text`. Decodes Text or Binary.
//! - [`MsgPackCodec`] — `rmp-serde`, sends as `Message::Binary`. Behind the
//!   `msgpack` cargo feature.
//! - [`RawCodec`] — `Tx = Rx = Message`, no transformation. Useful when you want
//!   the typed [`crate::Socketeer::send`] / [`crate::Socketeer::next_message`]
//!   path but don't want any (de)serialization.
//!
//! Custom codecs implement the [`Codec`] trait directly.

use std::marker::PhantomData;

use serde::{Serialize, de::DeserializeOwned};
use tokio_tungstenite::tungstenite::Message;

use crate::Error;

/// Encodes outgoing values into WebSocket messages and decodes incoming messages
/// into typed values.
///
/// The `Tx` and `Rx` associated types are the values surfaced to users via
/// [`crate::Socketeer::send`] and [`crate::Socketeer::next_message`]. A codec is
/// free to use the same type on both sides (most common) or to use different
/// types when a protocol's send and receive shapes differ.
pub trait Codec: Send + Sync + 'static {
    /// The type accepted by [`crate::Socketeer::send`].
    type Tx;
    /// The type returned by [`crate::Socketeer::next_message`].
    type Rx;

    /// Encode a value of [`Self::Tx`] into a WebSocket [`Message`].
    /// # Errors
    /// - If the value cannot be encoded.
    fn encode(&self, value: &Self::Tx) -> Result<Message, Error>;

    /// Decode a WebSocket [`Message`] into a value of [`Self::Rx`].
    /// # Errors
    /// - If the message cannot be decoded.
    fn decode(&self, frame: &Message) -> Result<Self::Rx, Error>;
}

/// JSON codec backed by `serde_json`.
///
/// Encodes outgoing values as [`Message::Text`]. Decodes incoming `Text`
/// frames and, for compatibility with servers that send JSON in binary frames,
/// also accepts [`Message::Binary`].
pub struct JsonCodec<Rx, Tx>(PhantomData<fn() -> (Rx, Tx)>);

impl<Rx, Tx> JsonCodec<Rx, Tx> {
    /// Construct a new [`JsonCodec`].
    #[must_use]
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<Rx, Tx> Default for JsonCodec<Rx, Tx> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Rx, Tx> std::fmt::Debug for JsonCodec<Rx, Tx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JsonCodec")
    }
}

impl<Rx, Tx> Clone for JsonCodec<Rx, Tx> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Rx, Tx> Copy for JsonCodec<Rx, Tx> {}

impl<Rx, Tx> Codec for JsonCodec<Rx, Tx>
where
    Rx: DeserializeOwned + Send + 'static,
    Tx: Serialize + Send + 'static,
{
    type Tx = Tx;
    type Rx = Rx;

    fn encode(&self, value: &Self::Tx) -> Result<Message, Error> {
        let text = serde_json::to_string(value).map_err(|e| Error::Codec(Box::new(e)))?;
        Ok(Message::Text(text.into()))
    }

    fn decode(&self, frame: &Message) -> Result<Self::Rx, Error> {
        match frame {
            Message::Text(text) => {
                serde_json::from_str(text).map_err(|e| Error::Codec(Box::new(e)))
            }
            Message::Binary(bytes) => {
                serde_json::from_slice(bytes).map_err(|e| Error::Codec(Box::new(e)))
            }
            other => Err(Error::UnexpectedMessageType(Box::new(other.clone()))),
        }
    }
}

/// `MessagePack` codec backed by `rmp-serde`.
///
/// Encodes outgoing values as [`Message::Binary`]. Decodes only `Binary` frames.
#[cfg(feature = "msgpack")]
pub struct MsgPackCodec<Rx, Tx>(PhantomData<fn() -> (Rx, Tx)>);

#[cfg(feature = "msgpack")]
impl<Rx, Tx> MsgPackCodec<Rx, Tx> {
    /// Construct a new [`MsgPackCodec`].
    #[must_use]
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

#[cfg(feature = "msgpack")]
impl<Rx, Tx> Default for MsgPackCodec<Rx, Tx> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "msgpack")]
impl<Rx, Tx> std::fmt::Debug for MsgPackCodec<Rx, Tx> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MsgPackCodec")
    }
}

#[cfg(feature = "msgpack")]
impl<Rx, Tx> Clone for MsgPackCodec<Rx, Tx> {
    fn clone(&self) -> Self {
        *self
    }
}

#[cfg(feature = "msgpack")]
impl<Rx, Tx> Copy for MsgPackCodec<Rx, Tx> {}

#[cfg(feature = "msgpack")]
impl<Rx, Tx> Codec for MsgPackCodec<Rx, Tx>
where
    Rx: DeserializeOwned + Send + 'static,
    Tx: Serialize + Send + 'static,
{
    type Tx = Tx;
    type Rx = Rx;

    fn encode(&self, value: &Self::Tx) -> Result<Message, Error> {
        let bytes = rmp_serde::to_vec_named(value).map_err(|e| Error::Codec(Box::new(e)))?;
        Ok(Message::Binary(bytes.into()))
    }

    fn decode(&self, frame: &Message) -> Result<Self::Rx, Error> {
        match frame {
            Message::Binary(bytes) => {
                rmp_serde::from_slice(bytes).map_err(|e| Error::Codec(Box::new(e)))
            }
            other => Err(Error::UnexpectedMessageType(Box::new(other.clone()))),
        }
    }
}

/// Identity codec — `Tx` and `Rx` are both [`Message`], no (de)serialization.
///
/// Useful when you want to drive the typed [`crate::Socketeer::send`] /
/// [`crate::Socketeer::next_message`] API without any encoding step, e.g. for
/// protocols where you assemble frame bodies by hand.
#[derive(Debug, Default, Clone, Copy)]
pub struct RawCodec;

impl RawCodec {
    /// Construct a new [`RawCodec`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Codec for RawCodec {
    type Tx = Message;
    type Rx = Message;

    fn encode(&self, value: &Self::Tx) -> Result<Message, Error> {
        Ok(value.clone())
    }

    fn decode(&self, frame: &Message) -> Result<Self::Rx, Error> {
        Ok(frame.clone())
    }
}
