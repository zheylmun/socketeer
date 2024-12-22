# socketeer

Socketeer is a simple wrapper which wraps an tokiotungstenite WebSocket connection and provides a simple async API to send and receive messages.
It automatically handles websocket handshake and reconnects if the connection is lost, as well as managing websocket ping/pong messages.
