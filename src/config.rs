use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, http};
use url::Url;

use crate::Error;

/// Configuration options for a WebSocket connection.
///
/// Controls HTTP headers on the upgrade request, keepalive behavior,
/// and other connection-level settings. Construct via
/// [`ConnectOptions::default`] or [`ConnectOptions::builder`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ConnectOptions {
    /// Extra HTTP headers for the WebSocket upgrade request (e.g., cookies, auth tokens).
    pub(crate) extra_headers: http::HeaderMap,
    /// Idle timeout before sending a keepalive. `None` disables keepalives entirely.
    /// Defaults to 2 seconds.
    ///
    /// Keepalives also keep the connection alive while the receive buffer is
    /// full and the reader is applying backpressure; with keepalives disabled,
    /// while the receive buffer is full the loop is not reading inbound frames
    /// (so it will not observe a server close), and a consumer that stops
    /// reading entirely can leave the connection parked until it resumes — in
    /// addition to the server potentially dropping the idle connection.
    pub(crate) keepalive_interval: Option<Duration>,
    /// If set, send this message as the keepalive instead of a WebSocket Ping frame.
    /// Useful for APIs that expect a custom keepalive payload — e.g. Interactive
    /// Brokers' literal text `tic`, or a binary heartbeat in a msgpack protocol.
    pub(crate) custom_keepalive_message: Option<Message>,
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
    /// Start building a [`ConnectOptions`] from the defaults.
    #[must_use]
    pub fn builder() -> ConnectOptionsBuilder {
        ConnectOptionsBuilder {
            options: ConnectOptions::default(),
        }
    }

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

/// Builder for [`ConnectOptions`]. Obtain one with [`ConnectOptions::builder`],
/// chain setters, and finish with [`ConnectOptionsBuilder::build`].
#[derive(Clone, Debug)]
pub struct ConnectOptionsBuilder {
    options: ConnectOptions,
}

impl ConnectOptionsBuilder {
    /// Set the idle timeout before a keepalive is sent. `None` disables keepalives.
    #[must_use]
    pub fn keepalive_interval(mut self, interval: Option<Duration>) -> Self {
        self.options.keepalive_interval = interval;
        self
    }

    /// Set a single HTTP header on the WebSocket upgrade request. If a header with
    /// the same name was already set, its previous value is replaced.
    #[must_use]
    pub fn header<K: http::header::IntoHeaderName>(
        mut self,
        name: K,
        value: http::HeaderValue,
    ) -> Self {
        self.options.extra_headers.insert(name, value);
        self
    }

    /// Replace the entire set of extra HTTP headers.
    #[must_use]
    pub fn headers(mut self, headers: http::HeaderMap) -> Self {
        self.options.extra_headers = headers;
        self
    }

    /// Send this message as the keepalive instead of a WebSocket Ping frame.
    #[must_use]
    pub fn custom_keepalive_message(mut self, message: Option<Message>) -> Self {
        self.options.custom_keepalive_message = message;
        self
    }

    /// Finish building, producing the configured [`ConnectOptions`].
    #[must_use]
    pub fn build(self) -> ConnectOptions {
        self.options
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_default_matches_default() {
        let built = ConnectOptions::builder().build();
        let default = ConnectOptions::default();
        assert_eq!(built.keepalive_interval, default.keepalive_interval);
        assert_eq!(
            built.custom_keepalive_message,
            default.custom_keepalive_message
        );
        assert_eq!(built.extra_headers, default.extra_headers);
    }

    #[test]
    fn builder_sets_each_field() {
        let opts = ConnectOptions::builder()
            .keepalive_interval(None)
            .custom_keepalive_message(Some(Message::text("ping")))
            .header("x-test", "v".parse().unwrap())
            .build();
        assert_eq!(opts.keepalive_interval, None);
        assert_eq!(opts.custom_keepalive_message, Some(Message::text("ping")));
        assert_eq!(opts.extra_headers.get("x-test").unwrap(), "v");
    }

    #[test]
    fn headers_replaces_whole_map() {
        let mut map = http::HeaderMap::new();
        map.insert("a", "1".parse().unwrap());
        map.insert("b", "2".parse().unwrap());
        let opts = ConnectOptions::builder()
            .header("z", "0".parse().unwrap())
            .headers(map)
            .build();
        assert_eq!(opts.extra_headers.len(), 2);
        assert_eq!(opts.extra_headers.get("a").unwrap(), "1");
        assert!(opts.extra_headers.get("z").is_none());
    }
}
