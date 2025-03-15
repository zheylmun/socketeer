use socketeer::{echo_server, get_mock_address, EchoControlMessage, Socketeer};
use tracing_subscriber::fmt::Subscriber;

#[tokio::main]
async fn main() {
    // We set up a tracing subscriber to see the logs from the Socketeer library.
    let subscriber = Subscriber::builder()
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    // We start out by creating a mock server that echoes messages back to the client
    // and handles a few control messages.
    let server_address = get_mock_address(echo_server).await;

    // Next, we create a Socketeer instance that connects to the mock server.
    let mut socketeer: Socketeer<EchoControlMessage, EchoControlMessage> =
        Socketeer::connect(&format!("ws://{server_address}",))
            .await
            .unwrap();
    // Send a message to the echo server
    socketeer
        .send(EchoControlMessage::Message("Hello, world!".to_string()))
        .await
        .unwrap();
    // Wait for the echo server to respond
    let response = socketeer.next_message().await.unwrap();

    // Check that the response is what we expect
    assert_eq!(
        response,
        EchoControlMessage::Message("Hello, world!".to_string())
    );
    // Shut everything down and close the connection
    socketeer.close_connection().await.unwrap();
}
