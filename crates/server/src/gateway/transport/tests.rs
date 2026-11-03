use super::*;

#[test]
fn grpc_framing_roundtrip() {
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&grpc_frame(b"abc"));
    buf.extend_from_slice(&grpc_frame(b""));
    buf.extend_from_slice(&grpc_frame(b"xy")[..3]);
    let msgs = take_messages(&mut buf);
    assert_eq!(msgs, vec![Bytes::from_static(b"abc"), Bytes::new()]);
    assert_eq!(buf.len(), 3);
}
