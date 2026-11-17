use super::*;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

#[test]
fn crc_hqx_matches_python() {
    // binascii.crc_hqx(b'123456789', 0) == 0x31C3
    assert_eq!(crc_hqx(b"123456789", 0), 0x31C3);
    assert_eq!(crc_hqx(b"", 0), 0);
}


/// Accept one real connection on a loopback listener and return it with a
/// single-slot admission gate holding its pending permit.
async fn accepted() -> (TcpListener, TcpStream, Conn, std::net::SocketAddr, std::sync::Arc<crate::admission::Admission>, Permit) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, addr) = listener.accept().await.unwrap();
    let gate = crate::admission::Admission::new(1, 1);
    let permit = gate.try_acquire(addr.ip()).unwrap();
    (listener, peer, Conn::new(stream, addr), addr, gate, permit)
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn qc_probes_do_not_extend_the_deadline() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (_listener, mut peer, conn, addr, gate, permit) = accepted().await;
    let deadline = Instant::now() + std::time::Duration::from_millis(700);
    let task = tokio::spawn(async move {
        let cfg = Config::default();
        run(conn, &cfg, &db, 2, permit, deadline).await
    });
    let compat = marshal::dumps(&Value::tuple([Value::Int(HANDSHAKE_SECRET_KEY), Value::Int(Config::default().server.macho_version), Value::None, Value::None, Value::Int(Config::default().server.boot_build), Value::None]));
    let qc = marshal::dumps(&Value::tuple([Value::None, Value::str("QC")]));
    infinity_wire::framing::read_frame(&mut peer).await.unwrap().expect("handshake tuple");
    for _ in 0..2 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        infinity_wire::framing::write_frame(&mut peer, &compat).await.unwrap();
        infinity_wire::framing::write_frame(&mut peer, &qc).await.unwrap();
        let reply = infinity_wire::framing::read_frame(&mut peer).await.unwrap().expect("queue position reply");
        assert!(matches!(marshal::loads(&reply.payload), Ok(Value::Int(1))));
        infinity_wire::framing::read_frame(&mut peer).await.unwrap().expect("renewed handshake tuple");
    }
    while infinity_wire::framing::read_frame(&mut peer).await.unwrap().is_some() {}
    // A probe that renewed the budget would keep the socket past deadline+150ms.
    let observed = Instant::now();
    assert!(observed >= deadline && observed <= deadline + std::time::Duration::from_millis(150), "a probe renewed the deadline");
    assert!(matches!(task.await.unwrap(), Ok(None)));
    assert!(gate.try_acquire(addr.ip()).is_some(), "pending slot released");
}

#[tokio::test]
async fn dropped_state_closes_socket_then_releases_registration_and_slot() {
    let account = 9_999_900_001_i64;
    let first = crate::live_sessions::register(account, true).unwrap();
    let mut replacement = crate::live_sessions::register(account, true).unwrap();
    let barrier = tokio::spawn(async move { replacement.wait_for_predecessor().await; });
    let (_listener, mut peer, conn, addr, gate, permit) = accepted().await;
    drop(State { conn, registration: Some(first), permit });
    tokio::time::timeout(std::time::Duration::from_secs(5), async { let mut b = [0u8; 8]; while peer.read(&mut b).await.unwrap() != 0 {} })
        .await.expect("peer did not observe the socket close");
    tokio::time::timeout(std::time::Duration::from_secs(5), barrier).await.unwrap().unwrap();
    assert!(gate.try_acquire(addr.ip()).is_some(), "pending slot released");
}
