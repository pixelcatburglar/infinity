use super::*;

#[test]
fn grpc_framing_roundtrip() {
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&grpc_frame(b"abc"));
    buf.extend_from_slice(&grpc_frame(b""));
    buf.extend_from_slice(&grpc_frame(b"xy")[..3]);
    let msgs = checked_messages(&mut buf).unwrap();
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
    let request = http::Request::builder().uri("http://localhost/eve_public.gateway.Notices/Consume")
        .header("application_instance_uuid", "00000000-0000-0000-0000-000000000002").body(()).unwrap();
    let (response, mut send) = client.send_request(request, false).unwrap();
    send.send_data(grpc_frame(&[]), false).unwrap();
    send.send_data(grpc_frame(&[]), true).unwrap();
    let mut body = response.await.unwrap().into_body();
    assert!(body.data().await.is_none());
    assert_eq!(body.trailers().await.unwrap().unwrap()["grpc-status"], "0");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn replacement_cancels_consumer_without_first_body_frame() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gateway = super::super::tests::gateway().await;
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        connection(socket, gateway).await.unwrap();
    });
    let (mut client, driver) = h2::client::handshake(tokio::net::TcpStream::connect(addr).await.unwrap()).await.unwrap();
    tokio::spawn(async move { let _ = driver.await; });
    let request = || http::Request::builder()
        .uri("http://localhost/eve_public.gateway.Notices/Consume")
        .header("application_instance_uuid", "00000000-0000-0000-0000-000000000002").body(()).unwrap();
    let (first, _first_body) = client.send_request(request(), false).unwrap();
    let mut first_response = first.await.unwrap().into_body();
    let (second, _second_body) = client.send_request(request(), false).unwrap();
    let _second_response = second.await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), first_response.data()).await;
    assert!(result.is_ok(), "replaced consumer remained open");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn application_notice_before_body_and_http2_reset_reconnect() {
    use super::super::{notices::GatewayNotice, protocol::target_group::Target, protocol::Notice};
    use prost_reflect::DynamicMessage;
    let gateway = super::super::tests::gateway().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_gateway = gateway.clone();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let _ = connection(socket, server_gateway).await;
    });
    let (mut client, driver) = h2::client::handshake(tokio::net::TcpStream::connect(addr).await.unwrap()).await.unwrap();
    tokio::spawn(async move { let _ = driver.await; });
    let open = || http::Request::builder().uri("http://localhost/eve_public.gateway.Notices/Consume")
        .header("application_instance_uuid", "00000000-0000-0000-0000-000000000002").body(()).unwrap();
    let (response, mut send) = client.send_request(open(), false).unwrap();
    let mut body = response.await.unwrap().into_body();
    let mut app = [0; UUID_LEN];
    app[UUID_LEN - 1] = 2;
    let payload = DynamicMessage::new(super::super::message::message("eve_public.Event").unwrap());
    gateway.notice_hub.dispatch(GatewayNotice::new(Target::ApplicationInstanceUuid(app.to_vec()), &payload).unwrap(), |_| None);
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(2), body.data()).await.unwrap().unwrap().unwrap();
    assert!(Notice::decode(&bytes[5..]).unwrap().payload.is_some());
    send.send_reset(h2::Reason::CANCEL);
    let (replacement, _send) = client.send_request(open(), false).unwrap();
    let mut replacement = replacement.await.unwrap().into_body();
    gateway.notice_hub.dispatch(GatewayNotice::new(Target::ApplicationInstanceUuid(app.to_vec()), &payload).unwrap(), |_| None);
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(2), replacement.data()).await.unwrap().unwrap().unwrap();
    assert!(Notice::decode(&bytes[5..]).unwrap().payload.is_some());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn cancelled_server_stream_allows_reconnect() {
    use super::super::{notices::GatewayNotice, protocol::target_group::Target};
    let gateway = super::super::tests::gateway().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tasks, mut task_rx) = tokio::sync::mpsc::unbounded_channel();
    let server_gateway = gateway.clone();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut connection = h2::server::handshake(socket).await.unwrap();
        while let Some(Ok((request, respond))) = connection.accept().await {
            let application = application_uuid(request.headers());
            let path = request.uri().path().to_owned();
            let gateway = server_gateway.clone();
            let task = tokio::spawn(async move { stream(path, application, request.into_body(), respond, gateway).await });
            tasks.send(task).unwrap();
        }
    });
    let (mut client, driver) = h2::client::handshake(tokio::net::TcpStream::connect(address).await.unwrap()).await.unwrap();
    tokio::spawn(async move { let _ = driver.await; });
    let open = || http::Request::builder().uri("http://localhost/eve_public.gateway.Notices/Consume")
        .header("application_instance_uuid", "00000000-0000-0000-0000-000000000002").body(()).unwrap();
    let (response, _send) = client.send_request(open(), false).unwrap();
    let task = tokio::time::timeout(std::time::Duration::from_secs(2), task_rx.recv()).await.unwrap().unwrap();
    let mut body = response.await.unwrap().into_body();
    let mut app = [0; UUID_LEN];
    app[UUID_LEN - 1] = 2;
    let payload = prost_reflect::DynamicMessage::new(super::super::message::message("eve_public.Event").unwrap());
    gateway.notice_hub.dispatch(GatewayNotice::new(Target::ApplicationInstanceUuid(app.to_vec()), &payload).unwrap(), |_| None);
    assert!(tokio::time::timeout(std::time::Duration::from_secs(2), body.data()).await.unwrap().unwrap().is_ok());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let (response, _send) = client.send_request(open(), false).unwrap();
    let _replacement_task = tokio::time::timeout(std::time::Duration::from_secs(2), task_rx.recv()).await.unwrap().unwrap();
    let mut replacement = response.await.unwrap().into_body();
    gateway.notice_hub.dispatch(GatewayNotice::new(Target::ApplicationInstanceUuid(app.to_vec()), &payload).unwrap(), |_| None);
    assert!(tokio::time::timeout(std::time::Duration::from_secs(2), replacement.data()).await.unwrap().unwrap().is_ok());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn zero_window_consumer_cannot_block_another_consumer() {
    use super::super::{notices::GatewayNotice, protocol::target_group::Target};
    let fixture = super::super::tests::gateway().await;
    let mut config = (*fixture.config).clone();
    config.gateway.notice_queue_capacity = 1;
    let gateway = Gateway::new(fixture.db.clone(), fixture.world.clone(), Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_gateway = gateway.clone();
    tokio::spawn(async move {
        for _ in 0..2 {
            let (socket, _) = listener.accept().await.unwrap();
            let gateway = server_gateway.clone();
            tokio::spawn(async move { let _ = connection(socket, gateway).await; });
        }
    });
    let mut builder = h2::client::Builder::new();
    builder.initial_window_size(0);
    let (mut slow_client, slow_driver) = builder.handshake::<_, Bytes>(tokio::net::TcpStream::connect(address).await.unwrap()).await.unwrap();
    tokio::spawn(async move { let _ = slow_driver.await; });
    let (mut fast_client, fast_driver) = h2::client::handshake(tokio::net::TcpStream::connect(address).await.unwrap()).await.unwrap();
    tokio::spawn(async move { let _ = fast_driver.await; });
    let open = |id| http::Request::builder().uri("http://localhost/eve_public.gateway.Notices/Consume")
        .header("application_instance_uuid", format!("00000000-0000-0000-0000-{id:012x}")).body(()).unwrap();
    let (slow, _slow_send) = slow_client.send_request(open(2), false).unwrap();
    let mut slow = slow.await.unwrap().into_body();
    let (fast, _fast_send) = fast_client.send_request(open(3), false).unwrap();
    let mut fast = fast.await.unwrap().into_body();
    let mut slow_app = [0; UUID_LEN];
    slow_app[UUID_LEN - 1] = 2;
    let mut fast_app = [0; UUID_LEN];
    fast_app[UUID_LEN - 1] = 3;
    let payload = prost_reflect::DynamicMessage::new(super::super::message::message("eve_public.Event").unwrap());
    for _ in 0..3 {
        gateway.notice_hub.dispatch(GatewayNotice::new(Target::ApplicationInstanceUuid(slow_app.to_vec()), &payload).unwrap(), |_| None);
    }
    gateway.notice_hub.dispatch(GatewayNotice::new(Target::ApplicationInstanceUuid(fast_app.to_vec()), &payload).unwrap(), |_| None);
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(2), fast.data()).await.unwrap().unwrap().unwrap();
    assert!(super::super::protocol::Notice::decode(&bytes[5..]).unwrap().payload.is_some());
    let ended = tokio::time::timeout(std::time::Duration::from_secs(2), slow.data()).await.unwrap();
    assert!(ended.is_none() || ended.unwrap().is_err());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn authenticated_request_and_consume_open_in_either_order() {
    use super::super::{notices::GatewayNotice, protocol::target_group::Target, protocol::{AuthoritativeContext, Identifier32, UserIdentifier}};
    use prost_reflect::DynamicMessage;
    let gateway = super::super::tests::gateway().await;
    let account: i64 = gateway.db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('gateway-test-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character: i64 = gateway.db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'gateway-test-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    gateway.world.register_pilot(character, tx);
    gateway.world.set_online_session(character, Some(infinity_core::session::OnlineSession {
        character_id: character, user_id: Some(account), session_id: 1, ship_id: None, station_id: None,
        system_id: Some(40), corporation_id: Some(1), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_gateway = gateway.clone();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        connection(socket, server_gateway).await.unwrap();
    });
    let (mut client, driver) = h2::client::handshake(tokio::net::TcpStream::connect(address).await.unwrap()).await.unwrap();
    tokio::spawn(async move { let _ = driver.await; });
    for (id, request_first) in [(3u8, true), (4u8, false)] {
        let metadata = format!("00000000-0000-0000-0000-{id:012x}");
        let open = |path: &str| http::Request::builder().uri(path).header("application_instance_uuid", &metadata).body(()).unwrap();
        let consume = "http://localhost/eve_public.gateway.Notices/Consume";
        let requests = "http://localhost/eve_public.gateway.Requests/Send";
        let mut application = vec![0; UUID_LEN];
        application[UUID_LEN - 1] = id;
        let request = Request {
            issued: None, correlation_uuid: vec![1; UUID_LEN], external_origin: String::new(), application_instance_uuid: application,
            authoritative_context: Some(AuthoritativeContext { tenant: String::new(), authenticated_user: Some(UserIdentifier { sequential: i32::try_from(account).unwrap() }), active_character: Some(Identifier32 { sequential: u32::try_from(character).unwrap() }), identity: None }),
            payload: Some(prost_types::Any { type_url: "type.googleapis.com/eve_public.character.GetNamesByUserRequest".into(), value: Vec::new() }),
        };
        let (notice_response, _notice_send, request_response, mut request_send) = if request_first {
            let (response, mut send) = client.send_request(open(requests), false).unwrap();
            send.send_data(grpc_frame(&request.encode_to_vec()), false).unwrap();
            let mut body = response.await.unwrap().into_body();
            assert!(body.data().await.unwrap().is_ok());
            let (notice, notice_send) = client.send_request(open(consume), false).unwrap();
            (notice, notice_send, body, send)
        } else {
            let (notice, _notice_send) = client.send_request(open(consume), false).unwrap();
            let notice = notice.await.unwrap();
            let (response, mut send) = client.send_request(open(requests), false).unwrap();
            send.send_data(grpc_frame(&request.encode_to_vec()), false).unwrap();
            let mut body = response.await.unwrap().into_body();
            assert!(body.data().await.unwrap().is_ok());
            let payload = DynamicMessage::new(super::super::message::message("eve_public.Event").unwrap());
            gateway.notice_hub.dispatch(GatewayNotice::new(Target::Character(u32::try_from(character).unwrap()), &payload).unwrap(), |id| gateway.world.online_session(id));
            let mut notice_body = notice.into_body();
            assert!(tokio::time::timeout(std::time::Duration::from_secs(2), notice_body.data()).await.unwrap().unwrap().is_ok());
            send.send_data(Bytes::new(), true).unwrap();
            continue;
        };
        let _request_response = request_response;
        let mut notice_body = notice_response.await.unwrap().into_body();
        let payload = DynamicMessage::new(super::super::message::message("eve_public.Event").unwrap());
        gateway.notice_hub.dispatch(GatewayNotice::new(Target::Character(u32::try_from(character).unwrap()), &payload).unwrap(), |id| gateway.world.online_session(id));
        assert!(tokio::time::timeout(std::time::Duration::from_secs(2), notice_body.data()).await.unwrap().unwrap().is_ok());
        let (replacement, _replacement_send) = client.send_request(open(consume), false).unwrap();
        let mut replacement_body = replacement.await.unwrap().into_body();
        let old_end = tokio::time::timeout(std::time::Duration::from_secs(2), notice_body.data()).await.unwrap();
        assert!(old_end.is_none() || old_end.unwrap().is_err());
        let payload = DynamicMessage::new(super::super::message::message("eve_public.Event").unwrap());
        gateway.notice_hub.dispatch(GatewayNotice::new(Target::Character(u32::try_from(character).unwrap()), &payload).unwrap(), |id| gateway.world.online_session(id));
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), replacement_body.data()).await.unwrap().unwrap().unwrap();
        let decoded = super::super::protocol::Notice::decode(&frame[5..]).unwrap();
        assert!(decoded.payload.is_some());
        request_send.send_data(Bytes::new(), true).unwrap();
    }
    gateway.world.unregister_pilot_generation(character, None);
    gateway.db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&character]).await.unwrap();
    gateway.db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
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
