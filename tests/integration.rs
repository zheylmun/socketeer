//! End-to-end tests that drive a real `Socketeer` against the in-crate mock
//! servers over loopback TCP. These exercise only the public API, so they live
//! here as integration tests rather than inline unit tests.
#![cfg(feature = "mocking")]

use std::time::Duration;

use bytes::Bytes;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;

use socketeer::{
    Codec, ConnectOptions, ConnectionHandler, EchoControlMessage, Error, HandshakeContext,
    JsonCodec, RawCodec, Socketeer, auth_echo_server, echo_server, get_mock_address,
};

#[cfg(feature = "msgpack")]
use socketeer::{MsgPackCodec, msgpack_echo_server};

type EchoJson = JsonCodec<EchoControlMessage, EchoControlMessage>;

#[tokio::test]
async fn test_server_startup() {
    let _server_address = get_mock_address(echo_server).await;
}

#[tokio::test]
async fn test_connection() {
    let server_address = get_mock_address(echo_server).await;
    let _socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_bad_url() {
    let error: Result<Socketeer<EchoJson>, Error> = Socketeer::connect("Not a URL").await;
    assert!(matches!(error.unwrap_err(), Error::UrlParse { .. }));
}

#[tokio::test]
async fn test_send_receive() {
    let server_address = get_mock_address(echo_server).await;
    let mut socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let message = EchoControlMessage::Message("Hello".to_string());
    socketeer.send(message.clone()).await.unwrap();
    let received_message = socketeer.next_message().await.unwrap();
    assert_eq!(message, received_message);
}

#[tokio::test]
async fn test_ping_request() {
    let server_address = get_mock_address(echo_server).await;
    let mut socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let ping_request = EchoControlMessage::SendPing;
    socketeer.send(ping_request).await.unwrap();
    // The server will respond with a ping request, which Socketeer will transparently respond to
    let message = EchoControlMessage::Message("Hello".to_string());
    socketeer.send(message.clone()).await.unwrap();
    let received_message = socketeer.next_message().await.unwrap();
    assert_eq!(received_message, message);
    // We should send a ping in here
    sleep(Duration::from_millis(2200)).await;
    // Ensure everything shuts down so we exercize the ping functionality fully
    socketeer.close_connection().await.unwrap();
}

#[tokio::test]
async fn test_reconnection() {
    let server_address = get_mock_address(echo_server).await;
    let mut socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let message = EchoControlMessage::Message("Hello".to_string());
    socketeer.send(message.clone()).await.unwrap();
    let received_message = socketeer.next_message().await.unwrap();
    assert_eq!(message, received_message);
    socketeer = socketeer.reconnect().await.unwrap();
    let message = EchoControlMessage::Message("Hello".to_string());
    socketeer.send(message.clone()).await.unwrap();
    let received_message = socketeer.next_message().await.unwrap();
    assert_eq!(message, received_message);
    socketeer.close_connection().await.unwrap();
}

#[tokio::test]
async fn test_closed_socket() {
    let server_address = get_mock_address(echo_server).await;
    let mut socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let close_request = EchoControlMessage::Close;
    socketeer.send(close_request.clone()).await.unwrap();
    let response = socketeer.next_message().await;
    assert!(matches!(response.unwrap_err(), Error::WebsocketClosed));
    let send_result = socketeer.send(close_request).await;
    assert!(send_result.is_err());
    let error = send_result.unwrap_err();
    println!("Actual Error: {error:#?}");
    assert!(matches!(error, Error::WebsocketClosed));
}

#[tokio::test]
async fn test_abrupt_close_surfaces_websocket_error() {
    // When the peer drops the connection without a close handshake, the socket
    // loop terminates with a WebsocketError. That real cause must reach the
    // consumer via next_message, rather than being flattened to the generic
    // WebsocketClosed that a graceful close produces.
    let server_address = get_mock_address(echo_server).await;
    let mut socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    socketeer.send(EchoControlMessage::Abort).await.unwrap();
    let err = socketeer.next_message().await.unwrap_err();
    assert!(
        matches!(err, Error::WebsocketError(_)),
        "expected the underlying WebsocketError, got {err:?}"
    );
}

#[tokio::test]
async fn test_close_request() {
    let server_address = get_mock_address(echo_server).await;
    let socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    socketeer.close_connection().await.unwrap();
}

#[tokio::test]
async fn test_connect_with_default_options() {
    let server_address = get_mock_address(echo_server).await;
    let mut socketeer: Socketeer<EchoJson> =
        Socketeer::connect_with(&format!("ws://{server_address}"), ConnectOptions::default())
            .await
            .unwrap();
    let message = EchoControlMessage::Message("Hello".to_string());
    socketeer.send(message.clone()).await.unwrap();
    let received_message = socketeer.next_message().await.unwrap();
    assert_eq!(message, received_message);
}

#[tokio::test]
async fn test_raw_codec_message_roundtrip() {
    // Typed send/next_message round-trip when the codec is RawCodec — the
    // codec is identity, so frames pass through unchanged.
    let server_address = get_mock_address(echo_server).await;
    let mut socketeer: Socketeer<RawCodec> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let raw_text = r#"{"Message":"raw hello"}"#;
    socketeer
        .send(Message::Text(raw_text.into()))
        .await
        .unwrap();
    let received = socketeer.next_message().await.unwrap();
    assert_eq!(received, Message::Text(raw_text.into()));
}

#[tokio::test]
async fn test_disabled_keepalive() {
    let server_address = get_mock_address(echo_server).await;
    let options = ConnectOptions {
        keepalive_interval: None,
        ..ConnectOptions::default()
    };
    let mut socketeer: Socketeer<EchoJson> =
        Socketeer::connect_with(&format!("ws://{server_address}"), options)
            .await
            .unwrap();
    let message = EchoControlMessage::Message("Hello".to_string());
    socketeer.send(message.clone()).await.unwrap();
    let received_message = socketeer.next_message().await.unwrap();
    assert_eq!(message, received_message);
}

#[tokio::test]
async fn test_handler_on_connected() {
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
    struct AuthResponse {
        status: String,
    }

    struct TestAuthHandler {
        connected_count: Arc<Mutex<u32>>,
    }

    impl<C: Codec> ConnectionHandler<C> for TestAuthHandler {
        async fn on_connected(&mut self, ctx: &mut HandshakeContext<'_, C>) -> Result<(), Error> {
            ctx.send_text(r#"{"action":"auth","token":"test-token"}"#)
                .await?;
            let text = ctx.recv_text().await?;
            let response: AuthResponse = serde_json::from_str(&text).unwrap();
            assert_eq!(response.status, "authenticated");
            let mut count = self.connected_count.lock().await;
            *count += 1;
            Ok(())
        }
    }

    let connected_count = Arc::new(Mutex::new(0u32));
    let handler = TestAuthHandler {
        connected_count: connected_count.clone(),
    };

    let server_address = get_mock_address(auth_echo_server).await;
    let mut socketeer: Socketeer<EchoJson, TestAuthHandler> = Socketeer::connect_with_codec(
        &format!("ws://{server_address}"),
        ConnectOptions::default(),
        JsonCodec::new(),
        handler,
    )
    .await
    .unwrap();

    assert_eq!(*connected_count.lock().await, 1);

    let message = EchoControlMessage::Message("after auth".to_string());
    socketeer.send(message.clone()).await.unwrap();
    let received = socketeer.next_message().await.unwrap();
    assert_eq!(message, received);
}

#[tokio::test]
async fn test_handler_reconnect() {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    struct ReconnectHandler {
        connected_count: Arc<Mutex<u32>>,
        disconnected_count: Arc<Mutex<u32>>,
    }

    impl<C: Codec> ConnectionHandler<C> for ReconnectHandler {
        async fn on_connected(&mut self, ctx: &mut HandshakeContext<'_, C>) -> Result<(), Error> {
            ctx.send_text(r#"{"action":"auth","token":"test-token"}"#)
                .await?;
            let _response = ctx.recv_text().await?;
            let mut count = self.connected_count.lock().await;
            *count += 1;
            Ok(())
        }

        async fn on_disconnected(&mut self) {
            let mut count = self.disconnected_count.lock().await;
            *count += 1;
        }
    }

    let connected_count = Arc::new(Mutex::new(0u32));
    let disconnected_count = Arc::new(Mutex::new(0u32));
    let handler = ReconnectHandler {
        connected_count: connected_count.clone(),
        disconnected_count: disconnected_count.clone(),
    };

    let server_address = get_mock_address(auth_echo_server).await;
    let mut socketeer = Socketeer::<EchoJson, ReconnectHandler>::connect_with_codec(
        &format!("ws://{server_address}"),
        ConnectOptions::default(),
        JsonCodec::new(),
        handler,
    )
    .await
    .unwrap();

    assert_eq!(*connected_count.lock().await, 1);
    assert_eq!(*disconnected_count.lock().await, 0);

    // Send a message to verify connection works
    let message = EchoControlMessage::Message("before reconnect".to_string());
    socketeer.send(message.clone()).await.unwrap();
    let received = socketeer.next_message().await.unwrap();
    assert_eq!(message, received);

    // Reconnect — handler should fire again
    socketeer = socketeer.reconnect().await.unwrap();

    assert_eq!(*connected_count.lock().await, 2);
    assert_eq!(*disconnected_count.lock().await, 1);

    // Verify connection still works after reconnect
    let message = EchoControlMessage::Message("after reconnect".to_string());
    socketeer.send(message.clone()).await.unwrap();
    let received = socketeer.next_message().await.unwrap();
    assert_eq!(message, received);

    socketeer.close_connection().await.unwrap();
}

#[cfg(feature = "msgpack")]
#[tokio::test]
async fn test_msgpack_send_receive() {
    type EchoMsgPack = MsgPackCodec<EchoControlMessage, EchoControlMessage>;

    let server_address = get_mock_address(msgpack_echo_server).await;
    let mut socketeer: Socketeer<EchoMsgPack> =
        Socketeer::connect(&format!("ws://{server_address}"))
            .await
            .unwrap();
    let message = EchoControlMessage::Message("msgpack hello".to_string());
    socketeer.send(message.clone()).await.unwrap();
    let received = socketeer.next_message().await.unwrap();
    assert_eq!(message, received);
    socketeer.close_connection().await.unwrap();
}

#[tokio::test]
async fn test_handler_uses_codec_driven_send_recv() {
    // Exercises HandshakeContext::send / recv (the codec-driven path).
    // Other handler tests only cover the raw send_text / recv_text helpers.
    struct TypedHandshakeHandler;

    impl ConnectionHandler<EchoJson> for TypedHandshakeHandler {
        async fn on_connected(
            &mut self,
            ctx: &mut HandshakeContext<'_, EchoJson>,
        ) -> Result<(), Error> {
            ctx.send(&EchoControlMessage::Message("handshake".into()))
                .await?;
            let echoed = ctx.recv().await?;
            assert_eq!(echoed, EchoControlMessage::Message("handshake".into()));
            Ok(())
        }
    }

    let server_address = get_mock_address(echo_server).await;
    let mut socketeer: Socketeer<EchoJson, TypedHandshakeHandler> = Socketeer::connect_with_codec(
        &format!("ws://{server_address}"),
        ConnectOptions::default(),
        JsonCodec::new(),
        TypedHandshakeHandler,
    )
    .await
    .unwrap();

    // Confirm normal traffic still flows after the typed handshake.
    let message = EchoControlMessage::Message("after handshake".into());
    socketeer.send(message.clone()).await.unwrap();
    assert_eq!(socketeer.next_message().await.unwrap(), message);
    socketeer.close_connection().await.unwrap();
}

#[tokio::test]
async fn test_handshake_recv_close_with_raw_codec() {
    // Regression: with RawCodec, recv_raw returns Ok(Message::Close(_)) and
    // RawCodec::decode is the identity, so a peer-initiated close used to
    // surface as Ok(Close) instead of Err(WebsocketClosed). recv must
    // intercept Close before delegating to the codec.
    struct CloseExpecting;

    impl ConnectionHandler<RawCodec> for CloseExpecting {
        async fn on_connected(
            &mut self,
            ctx: &mut HandshakeContext<'_, RawCodec>,
        ) -> Result<(), Error> {
            // Ask the echo server to close (JSON unit-variant for EchoControlMessage::Close).
            ctx.send(&Message::Text(r#""Close""#.into())).await?;
            let err = ctx.recv().await.unwrap_err();
            assert!(matches!(err, Error::WebsocketClosed));
            Ok(())
        }
    }

    let server_address = get_mock_address(echo_server).await;
    let _socketeer: Socketeer<RawCodec, CloseExpecting> = Socketeer::connect_with_codec(
        &format!("ws://{server_address}"),
        ConnectOptions::default(),
        RawCodec::new(),
        CloseExpecting,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn test_extra_headers_used() {
    // Cover ConnectOptions::build_request's loop body that copies
    // `extra_headers` onto the upgrade request.
    let server_address = get_mock_address(echo_server).await;
    let mut headers = tokio_tungstenite::tungstenite::http::HeaderMap::new();
    headers.insert("X-Test-Header", "socketeer".parse().unwrap());
    let options = ConnectOptions {
        extra_headers: headers,
        ..ConnectOptions::default()
    };
    let mut socketeer: Socketeer<EchoJson> =
        Socketeer::connect_with(&format!("ws://{server_address}"), options)
            .await
            .unwrap();
    let message = EchoControlMessage::Message("hi".into());
    socketeer.send(message.clone()).await.unwrap();
    assert_eq!(socketeer.next_message().await.unwrap(), message);
    socketeer.close_connection().await.unwrap();
}

#[tokio::test]
async fn test_auth_handler_bad_token() {
    // Covers auth_echo_server's bad-token branch (sends {"status":"error"}
    // and shuts down). The handler observes the error response, returns
    // Ok, then a subsequent send fails because the server has closed.
    struct BadTokenHandler;

    impl<C: Codec> ConnectionHandler<C> for BadTokenHandler {
        async fn on_connected(&mut self, ctx: &mut HandshakeContext<'_, C>) -> Result<(), Error> {
            ctx.send_text(r#"{"action":"auth","token":"WRONG"}"#)
                .await?;
            let resp = ctx.recv_text().await?;
            assert!(resp.contains("error"));
            Ok(())
        }
    }

    let server_address = get_mock_address(auth_echo_server).await;
    let _socketeer: Socketeer<EchoJson, BadTokenHandler> = Socketeer::connect_with_codec(
        &format!("ws://{server_address}"),
        ConnectOptions::default(),
        JsonCodec::new(),
        BadTokenHandler,
    )
    .await
    .unwrap();
}

#[cfg(feature = "msgpack")]
#[tokio::test]
async fn test_msgpack_send_ping() {
    // Covers the SendPing arm of msgpack_echo_server.
    type EchoMsgPack = MsgPackCodec<EchoControlMessage, EchoControlMessage>;

    let server_address = get_mock_address(msgpack_echo_server).await;
    let mut socketeer: Socketeer<EchoMsgPack> =
        Socketeer::connect(&format!("ws://{server_address}"))
            .await
            .unwrap();
    socketeer.send(EchoControlMessage::SendPing).await.unwrap();
    // Server replies with a Ping; Socketeer auto-Pongs. Round-trip a real
    // message to confirm the connection is still alive.
    let message = EchoControlMessage::Message("after ping".into());
    socketeer.send(message.clone()).await.unwrap();
    assert_eq!(socketeer.next_message().await.unwrap(), message);
    socketeer.close_connection().await.unwrap();
}

#[cfg(feature = "msgpack")]
#[tokio::test]
async fn test_msgpack_close_request() {
    // Covers the Close arm of msgpack_echo_server.
    type EchoMsgPack = MsgPackCodec<EchoControlMessage, EchoControlMessage>;

    let server_address = get_mock_address(msgpack_echo_server).await;
    let mut socketeer: Socketeer<EchoMsgPack> =
        Socketeer::connect(&format!("ws://{server_address}"))
            .await
            .unwrap();
    socketeer.send(EchoControlMessage::Close).await.unwrap();
    let result = socketeer.next_message().await;
    assert!(matches!(result.unwrap_err(), Error::WebsocketClosed));
}

#[tokio::test]
async fn test_socketeer_debug_format() {
    let server_address = get_mock_address(echo_server).await;
    let socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let formatted = format!("{socketeer:?}");
    assert!(formatted.starts_with("Socketeer"));
    assert!(formatted.contains("url"));
}

#[tokio::test]
async fn test_send_raw_next_raw_message() {
    // Cover the raw send/receive escape hatches on a typed (non-RawCodec)
    // connection: send_raw bypasses encoding, next_raw_message bypasses
    // decoding, so we can speak frames the codec wouldn't otherwise
    // produce or accept.
    let server_address = get_mock_address(echo_server).await;
    let mut socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let raw_text = r#"{"Message":"raw recv"}"#;
    socketeer
        .send_raw(Message::Text(raw_text.into()))
        .await
        .unwrap();
    let frame = socketeer.next_raw_message().await.unwrap();
    assert_eq!(frame, Message::Text(raw_text.into()));
    socketeer.close_connection().await.unwrap();
}

#[cfg(feature = "msgpack")]
#[tokio::test]
async fn test_handshake_send_binary_recv_raw() {
    // Cover HandshakeContext::send_binary by sending a pre-encoded
    // msgpack frame from on_connected and reading the binary echo back
    // via recv_raw.
    struct BinaryHandshake;

    type EchoMsgPack = MsgPackCodec<EchoControlMessage, EchoControlMessage>;

    impl ConnectionHandler<EchoMsgPack> for BinaryHandshake {
        async fn on_connected(
            &mut self,
            ctx: &mut HandshakeContext<'_, EchoMsgPack>,
        ) -> Result<(), Error> {
            let payload =
                rmp_serde::to_vec_named(&EchoControlMessage::Message("binary".into())).unwrap();
            ctx.send_binary(payload).await?;
            let echo = ctx.recv_raw().await?;
            assert!(matches!(echo, Message::Binary(_)));
            Ok(())
        }
    }

    let server_address = get_mock_address(msgpack_echo_server).await;
    let socketeer: Socketeer<EchoMsgPack, BinaryHandshake> = Socketeer::connect_with_codec(
        &format!("ws://{server_address}"),
        ConnectOptions::default(),
        MsgPackCodec::new(),
        BinaryHandshake,
    )
    .await
    .unwrap();
    socketeer.close_connection().await.unwrap();
}

#[cfg(feature = "msgpack")]
#[tokio::test]
async fn test_handshake_recv_text_rejects_binary() {
    // Cover the non-Text branch of HandshakeContext::recv_text by pointing
    // it at a server that only speaks binary frames.
    struct ExpectsTextOnBinary;

    type EchoMsgPack = MsgPackCodec<EchoControlMessage, EchoControlMessage>;

    impl ConnectionHandler<EchoMsgPack> for ExpectsTextOnBinary {
        async fn on_connected(
            &mut self,
            ctx: &mut HandshakeContext<'_, EchoMsgPack>,
        ) -> Result<(), Error> {
            let payload =
                rmp_serde::to_vec_named(&EchoControlMessage::Message("hi".into())).unwrap();
            ctx.send_binary(payload).await?;
            // recv_text must reject the echoed Binary frame.
            let err = ctx.recv_text().await.unwrap_err();
            assert!(matches!(err, Error::UnexpectedMessageType(_)));
            Ok(())
        }
    }

    let server_address = get_mock_address(msgpack_echo_server).await;
    let socketeer: Socketeer<EchoMsgPack, ExpectsTextOnBinary> = Socketeer::connect_with_codec(
        &format!("ws://{server_address}"),
        ConnectOptions::default(),
        MsgPackCodec::new(),
        ExpectsTextOnBinary,
    )
    .await
    .unwrap();
    socketeer.close_connection().await.unwrap();
}

#[tokio::test]
async fn test_binary_custom_keepalive() {
    // The widening of custom_keepalive_message from Option<String> to
    // Option<Message> is otherwise unexercised. echo_server silently
    // ignores Binary frames, so the receive queue stays clean and we can
    // verify the connection survives a binary keepalive cycle.
    let server_address = get_mock_address(echo_server).await;
    let options = ConnectOptions {
        keepalive_interval: Some(Duration::from_millis(100)),
        custom_keepalive_message: Some(Message::Binary(Bytes::from_static(b"keepalive"))),
        ..ConnectOptions::default()
    };
    let mut socketeer: Socketeer<EchoJson> =
        Socketeer::connect_with(&format!("ws://{server_address}"), options)
            .await
            .unwrap();

    // Wait long enough for at least a couple of keepalive ticks to fire.
    sleep(Duration::from_millis(350)).await;

    let message = EchoControlMessage::Message("post-keepalive".into());
    socketeer.send(message.clone()).await.unwrap();
    assert_eq!(socketeer.next_message().await.unwrap(), message);
    socketeer.close_connection().await.unwrap();
}
