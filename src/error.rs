use thiserror::Error;
use tokio_tungstenite::tungstenite::Message;

/// Error type for the Socketeer library.
/// This type is used to represent all possible external errors that can occur when using the Socketeer library.
#[derive(Debug, Error)]
pub enum Error {
    /// Url Parse Error
    #[error("Failed to parse URL: {}", 0)]
    UrlParse {
        /// The URL that failed to parse
        url: String,
        /// The source of the error, from the [URL crate](https://docs.rs/url/2.2.2/url/enum.ParseError.html)
        #[source]
        source: url::ParseError,
    },
    /// Websocket Error
    /// Error thrown by the Tungstenite library when there is an issue with the websocket connection.
    #[error("Tungstenite error: {0}")]
    WebsocketError(#[from] tokio_tungstenite::tungstenite::Error),
    /// Socketeer error when the websocket connection is closed unexpectedly.
    #[error("Socket Closed")]
    WebsocketClosed,
    /// Error thrown if a message type not handled by `socketeer` is received.
    #[error("Unexpected Message type: {0}")]
    UnexpectedMessageType(Message),
    /// Error thrown if the message received fails to serialize or deserialize.
    #[error("Serialization Error: {0}")]
    SerializationError(#[from] serde_json::Error),
    /// Error thrown if socketeer is dropped without closing the connection.
    /// This error will be removed once async destructors are stabilized.
    /// See [issue](https://github.com/rust-lang/rust/issues/126482)
    #[error("Socketeer dropped without closing")]
    SocketeerDroppedWithoutClosing,
}
