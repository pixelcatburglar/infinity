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

#[test]
fn validates_metadata_and_fragmented_frames() {
    let mut headers = http::HeaderMap::new();
    assert!(application_uuid(&headers).is_none());
    headers.insert("application_instance_uuid", "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap());
    assert_eq!(application_uuid(&headers), Some([0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]));
    headers.insert("application_instance_uuid", "not-a-uuid".parse().unwrap());
    for invalid in ["00112233445566778899aabbccddeeff", "00112233-4455-6677-8899-AABBCCDDEEFF", "00112233_4455-6677-8899-aabbccddeeff"] {
        headers.insert("application_instance_uuid", invalid.parse().unwrap());
        assert!(application_uuid(&headers).is_none(), "{invalid}");
    }
    assert!(application_uuid(&headers).is_none());
    let frame = grpc_frame(b"abc");
    let mut buffer = BytesMut::from(&frame[..3]);
    assert!(checked_messages(&mut buffer).unwrap().is_empty());
    buffer.extend_from_slice(&frame[3..]);
    buffer.extend_from_slice(&grpc_frame(b"xy"));
    assert_eq!(checked_messages(&mut buffer).unwrap(), vec![Bytes::from_static(b"abc"), Bytes::from_static(b"xy")]);
    buffer.extend_from_slice(&[1]);
    assert!(checked_messages(&mut buffer).is_err());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn http2_rejects_bad_frames_and_unknown_methods() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        connection(sock, super::super::tests::gateway().await).await.unwrap();
    });
    let socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut client, conn) = h2::client::handshake(socket).await.unwrap();
    tokio::spawn(async move { conn.await.unwrap(); });
    let request = http::Request::builder().uri("http://localhost/eve_public.gateway.Requests/Send")
        .header("application_instance_uuid", "00112233-4455-6677-8899-aabbccddeeff").body(()).unwrap();
    let (response, mut send) = client.send_request(request, false).unwrap();
    send.send_data(Bytes::from_static(&[1, 0, 0, 0, 0]), true).unwrap();
    let mut body = response.await.unwrap().into_body();
    while body.data().await.is_some() {}
    assert_eq!(body.trailers().await.unwrap().unwrap()["grpc-status"], "3");
    let request = http::Request::builder().uri("http://localhost/unknown/Ping").body(()).unwrap();
    let (response, _) = client.send_request(request, true).unwrap();
    let mut body = response.await.unwrap().into_body();
    while body.data().await.is_some() {}
    assert_eq!(body.trailers().await.unwrap().unwrap()["grpc-status"], "12");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn event_stream_ends_without_per_event_responses() {
    use prost_reflect::{DynamicMessage, Value};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        connection(sock, super::super::tests::gateway().await).await.unwrap();
    });
    let (mut client, conn) = h2::client::handshake(tokio::net::TcpStream::connect(addr).await.unwrap()).await.unwrap();
    tokio::spawn(async move { conn.await.unwrap(); });
    let request = http::Request::builder().uri("http://localhost/eve_public.gateway.Events/Publish")
        .header("application_instance_uuid", "00000000-0000-0000-0000-000000000002").body(()).unwrap();
    let (response, mut send) = client.send_request(request, false).unwrap();
    let mut event = DynamicMessage::new(super::super::message::message("eve_public.Event").unwrap());
    let mut app = vec![0; UUID_LEN];
    app[UUID_LEN - 1] = 2;
    event.set_field_by_name("uuid", Value::Bytes(vec![1; UUID_LEN].into()));
    event.set_field_by_name("application_instance_uuid", Value::Bytes(app.into()));
    let mut any = DynamicMessage::new(super::super::message::message("google.protobuf.Any").unwrap());
    any.set_field_by_name("type_url", Value::String("type.googleapis.com/unknown.Event".into()));
    event.set_field_by_name("payload", Value::Message(any));
    let frame = grpc_frame(&event.encode_to_vec());
    send.send_data(frame.slice(..3), false).unwrap();
    let mut rest = BytesMut::from(&frame[3..]);
    rest.extend_from_slice(&frame);
    send.send_data(rest.freeze(), true).unwrap();
    let mut body = response.await.unwrap().into_body();
    assert!(body.data().await.is_none());
    assert_eq!(body.trailers().await.unwrap().unwrap()["grpc-status"], "0");
}

async fn status_for_request_frame(metadata: Option<&str>, frame: Bytes) -> String {
    status_for_frame("http://localhost/eve_public.gateway.Requests/Send", metadata, frame).await
}

