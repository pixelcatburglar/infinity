//! Bounded HTTP request reads against real sockets, and the content CDN
//! endpoint served by the same listener. The socket helper and its imports are
//! `tests-runtime`; the CDN test is `tests-sde` (it exercises the verified
//! retail catalogue).
#[cfg(feature = "tests-runtime")]
use super::*;

#[cfg(feature = "tests-runtime")]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(feature = "tests-runtime")]
use tokio::net::{TcpListener, TcpStream};
#[cfg(any(feature = "tests-runtime", feature = "tests-sde"))]
use std::time::Duration;

/// Drive `handle` on one real socket carrying `request` and return the bytes
/// written before the server closed, plus `handle`'s own result.
#[cfg(feature = "tests-runtime")]
async fn exchange(request: &[u8], deadline: Instant) -> (Vec<u8>, Result<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut peer = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, addr) = listener.accept().await.unwrap();
    let upstream = Upstream::new("", 1);
    let task = tokio::spawn(async move { handle(stream, Path::new(""), "test", addr, &upstream, usize::MAX, deadline).await });
    peer.write_all(request).await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), peer.read_to_end(&mut out)).await.expect("request was not rejected promptly").unwrap();
    (out, task.await.unwrap())
}

#[cfg(feature = "tests-runtime")]
#[tokio::test]
async fn request_reads_are_bounded_and_timed() {
    let far = Instant::now() + Duration::from_secs(30);
    // An unterminated line over the bound is refused at once, far ahead of the
    // read deadline; an immediate reject proves the length limit fired.
    let (out, result) = exchange(&vec![b'G'; HTTP_REQUEST_LINE_LIMIT_BYTES + 1], far).await;
    assert!(result.is_err() && out.is_empty(), "oversized request line answered");
    // Complete terminated headers and a blank line whose total passes the bound.
    let headers = [b"GET /x HTTP/1.0\r\nX: ".as_slice(), &[b'a'; 32762], b"\r\n\r\n"].concat();
    assert_eq!(headers.len(), 17 + 32769);
    let (out, result) = exchange(&headers, far).await;
    assert!(result.is_err() && out.is_empty(), "headers over the total budget answered");
    let (out, result) = exchange(b"GET /x HTTP/1.0\r\n", Instant::now() + Duration::from_millis(300)).await;
    assert!(result.is_err() && out.is_empty(), "stalled request survived the read deadline");
}

#[cfg(feature = "tests-sde")]
#[tokio::test]
async fn http_get_and_head_match_loaded_checkpoint() {
    let reservation = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let root = std::env::temp_dir().join(format!("achievement-http-{}-{}", std::process::id(), address.port()));
    let server = tokio::spawn(crate::images::serve(crate::images::ListenerKind::Cdn, address.to_string(), root.clone(), crate::images::Upstream::new("", 1), 1));
    let url = format!("http://{address}/content/achievements");
    let client = reqwest::Client::new();
    let mut get = None;
    for _ in 0..100 {
        if let Ok(response) = client.get(&url).send().await { get = Some(response); break; }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let get = get.expect("CDN server did not start");
    assert_eq!(get.status(), reqwest::StatusCode::OK);
    let length = get.content_length().unwrap();
    let bytes = get.bytes().await.unwrap();
    let head = client.head(&url).send().await.unwrap();
    assert_eq!(head.status(), reqwest::StatusCode::OK);
    assert_eq!(head.headers().get(reqwest::header::CONTENT_LENGTH).unwrap().to_str().unwrap().parse::<u64>().unwrap(), length);
    assert!(head.bytes().await.unwrap().is_empty());
    let source = infinity_content::retail::cdn("achievements").unwrap();
    assert_eq!(bytes.as_ref(), source.data);
    assert_eq!(length as usize, source.data.len());
    let catalog = infinity_content::achievements::Achievements::load(source).unwrap();
    assert_eq!(catalog.checkpoint_metadata().crc, infinity_content::cdn::crc32(&bytes));
    server.abort();
    tokio::fs::remove_dir_all(root).await.unwrap();
}
