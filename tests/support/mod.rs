//! Shared helpers for the integration tests.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use socketeer::{Bytes, EchoControlMessage, Message, WebSocketStreamType, tungstenite};
use tokio::time::timeout;

/// Probe server for the backpressure tests.
///
/// On connect it immediately sends a burst of five `EchoControlMessage::Message`
/// frames (`"burst-0"`..`"burst-4"`), then waits up to 500ms for a client
/// keepalive [`Message::Ping`]. It sends a final `"alive"` frame ONLY if that
/// ping arrives — so a client whose socket loop is stalled by a full receive
/// buffer (and therefore cannot send keepalives) never receives the marker.
/// After that it pongs and drains until the client closes.
///
/// # Errors
/// - If sending a burst frame, the marker, or a pong fails
/// # Panics
/// - If serializing an [`EchoControlMessage`] to JSON fails (infallible in practice)
pub async fn backpressure_probe_server(
    ws: WebSocketStreamType,
) -> Result<bool, tungstenite::Error> {
    let (mut sink, mut stream) = ws.split();

    for i in 0..5u32 {
        let msg = EchoControlMessage::Message(format!("burst-{i}"));
        let text = serde_json::to_string(&msg).unwrap();
        sink.send(Message::Text(text.into())).await?;
    }

    let got_ping = timeout(Duration::from_millis(500), async {
        while let Some(Ok(frame)) = stream.next().await {
            if matches!(frame, Message::Ping(_)) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    if got_ping {
        let alive = EchoControlMessage::Message("alive".to_string());
        let text = serde_json::to_string(&alive).unwrap();
        sink.send(Message::Text(text.into())).await?;
    }

    while let Some(Ok(frame)) = stream.next().await {
        match frame {
            Message::Ping(_) => sink.send(Message::Pong(Bytes::new())).await?,
            Message::Close(_) => {
                sink.close().await?;
                break;
            }
            _ => {}
        }
    }
    Ok(true)
}
