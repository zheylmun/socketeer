//! End-to-end tests that drive a real `Socketeer` against the in-crate mock
//! servers over loopback TCP. These exercise only the public API, so they live
//! here as integration tests rather than inline unit tests.
#![cfg(feature = "mocking")]

use std::time::Duration;

use bytes::Bytes;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message;

use futures::StreamExt;
use socketeer::{
    Codec, ConnectOptions, ConnectionHandler, EchoControlMessage, Error, HandshakeContext,
    JsonCodec, NoopHandler, RawCodec, Socketeer, auth_echo_server, backpressure_probe_server,
    echo_server, get_mock_address,
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
    let options = ConnectOptions::builder().keepalive_interval(None).build();
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
    let options = ConnectOptions::builder()
        .header("X-Test-Header", "socketeer".parse().unwrap())
        .build();
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
async fn test_split_round_trip() {
    let server_address = get_mock_address(echo_server).await;
    let socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let (tx, mut rx) = socketeer.split();
    let message = EchoControlMessage::Message("split hello".into());
    tx.send(message.clone()).await.unwrap();
    assert_eq!(rx.next_message().await.unwrap(), message);
    tx.close().await.unwrap();
}

#[tokio::test]
async fn test_split_concurrent_sends_from_clones() {
    // Multiple cloned Tx handles send concurrently; all deliveries observed.
    use std::collections::HashSet;

    let server_address = get_mock_address(echo_server).await;
    let socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let (tx, mut rx) = socketeer.split();
    let tx1 = tx.clone();
    let tx2 = tx.clone();

    let payload1 = "concurrent-a".to_string();
    let payload2 = "concurrent-b".to_string();

    let msg1 = EchoControlMessage::Message(payload1.clone());
    let msg2 = EchoControlMessage::Message(payload2.clone());

    // Spawn two independent tasks, each owning a clone; SocketeerTx is Send + 'static.
    let h1 = tokio::spawn(async move { tx1.send(msg1).await.unwrap() });
    let h2 = tokio::spawn(async move { tx2.send(msg2).await.unwrap() });

    h1.await.unwrap();
    h2.await.unwrap();

    // Drain exactly two echoed messages; order across senders is unspecified.
    let r1 = rx.next_message().await.unwrap();
    let r2 = rx.next_message().await.unwrap();

    let received: HashSet<String> = [r1, r2]
        .into_iter()
        .map(|m| match m {
            EchoControlMessage::Message(s) => s,
            other => panic!("unexpected echo: {other:?}"),
        })
        .collect();
    let expected: HashSet<String> = [payload1, payload2].into_iter().collect();
    assert_eq!(received, expected);

    tx.close().await.unwrap();
}

#[tokio::test]
async fn test_tx_close_ends_receive() {
    let server_address = get_mock_address(echo_server).await;
    let socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let (tx, mut rx) = socketeer.split();
    tx.close().await.unwrap();
    assert!(matches!(
        rx.next_message().await.unwrap_err(),
        Error::WebsocketClosed
    ));
}

#[tokio::test]
async fn test_send_unconfirmed_delivers() {
    let server_address = get_mock_address(echo_server).await;
    let socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let (tx, mut rx) = socketeer.split();
    let message = EchoControlMessage::Message("unconfirmed".into());
    // Fire-and-forget: returns once enqueued, no round-trip.
    tx.send_unconfirmed(message.clone()).await.unwrap();
    // The echo server still delivers it.
    assert_eq!(rx.next_message().await.unwrap(), message);
    // Liveness: a confirmed send still round-trips afterward.
    let confirmed = EchoControlMessage::Message("after".into());
    tx.send(confirmed.clone()).await.unwrap();
    assert_eq!(rx.next_message().await.unwrap(), confirmed);
    tx.close().await.unwrap();
}

#[tokio::test]
async fn test_rx_stream_round_trip() {
    let server_address = get_mock_address(echo_server).await;
    let socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let (tx, mut rx) = socketeer.split();
    let message = EchoControlMessage::Message("stream hello".into());
    tx.send(message.clone()).await.unwrap();
    let item = rx.next().await.unwrap().unwrap();
    assert_eq!(item, message);
    tx.close().await.unwrap();
}

#[tokio::test]
async fn test_rx_stream_abrupt_close_yields_error_then_ends() {
    let server_address = get_mock_address(echo_server).await;
    let socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let (tx, mut rx) = socketeer.split();
    tx.send(EchoControlMessage::Abort).await.unwrap();
    let err = rx.next().await.unwrap().unwrap_err();
    assert!(
        matches!(err, Error::WebsocketError(_)),
        "expected WebsocketError, got {err:?}"
    );
    assert!(rx.next().await.is_none(), "stream must end after the error");
}

#[tokio::test]
async fn test_rx_stream_graceful_close_ends_cleanly() {
    let server_address = get_mock_address(echo_server).await;
    let socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let (tx, mut rx) = socketeer.split();
    tx.close().await.unwrap();
    // Graceful close records no cause, so the stream just ends.
    assert!(rx.next().await.is_none());
}

#[tokio::test]
async fn test_reunite_then_reconnect() {
    let server_address = get_mock_address(echo_server).await;
    let socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let (tx, rx) = socketeer.split();
    let socketeer = rx.reunite(tx).expect("matching halves reunite");
    // Full handle restored: reconnect works.
    let mut socketeer = socketeer.reconnect().await.unwrap();
    let message = EchoControlMessage::Message("after reunite".into());
    socketeer.send(message.clone()).await.unwrap();
    assert_eq!(socketeer.next_message().await.unwrap(), message);
    socketeer.close_connection().await.unwrap();
}

#[tokio::test]
async fn test_reunite_mismatch_returns_halves() {
    // Two separate servers: the mock server's accept loop is serial (it blocks
    // in echo_server until the connection closes), so two concurrent
    // connections must target two addresses or the second connect hangs.
    let addr_a = get_mock_address(echo_server).await;
    let addr_b = get_mock_address(echo_server).await;
    let sock_a: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{addr_a}")).await.unwrap();
    let sock_b: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{addr_b}")).await.unwrap();
    let (tx_a, rx_a) = sock_a.split();
    let (tx_b, rx_b) = sock_b.split();
    // Mismatched halves: rx from A, tx from B.
    let err = rx_a
        .reunite(tx_b)
        .expect_err("mismatched halves must not reunite");
    // Exercise Display and Error trait impls before consuming err by value.
    assert!(!err.to_string().is_empty());
    let _: &dyn std::error::Error = &err;
    let socketeer::ReuniteError { tx: tx_b, rx: rx_a } = err;
    // Both halves survive and reunite with their correct partners.
    let sock_a = rx_a.reunite(tx_a).expect("correct halves reunite");
    let sock_b = rx_b.reunite(tx_b).expect("correct halves reunite");
    sock_a.close_connection().await.unwrap();
    sock_b.close_connection().await.unwrap();
}

#[tokio::test]
async fn test_socketeer_as_stream() {
    let server_address = get_mock_address(echo_server).await;
    let mut socketeer: Socketeer<EchoJson> = Socketeer::connect(&format!("ws://{server_address}"))
        .await
        .unwrap();
    let message = EchoControlMessage::Message("direct stream".into());
    socketeer.send(message.clone()).await.unwrap();
    let item = socketeer.next().await.unwrap().unwrap();
    assert_eq!(item, message);
}

#[tokio::test]
async fn test_backpressure_probe_server_delivers_burst_and_marker() {
    // Large buffer (16) > burst (5): no backpressure. Validates the probe
    // server — the client buffers the burst, goes idle, and its keepalive ping
    // prompts the "alive" marker. Passes on the unmodified loop.
    let server_address = get_mock_address(backpressure_probe_server).await;
    let options = ConnectOptions::builder()
        .keepalive_interval(Some(Duration::from_millis(100)))
        .build();
    let mut socketeer: Socketeer<EchoJson, NoopHandler, 16> =
        Socketeer::connect_with(&format!("ws://{server_address}"), options)
            .await
            .unwrap();

    let mut received = Vec::new();
    for _ in 0..5 {
        match socketeer.next_message().await.unwrap() {
            EchoControlMessage::Message(s) => received.push(s),
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(
        received,
        vec!["burst-0", "burst-1", "burst-2", "burst-3", "burst-4"]
    );

    let marker = socketeer.next_message().await.unwrap();
    assert_eq!(marker, EchoControlMessage::Message("alive".to_string()));

    socketeer.close_connection().await.unwrap();
}

#[tokio::test]
async fn test_binary_custom_keepalive() {
    // The widening of custom_keepalive_message from Option<String> to
    // Option<Message> is otherwise unexercised. echo_server silently
    // ignores Binary frames, so the receive queue stays clean and we can
    // verify the connection survives a binary keepalive cycle.
    let server_address = get_mock_address(echo_server).await;
    let options = ConnectOptions::builder()
        .keepalive_interval(Some(Duration::from_millis(100)))
        .custom_keepalive_message(Some(Message::Binary(Bytes::from_static(b"keepalive"))))
        .build();
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

#[tokio::test]
async fn test_backpressure_keeps_protocol_alive() {
    // Small buffer (2) < burst (5) forces backpressure. The client pauses past
    // the keepalive interval before draining; the "alive" marker proves the
    // loop kept firing keepalives WHILE backpressured (the server withholds it
    // if no ping arrives within 500ms). On the pre-backpressure loop the loop
    // is blocked on a full-channel send and sends no ping until the drain at
    // 800ms — past the server's window — so the marker never comes.
    let server_address = get_mock_address(backpressure_probe_server).await;
    let options = ConnectOptions::builder()
        .keepalive_interval(Some(Duration::from_millis(100)))
        .build();
    let mut socketeer: Socketeer<EchoJson, NoopHandler, 2> =
        Socketeer::connect_with(&format!("ws://{server_address}"), options)
            .await
            .unwrap();

    // Stay paused well past the server's 500ms ping window.
    sleep(Duration::from_millis(800)).await;

    // All five burst frames must arrive, in order — zero loss.
    let mut received = Vec::new();
    for _ in 0..5 {
        match socketeer.next_message().await.unwrap() {
            EchoControlMessage::Message(s) => received.push(s),
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(
        received,
        vec!["burst-0", "burst-1", "burst-2", "burst-3", "burst-4"]
    );

    // Marker present => the client kept the protocol alive while backpressured.
    let marker = timeout(Duration::from_secs(2), socketeer.next_message())
        .await
        .expect("alive marker should arrive, not hang")
        .unwrap();
    assert_eq!(marker, EchoControlMessage::Message("alive".to_string()));

    socketeer.close_connection().await.unwrap();
}

#[tokio::test]
async fn test_backpressure_without_keepalive_preserves_frames() {
    // Small buffer (2) < burst (5) forces backpressure, and keepalives are
    // disabled — exercising the no-keepalive arm of `deliver_with_backpressure`.
    // The consumer pauses, then drains: every burst frame must still arrive in
    // order with zero loss. (No keepalive => no ping => the server never emits
    // the "alive" marker, so we only assert the burst.)
    let server_address = get_mock_address(backpressure_probe_server).await;
    let options = ConnectOptions::builder().keepalive_interval(None).build();
    let mut socketeer: Socketeer<EchoJson, NoopHandler, 2> =
        Socketeer::connect_with(&format!("ws://{server_address}"), options)
            .await
            .unwrap();

    // Let the burst fill the buffer and park the loop in backpressure.
    sleep(Duration::from_millis(200)).await;

    let mut received = Vec::new();
    for _ in 0..5 {
        match timeout(Duration::from_secs(2), socketeer.next_message())
            .await
            .expect("burst frame should arrive, not hang")
            .unwrap()
        {
            EchoControlMessage::Message(s) => received.push(s),
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(
        received,
        vec!["burst-0", "burst-1", "burst-2", "burst-3", "burst-4"]
    );

    socketeer.close_connection().await.unwrap();
}

#[tokio::test]
async fn test_backpressure_allows_outgoing_sends() {
    // Small buffer (2) < burst (5) parks the loop in backpressure while the
    // consumer is not reading. A confirmed send issued in that window must still
    // complete: it exercises the `receiver.recv()` arm of the backpressure loop,
    // proving a slow receiver does not block the outgoing path. After that the
    // held burst still drains in full and in order.
    let server_address = get_mock_address(backpressure_probe_server).await;
    let options = ConnectOptions::builder()
        .keepalive_interval(Some(Duration::from_millis(100)))
        .build();
    let mut socketeer: Socketeer<EchoJson, NoopHandler, 2> =
        Socketeer::connect_with(&format!("ws://{server_address}"), options)
            .await
            .unwrap();

    // Let the burst fill the buffer and park the loop in backpressure before we
    // send, so the send is serviced from inside the backpressure loop rather
    // than the outer select.
    sleep(Duration::from_millis(200)).await;

    // Confirmed send completes even though the receive buffer is full and the
    // consumer has read nothing.
    socketeer
        .send(EchoControlMessage::Message("while-backpressured".into()))
        .await
        .unwrap();

    let mut received = Vec::new();
    for _ in 0..5 {
        match timeout(Duration::from_secs(2), socketeer.next_message())
            .await
            .expect("burst frame should arrive, not hang")
            .unwrap()
        {
            EchoControlMessage::Message(s) => received.push(s),
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(
        received,
        vec!["burst-0", "burst-1", "burst-2", "burst-3", "burst-4"]
    );

    socketeer.close_connection().await.unwrap();
}
