use super::{inflate, read_frame, FLAG_ZLIB, MAX_INFLATED_PACKET};
use flate2::{write::ZlibEncoder, Compression};
use std::io::Write;

#[tokio::test]
async fn inflates_zlib_payload_and_frame() {
    let payload = b"compressed packet payload";
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(payload).unwrap();
    let compressed = encoder.finish().unwrap();

    assert_eq!(inflate(&compressed).unwrap(), payload);

    let header = FLAG_ZLIB | compressed.len() as u32;
    let mut frame = header.to_le_bytes().to_vec();
    frame.extend_from_slice(&compressed);
    let decoded = read_frame(&mut frame.as_slice()).await.unwrap().unwrap();
    assert_eq!(decoded.payload, payload);
}

#[test]
fn rejects_malformed_zlib() {
    assert!(inflate(&[0xff, 0xff, 0xff, 0xff]).is_err());
}

#[tokio::test]
async fn accepts_inflated_packet_at_limit() {
    let payload = vec![b'a'; MAX_INFLATED_PACKET];
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&payload).unwrap();
    let compressed = encoder.finish().unwrap();

    assert_eq!(inflate(&compressed).unwrap(), payload);

    let header = FLAG_ZLIB | compressed.len() as u32;
    let mut frame = header.to_le_bytes().to_vec();
    frame.extend_from_slice(&compressed);
    let decoded = read_frame(&mut frame.as_slice()).await.unwrap().unwrap();
    assert_eq!(decoded.payload, payload);
}

#[tokio::test]
async fn rejects_inflated_packet_over_limit() {
    let payload = vec![b'a'; MAX_INFLATED_PACKET + 1];
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&payload).unwrap();
    let compressed = encoder.finish().unwrap();

    let error = inflate(&compressed).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

    let header = FLAG_ZLIB | compressed.len() as u32;
    let mut frame = header.to_le_bytes().to_vec();
    frame.extend_from_slice(&compressed);
    let error = read_frame(&mut frame.as_slice()).await.err().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}