async fn status_for_frame(path: &str, metadata: Option<&str>, frame: Bytes) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        connection(socket, super::super::tests::gateway().await).await.unwrap();
    });
    let (mut client, driver) = h2::client::handshake(tokio::net::TcpStream::connect(address).await.unwrap()).await.unwrap();
    tokio::spawn(async move { driver.await.unwrap(); });
    let mut request = http::Request::builder().uri(path);
    if let Some(metadata) = metadata { request = request.header("application_instance_uuid", metadata); }
    let (response, mut send) = client.send_request(request.body(()).unwrap(), false).unwrap();
    send.send_data(frame, true).unwrap();
    let mut body = response.await.unwrap().into_body();
    while body.data().await.is_some() {}
    body.trailers().await.unwrap().unwrap()["grpc-status"].to_str().unwrap().to_owned()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn request_stream_rejects_invalid_envelopes_and_truncation() {
    let app = "00000000-0000-0000-0000-000000000002";
    assert_eq!(status_for_request_frame(None, Bytes::new()).await, "3");
    assert_eq!(status_for_request_frame(Some(app), Bytes::from_static(&[0, 0])).await, "3");
    assert_eq!(status_for_request_frame(Some(app), grpc_frame(&[0xff])).await, "3");
    let req = Request {
        issued: None, correlation_uuid: vec![1; UUID_LEN], payload: None, external_origin: String::new(),
        application_instance_uuid: vec![3; UUID_LEN], authoritative_context: None,
    };
    assert_eq!(status_for_request_frame(Some(app), grpc_frame(&req.encode_to_vec())).await, "3");
    for correlation_len in [0, UUID_LEN - 1, UUID_LEN + 1] {
        let mut invalid = req.clone();
        invalid.application_instance_uuid = { let mut app = vec![0; UUID_LEN]; app[UUID_LEN - 1] = 2; app };
        invalid.correlation_uuid = vec![1; correlation_len];
        assert_eq!(status_for_request_frame(Some(app), grpc_frame(&invalid.encode_to_vec())).await, "3");
    }
    for application_len in [0, UUID_LEN - 1, UUID_LEN + 1] {
        let mut invalid = req.clone();
        invalid.application_instance_uuid = vec![2; application_len];
        assert_eq!(status_for_request_frame(Some(app), grpc_frame(&invalid.encode_to_vec())).await, "3");
    }
    assert_eq!(status_for_request_frame(Some("not-a-uuid"), Bytes::new()).await, "3");
    assert_eq!(status_for_request_frame(Some(app), Bytes::from_static(&[0, 0, 0, 0, 2, 1])).await, "3");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn event_stream_rejects_invalid_metadata_frames_and_envelopes() {
    let path = "http://localhost/eve_public.gateway.Events/Publish";
    let app = "00000000-0000-0000-0000-000000000002";
    assert_eq!(status_for_frame(path, None, Bytes::new()).await, "3");
    assert_eq!(status_for_frame(path, Some("invalid"), Bytes::new()).await, "3");
    assert_eq!(status_for_frame(path, Some(app), Bytes::from_static(&[1, 0, 0, 0, 0])).await, "3");
    assert_eq!(status_for_frame(path, Some(app), Bytes::from_static(&[0, 0, 0, 0, 2, 1])).await, "3");
    assert_eq!(status_for_frame(path, Some(app), grpc_frame(&[0xff])).await, "3");
    assert_eq!(status_for_frame(path, Some(app), grpc_frame(&[])).await, "3");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn consume_remains_ungated_and_drains() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gateway = super::super::tests::gateway().await;
    let server_gateway = gateway.clone();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        connection(socket, server_gateway).await.unwrap();
    });
    let (mut client, driver) = h2::client::handshake(tokio::net::TcpStream::connect(addr).await.unwrap()).await.unwrap();
    tokio::spawn(async move { driver.await.unwrap(); });
    let request = http::Request::builder().uri("http://localhost/eve_public.gateway.Notices/Consume").body(()).unwrap();
    let (response, mut send) = client.send_request(request, false).unwrap();
    send.send_data(grpc_frame(&[]), false).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if !gateway.notice_streams.lock().unwrap().is_empty() { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    send.send_data(grpc_frame(&[]), true).unwrap();
    let mut body = response.await.unwrap().into_body();
    assert!(body.data().await.is_none());
    assert_eq!(body.trailers().await.unwrap().unwrap()["grpc-status"], "0");
    assert!(gateway.notice_streams.lock().unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn fragmented_request_stream_returns_correlated_responses() {
    use super::super::protocol::Response;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        connection(socket, super::super::tests::gateway().await).await.unwrap();
    });
    let (mut client, driver) = h2::client::handshake(tokio::net::TcpStream::connect(address).await.unwrap()).await.unwrap();
    tokio::spawn(async move { driver.await.unwrap(); });
    let request = http::Request::builder().uri("http://localhost/eve_public.gateway.Requests/Send")
        .header("application_instance_uuid", "00000000-0000-0000-0000-000000000002").body(()).unwrap();
    let (response, mut send) = client.send_request(request, false).unwrap();
    let mut app = vec![0; UUID_LEN];
    app[UUID_LEN - 1] = 2;
    let req = Request {
        issued: None, correlation_uuid: vec![1; UUID_LEN], external_origin: String::new(),
        application_instance_uuid: app, authoritative_context: None,
        payload: Some(prost_types::Any {
            type_url: "type.googleapis.com/eve_public.character.GetNamesByUserRequest".into(),
            value: vec![0xff],
        }),
    };
    let frame = grpc_frame(&req.encode_to_vec());
    send.send_data(frame.slice(..3), false).unwrap();
    let mut rest = BytesMut::from(&frame[3..]);
    rest.extend_from_slice(&frame);
    send.send_data(rest.freeze(), true).unwrap();
    let mut body = response.await.unwrap().into_body();
    let mut received = BytesMut::new();
    while let Some(chunk) = body.data().await {
        received.extend_from_slice(&chunk.unwrap());
    }
    let messages = checked_messages(&mut received).unwrap();
    assert_eq!(messages.len(), 2);
    for bytes in messages {
        let response = Response::decode(bytes).unwrap();
        assert_eq!(response.status_code, 400);
        assert_eq!(response.correlation_uuid, req.correlation_uuid);
        assert!(response.payload.is_none());
    }
    assert!(received.is_empty());
    assert_eq!(body.trailers().await.unwrap().unwrap()["grpc-status"], "0");
}
