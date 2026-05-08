use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, http};
use url::Url;

use crate::Error;

/// Configuration options for a WebSocket connection.
///
/// Controls HTTP headers on the upgrade request, keepalive behavior,
/// and other connection-level settings.
#[derive(Clone, Debug)]
pub struct ConnectOptions {
    /// Extra HTTP headers for the WebSocket upgrade request (e.g., cookies, auth tokens).
    pub extra_headers: http::HeaderMap,
    /// Idle timeout before sending a keepalive. `None` disables keepalives entirely.
    /// Defaults to 2 seconds.
    pub keepalive_interval: Option<Duration>,
    /// If set, send this message as the keepalive instead of a WebSocket Ping frame.
    /// Useful for APIs that expect a custom keepalive payload — e.g. Interactive
    /// Brokers' literal text `tic`, or a binary heartbeat in a msgpack protocol.
    pub custom_keepalive_message: Option<Message>,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            extra_headers: http::HeaderMap::new(),
            keepalive_interval: Some(Duration::from_secs(2)),
            custom_keepalive_message: None,
        }
    }
}

impl ConnectOptions {
    /// Build an HTTP request from the URL and configured headers.
    ///
    /// Uses tungstenite's `IntoClientRequest` to generate a properly formed
    /// WebSocket upgrade request, then appends any extra headers.
    pub(crate) fn build_request(&self, url: &Url) -> Result<http::Request<()>, Error> {
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(Error::WebsocketError)?;
        for (key, value) in &self.extra_headers {
            request.headers_mut().insert(key, value.clone());
        }
        Ok(request)
    }
}
