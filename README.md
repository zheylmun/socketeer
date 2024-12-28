# socketeer

Socketeer is a simple wrapper which wraps an tokio-tungstenite sebsocket connection and provides a simple async API to send and receive messages.
It automatically handles the underlying websocket and allows for reconnection.
Currently it only supports binary and text messages which are json encoded.
