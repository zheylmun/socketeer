use socketeer::{echo_server, get_mock_address};

#[tokio::main]
async fn main() {
    // We start out by creating a mock server that echoes messages back to the client
    // and handles a few control messages.
    let server_address = get_mock_address(echo_server).await;

    // Next, we create a Socketeer instance that connects to the mock server.
    let socketeer: Socketeer<EchoControlMessage, EchoControlMessage> =
        Socketeer::connect(&format!("ws://{server_address}",))
            .await
            .unwrap();
}
