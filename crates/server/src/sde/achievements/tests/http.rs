use super::*;
use std::time::Duration;

#[tokio::test]
async fn http_get_and_head_match_loaded_checkpoint() {
    let reservation = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let root = std::env::temp_dir().join(format!("achievement-http-{}-{}", std::process::id(), address.port()));
    let server = tokio::spawn(crate::images::serve("achievement-test", address.to_string(), root.clone(), crate::images::Upstream::new("", 1), 1));
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
    let source = crate::retail::cdn("achievements").unwrap();
    assert_eq!(bytes.as_ref(), source.data);
    assert_eq!(length as usize, source.data.len());
    let catalog = Achievements::load(source).unwrap();
    assert_eq!(catalog.checkpoint_metadata().crc, crate::cdn::crc32(&bytes));
    server.abort();
    tokio::fs::remove_dir_all(root).await.unwrap();
}
