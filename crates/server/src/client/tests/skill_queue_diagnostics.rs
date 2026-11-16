//! Transport regressions for skill queue diagnostics; uses synthetic values and no database.
use super::*;
use tokio::io::AsyncReadExt;
use tracing::instrument::WithSubscriber;

fn request(service: &str, method: &str, args: Vec<Value>, id: i64) -> Packet {
    let inner = Value::tuple([Value::Int(1), Value::str(method), Value::Tuple(args),
        Value::dict([("machoVersion", Value::Int(1)), ("other", Value::Int(1))])]);
    Packet::new(cmd::CALL_REQ,
        Address::Client { client_id: 1, call_id: Value::Int(id), service: None },
        Address::Any { service: Some(service.into()), call_id: Value::None },
        Value::Int(1), vec![Value::tuple([Value::Int(0), Value::str(marshal::dumps(&inner))])])
}

fn queue_notification(client: &mut Client) {
    let entry = crate::services::keyval([
        ("queuePosition", Value::Int(1)), ("trainingTypeID", Value::Int(1)),
        ("trainingToLevel", Value::Int(1)), ("trainingStartSP", Value::Int(1)),
        ("trainingDestinationSP", Value::Int(1)), ("trainingStartTime", Value::None),
        ("trainingEndTime", Value::Int(1)),
    ]);
    client.notify("OnNewSkillQueueSaved", vec![Value::list([entry])]);
}

async fn frame(peer: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut header = [0; 4];
    peer.read_exact(&mut header).await.unwrap();
    let mut bytes = vec![0; u32::from_le_bytes(header) as usize];
    peer.read_exact(&mut bytes).await.unwrap();
    [header.as_slice(), bytes.as_slice()].concat()
}

fn packet(frame: &[u8]) -> Packet {
    Packet::from_value(&marshal::loads(&frame[4..]).unwrap()).unwrap()
}

async fn unused(fixture: &PublicationFixture) {
    fixture.db.assert_publication_fixture_unused();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(50), fixture.trap.accept()).await.is_err());
}

async fn scenario() -> Vec<Vec<u8>> {
    let mut f = PublicationFixture::new().await;
    let mut kwargs = Value::dict([("machoVersion", Value::Int(1)), ("other", Value::Int(1))]);
    strip_macho_version(&mut kwargs);
    assert_eq!(kwargs, Value::dict([("other", Value::Int(1))]));
    queue_notification(&mut f.client);
    let expected_notification = marshal::dumps(&f.client.outbox[0].to_value());
    let calls = [
        request("skillHandler", "GetSkillChangesForISIS", vec![], 1),
        request("skillHandler", "SaveNewQueue", vec![Value::None], 1),
        request("alert", "BeanCount", vec![], 1),
    ];
    let mut frames = Vec::new();
    for (index, call) in calls.into_iter().enumerate() {
        let read = async {
            let mut received = Vec::new();
            for _ in 0..if index == 0 { 2 } else { 1 } { received.push(frame(&mut f.peer).await); }
            received
        };
        let (result, received) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(f.client.handle_call(call.clone()), read)
        }).await.unwrap();
        result.unwrap();
        if index == 0 {
            assert_eq!(&received[0][4..], expected_notification);
            assert_eq!(packet(&received[0]).command, cmd::NOTIFICATION);
        }
        let response = packet(received.last().unwrap());
        assert_eq!(response.destination, call.source);
        if index == 1 {
            let expected = user_error(infinity_core::messages::MSG_QUEUE_INVALID_POSITION, vec![]);
            let expected = call.error_response(ERR_WRAPPEDEXCEPTION, Value::tuple([Value::str(marshal::dumps(&expected))]));
            assert_eq!(response.to_value(), expected.to_value());
        } else {
            assert_eq!(response.command, cmd::CALL_RSP);
            let result = marshal::loads(response.body[0].as_bytes().unwrap()).unwrap();
            assert_eq!(result, if index == 0 { Value::list([]) } else { Value::tuple([Value::None, Value::None]) });
        }
        frames.extend(received);
    }
    unused(&f).await;
    frames
}

fn subscriber(level: &str) -> impl tracing::Subscriber + Send + Sync {
    let filter = tracing_subscriber::EnvFilter::new(format!("off,infinity_server::client::skill_queue={level}"));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::sink).finish()
}

#[tokio::test]
async fn skill_queue_logging_preserves_tcp_frames_and_results() {
    let off = scenario().with_subscriber(subscriber("off")).await;
    let debug = scenario().with_subscriber(subscriber("debug")).await;
    let trace = scenario().with_subscriber(subscriber("trace")).await;
    assert_eq!(off, debug);
    assert_eq!(off, trace);
}

async fn closed_writer(pending: bool) -> String {
    let mut f = PublicationFixture::new().await;
    if pending { queue_notification(&mut f.client); }
    f.client.conn.close().await.unwrap();
    let error = f.client.handle_call(request("skillHandler", "GetSkillChangesForISIS", vec![], 1)).await.unwrap_err();
    assert!(error.downcast_ref::<std::io::Error>().is_some());
    assert!(f.client.outbox.is_empty());
    assert!(infinity_wire::framing::read_frame(&mut f.peer).await.unwrap().is_none());
    unused(&f).await;
    format!("{error:#}")
}

#[tokio::test]
async fn skill_queue_logging_preserves_closed_writer_errors() {
    for pending in [false, true] {
        let off = closed_writer(pending).with_subscriber(subscriber("off")).await;
        let trace = closed_writer(pending).with_subscriber(subscriber("trace")).await;
        assert_eq!(off, trace);
        assert!(!off.is_empty());
    }
}
