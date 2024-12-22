use thiserror::Error;
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Tungstenite error: {0}")]
    WebsocketError(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("Socket Closed")]
    WebSocketClosed,
    #[error("Channel Full")]
    ChannelFull,
    #[error("Unexpected Message type: {0}")]
    UnexpectedMessage(Message),
    #[error("Serialization Error: {0}")]
    SerializationError(#[from] serde_json::Error),
}
