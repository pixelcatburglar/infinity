use super::*;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::time::{timeout, Duration};

#[test]
fn usurp_exception_roundtrips_with_close_arguments() {
    let decoded = marshal::loads(&marshal::dumps(&usurped_close())).unwrap();
    assert_eq!(decoded, Value::Reduce {
        data: Box::new(Value::tuple([
            Value::global("exceptions.GPSTransportClosed"),
            Value::tuple([Value::str(infinity_core::messages::SESSION_USURPED)]),
            Value::dict([
                ("reason", Value::str(infinity_core::messages::SESSION_USURPED)),
                ("reasonCode", Value::None),
                ("reasonArgs", Value::Dict(vec![])),
            ]),
        ])),
        list: vec![],
        dict: vec![],
    });
}

#[test]
fn refusal_exception_roundtrips_with_reason_code() {
    for reason_code in [infinity_core::messages::ACL_NOTACCEPTING, infinity_core::messages::ACL_SHUTTINGDOWN] {
        let decoded = marshal::loads(&marshal::dumps(&not_accepting_close(reason_code))).unwrap();
        assert_eq!(decoded, Value::Reduce {
            data: Box::new(Value::tuple([
                Value::global("exceptions.GPSTransportClosed"),
                Value::tuple([Value::None]),
                Value::dict([
                    ("reason", Value::None),
                    ("reasonCode", Value::str(reason_code)),
                    ("reasonArgs", Value::Dict(vec![])),
                ]),
            ])),
            list: vec![],
            dict: vec![],
        });
    }
}

#[tokio::test]
async fn close_stops_reader_and_delivers_eof() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut peer = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, addr) = listener.accept().await.unwrap();
    let (mut writer, mut inbox) = Conn::new(stream, addr).split();
    writer.close().await.unwrap();
    assert!(inbox.recv().await.is_none());
    let mut byte = [0; 1];
    assert_eq!(timeout(Duration::from_secs(1), peer.read(&mut byte)).await.unwrap().unwrap(), 0);
}

#[tokio::test(start_paused = true)]
async fn write_with_timeout_maps_elapsed_to_timed_out() {
    let elapsed = write_with_timeout(std::future::pending::<std::io::Result<()>>(), Duration::from_secs(1)).await.unwrap_err();
    assert_eq!(elapsed.kind(), std::io::ErrorKind::TimedOut);
}

#[tokio::test(start_paused = true)]
async fn timed_out_send_poisons_the_transport_without_retrying() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let mut conn = Conn::new(stream, address).with_write_timeout(Duration::from_secs(1));
    let flood = Value::str(vec![0u8; 16 << 20]);
    let error = conn.send(&flood).await.unwrap_err();
    assert_eq!(error.downcast_ref::<std::io::Error>().map(|e| e.kind()), Some(std::io::ErrorKind::TimedOut));
    let error = conn.send(&Value::None).await.unwrap_err();
    assert!(format!("{error:#}").contains("already failed"), "{error:#}");
}
