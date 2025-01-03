use thiserror::Error;
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Error)]
pub enum Error {
    /// Url Parse Error
    #[error("Failed to parse URL: {}", 0)]
    UrlParse {
        url: String,
        #[source]
        source: url::ParseError,
    },
    #[error("Tungstenite error: {0}")]
    WebsocketError(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("Socket Closed")]
    WebsocketClosed,
    #[error("Channel Full")]
    ChannelFull,
    #[error("Unexpected Message type: {0}")]
    UnexpectedMessage(Message),
    #[error("Serialization Error: {0}")]
    SerializationError(#[from] serde_json::Error),
    #[error("Socketeer dropped without closing")]
    SocketeerDropped,
}
