//! Compile-time proof that the foreign types socketeer exposes are re-exported,
//! so downstream users need no direct dependency on tungstenite/http/bytes.

#[test]
fn reexports_resolve() {
    // Each path must resolve purely through `socketeer::`.
    fn _takes_message(_: socketeer::Message) {}
    fn _takes_bytes(_: socketeer::Bytes) {}
    fn _takes_header_map(_: socketeer::http::HeaderMap) {}
    fn _takes_ws_error(_: socketeer::tungstenite::Error) {}
    fn _takes_ws_stream(_: socketeer::WebSocketStreamType) {}
}
