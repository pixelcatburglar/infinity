use super::*;
use flate2::{write::ZlibEncoder, Compression};

fn call<'a>(method: &'a str, args: &'a [Value]) -> Call<'a> {
    Call { service: "alert", method, args, kwargs: &Value::None, bound: false, bind_params: None }
}
fn event(args: &[Value]) -> Call<'_> {
    Call { service: "eventLog", method: "LogClientStats", args, kwargs: &Value::None, bound: false, bind_params: None }
}

async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(crate::services::dispatch(c, call).await)
}

#[test]
fn integer_hashes_survive_marshal_roundtrip() {
    for hash in [42, i64::from(i32::MAX) + 1] {
        let value = Value::Int(hash);
        let decoded = marshal::loads(&marshal::dumps(&value)).unwrap();
        assert_eq!(integer(&decoded), Some(hash));
        assert_eq!(decoded, value);
    }
}

#[test]
fn traceback_tail_respects_utf8_and_limit() {
    let text = format!("{}end", "中".repeat(TRACE_BYTES));
    let result = tail(&text, TRACE_BYTES);
    assert!(result.len() <= TRACE_BYTES);
    assert!(result.ends_with("end"));
    assert!(result.len() > TRACE_BYTES - 4);
}

#[test]
fn compressed_group_roundtrip_and_limits() {
    let group = Value::Dict(vec![(Value::Int(2), Value::Dict(vec![]))]);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&marshal::dumps(&group)).unwrap();
    let compressed = encoder.finish().unwrap();
    assert_eq!(marshal::loads(&crate::framing::inflate(&compressed).unwrap()).unwrap(), group);
    assert!(crate::framing::inflate(b"not zlib").is_err());
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&vec![0; crate::framing::MAX_PACKET + 1]).unwrap();
    assert!(crate::framing::inflate(&encoder.finish().unwrap()).is_err());
}

#[test]
fn payload_validation_boundaries() {
    let valid = Value::tuple([Value::Int(1), Value::str("x".repeat(MAX_STACK_CHARS))]);
    assert!(stack(&valid).is_some());
    assert!(stack(&Value::tuple([Value::Int(1), Value::str("x".repeat(MAX_STACK_CHARS + 1))])).is_none());
    assert!(stack(&Value::list([Value::Int(1), Value::str("x")])).is_none());
    assert_eq!(count(&Value::Int(MAX_COUNT)), Some(MAX_COUNT));
    assert_eq!(count(&Value::Int(MAX_COUNT + 1)), None);
    assert_eq!(seconds(i64::MIN), 0);
    assert!(seconds(i64::MAX) <= seconds(filetime_now()));
    let valid_row = Value::tuple([Value::Int(1), Value::Int(0)]);
    let bad = Value::Dict(vec![(Value::Int(1), valid_row.clone()), (Value::Int(2), Value::None)]);
    assert!(batch(&bad).is_none());
    assert!(batch(&Value::Dict(vec![(Value::Int(1), valid_row); MAX_RECORDS + 1])).is_none());
    let detail = |arg| Value::Dict(vec![(Value::Int(1), Value::Dict(vec![(Value::Int(1), Value::tuple([
        Value::Int(1), Value::Int(0), Value::None, Value::None, Value::None, arg, Value::None
    ]))]))]);
    assert!(group(&detail(Value::str("x".repeat(MAX_ARGUMENT_BYTES)))).is_some());
    assert!(group(&detail(Value::str("x".repeat(MAX_ARGUMENT_BYTES + 1)))).is_none());
    assert!(group(&detail(Value::Int(1))).is_none());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn dispatch_protocol_smoke() {
    crate::tests::init_config();
    let log_dir = std::env::temp_dir().join(format!("client-error-smoke-{}", std::process::id()));
    let logging = crate::config::deployment::LoggingConfig { dir: log_dir.to_string_lossy().into_owned(), rotation: "daily".into(), ..Default::default() };
    let guards = crate::logging::init(&logging).unwrap();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let own = crate::services::tests::fixture::create(&db).await.unwrap();
    let foreign = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut c, _peer) = crate::services::tests::client(db.clone(), 17).await;
    c.login.user_id = own.account;
    std::sync::Arc::make_mut(&mut c.cfg).telemetry.client_error_log_mode = "D".into();
    let hash = -i64::from(std::process::id()) - 4_000_000_000;
    let args = [Value::Int(hash)];
    assert_eq!(dispatch(&mut c, &call("BeanCount", &args)).await.unwrap().unwrap(), Value::tuple([Value::None, Value::None]));
    let invalid = [Value::tuple([Value::Int(hash), Value::str("stack")]), Value::str("trace"), Value::str("invalid"), Value::None];
    assert_eq!(dispatch(&mut c, &call("SendClientStackTraceAlert", &invalid)).await.unwrap().unwrap(), Value::None);
    assert_eq!(dispatch(&mut c, &call("BeanCount", &args)).await.unwrap().unwrap(), Value::tuple([Value::None, Value::None]));
    let upload = [Value::tuple([Value::Int(hash), Value::str("stack")]), Value::str("first trace\nsecond"), Value::str("Error"), Value::None];
    let answer = dispatch(&mut c, &call("SendClientStackTraceAlert", &upload)).await.unwrap().unwrap();
    let id = answer.items().unwrap()[0].as_int().unwrap();
    assert_eq!(answer, Value::tuple([Value::Int(id), Value::str("D")]));
    let log_path = std::fs::read_dir(&log_dir).unwrap().map(|e| e.unwrap().path()).find(|p| p.file_name().unwrap().to_string_lossy().starts_with("client-errors")).unwrap();
    let filename = log_path.file_name().unwrap().to_str().unwrap();
    let date = filename.strip_prefix("client-errors.").and_then(|name| name.strip_suffix(".log")).expect("daily client error log filename");
    assert!(date.len() == 10 && date.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() }), "unexpected daily client error log filename: {filename}");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let text = std::fs::read_to_string(&log_path).unwrap_or_default();
            if text.lines().any(|line| line.starts_with("==== ") && line.contains(&format!("error_id={id} hash={hash} mode=Error")))
                && text.contains("first trace\nsecond\n\n") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("first upload header and traceback were not visible while logging guards were alive within one second");
    assert_eq!(dispatch(&mut c, &call("BeanCount", &args)).await.unwrap().unwrap(), answer);
    dispatch(&mut c, &call("SendClientStackTraceAlert", &upload)).await.unwrap().unwrap();
    let time = crate::client::filetime_now();
    let batch = [Value::Dict(vec![(Value::Int(id), Value::tuple([Value::Int(3), Value::Int(time)]))])];
    dispatch(&mut c, &call("BeanDelivery", &batch)).await.unwrap().unwrap();
    let detail = |ch| Value::tuple([Value::Int(2), Value::Int(time), ch, Value::None, Value::None, Value::str("args"), Value::None]);
    let group = Value::Dict(vec![(Value::Int(id), Value::Dict(vec![
        (Value::Int(own.account), detail(Value::None)),
        (Value::Int(own.account), detail(Value::Int(own.character))),
        (Value::Int(own.account), detail(Value::Int(foreign.character))),
        (Value::Int(foreign.account), detail(Value::None)),
    ]))]);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&marshal::dumps(&group)).unwrap();
    let group_args = [Value::Bytes(encoder.finish().unwrap())];
    dispatch(&mut c, &call("GroupBeanDelivery", &group_args)).await.unwrap().unwrap();
    let malformed = [Value::Dict(vec![(Value::Int(id), Value::tuple([Value::Int(1), Value::Int(time)])), (Value::Int(id), Value::None)])];
    dispatch(&mut c, &call("BeanDelivery", &malformed)).await.unwrap().unwrap();
    let oversized = [Value::Dict(vec![(Value::Int(id), Value::tuple([Value::Int(1), Value::Int(time)])); MAX_RECORDS + 1])];
    dispatch(&mut c, &call("BeanDelivery", &oversized)).await.unwrap().unwrap();
    let invalid_count = [Value::Dict(vec![(Value::Int(id), Value::tuple([Value::Int(MAX_COUNT + 1), Value::Int(time)]))])];
    dispatch(&mut c, &call("BeanDelivery", &invalid_count)).await.unwrap().unwrap();
    let bad_group = [Value::Dict(vec![(Value::Int(id), Value::Dict(vec![
        (Value::Int(own.account), detail(Value::None)), (Value::Int(own.account), Value::None),
    ]))])];
    dispatch(&mut c, &call("GroupBeanDelivery", &bad_group)).await.unwrap().unwrap();
    let oversized_detail = Value::tuple([Value::Int(1), Value::Int(time), Value::None, Value::None, Value::None, Value::str("x".repeat(MAX_ARGUMENT_BYTES + 1)), Value::None]);
    let oversized_group = [Value::Dict(vec![(Value::Int(id), Value::Dict(vec![
        (Value::Int(own.account), detail(Value::None)), (Value::Int(own.account), oversized_detail),
    ]))])];
    dispatch(&mut c, &call("GroupBeanDelivery", &oversized_group)).await.unwrap().unwrap();
    let many_group = [Value::Dict(vec![(Value::Int(id), Value::Dict(vec![(Value::Int(own.account), detail(Value::None)); MAX_RECORDS + 1]))])];
    dispatch(&mut c, &call("GroupBeanDelivery", &many_group)).await.unwrap().unwrap();
    let row = db.query_one("SELECT occurrences,traceback FROM game.client_errors WHERE error_id=$1", &[&id]).await.unwrap();
    assert_eq!(row.get::<_, i64>("occurrences"), 5);
    assert_eq!(row.get::<_, String>("traceback"), "first trace\nsecond");
    let reports: i64 = db.query_one("SELECT count(*) AS n FROM game.client_error_reports WHERE error_id=$1", &[&id]).await.unwrap().get("n");
    assert_eq!(reports, 2);
    let multiline_args = "first\nsecond\rthird";
    let multiline_detail = Value::tuple([Value::Int(1), Value::Int(time), Value::None, Value::None, Value::None, Value::str(multiline_args), Value::None]);
    let multiline_group = [Value::Dict(vec![(Value::Int(id), Value::Dict(vec![(Value::Int(own.account), multiline_detail)]))])];
    dispatch(&mut c, &call("GroupBeanDelivery", &multiline_group)).await.unwrap().unwrap();
    let stored: String = db.query_one("SELECT arguments FROM game.client_error_reports WHERE error_id=$1 AND arguments=$2", &[&id, &multiline_args]).await.unwrap().get("arguments");
    assert_eq!(stored, multiline_args);
    db.execute("UPDATE game.client_errors SET log_mode='B' WHERE error_id=$1", &[&id]).await.unwrap();
    let modes = [Value::Dict(vec![(Value::Int(hash), Value::tuple([Value::Int(id), Value::str("D")]))])];
    assert_eq!(dispatch(&mut c, &call("GetLogModeForError", &modes)).await.unwrap().unwrap(), Value::tuple([Value::Dict(vec![(Value::Int(hash), Value::str("B"))]), Value::list([])]));
    let same_mode = [Value::Dict(vec![(Value::Int(hash), Value::tuple([Value::Int(id + 1), Value::str("B")]))])];
    assert_eq!(dispatch(&mut c, &call("GetLogModeForError", &same_mode)).await.unwrap().unwrap(), Value::tuple([Value::Dict(vec![]), Value::list([])]));
    assert!(db.execute("UPDATE game.client_errors SET log_mode='X' WHERE error_id=$1", &[&id]).await.is_err());
    let max_count = [Value::Dict(vec![(Value::Int(id), Value::tuple([Value::Int(MAX_COUNT), Value::Int(time)]))])];
    dispatch(&mut c, &call("BeanDelivery", &max_count)).await.unwrap().unwrap();
    let count_after_max: i64 = db.query_one("SELECT occurrences FROM game.client_errors WHERE error_id=$1", &[&id]).await.unwrap().get("occurrences");
    assert_eq!(count_after_max, 5 + MAX_COUNT);
    let mut before: f64 = db.query_one("SELECT extract(epoch from last_seen)::float8 AS t FROM game.client_errors WHERE error_id=$1", &[&id]).await.unwrap().get("t");
    for extreme in [i64::MIN, i64::MAX] {
        let batch = [Value::Dict(vec![(Value::Int(id), Value::tuple([Value::Int(1), Value::Int(extreme)]))])];
        dispatch(&mut c, &call("BeanDelivery", &batch)).await.unwrap().unwrap();
        let current = db.query_one("SELECT extract(epoch from last_seen)::float8 AS t, last_seen <= now() AS not_future FROM game.client_errors WHERE error_id=$1", &[&id]).await.unwrap();
        let timestamp: f64 = current.get("t");
        assert!(timestamp >= before && current.get::<_, bool>("not_future"));
        before = timestamp;
        let timed_detail = Value::tuple([Value::Int(1), Value::Int(extreme), Value::None, Value::None, Value::None, Value::str("extreme"), Value::None]);
        let timed_group = [Value::Dict(vec![(Value::Int(id), Value::Dict(vec![(Value::Int(own.account), timed_detail)]))])];
        dispatch(&mut c, &call("GroupBeanDelivery", &timed_group)).await.unwrap().unwrap();
    }
    let times = db.query_one("SELECT min(extract(epoch from reported_at))::float8 AS min_t, max(reported_at) <= now() AS not_future, max(reported_at) >= now() - interval '1 minute' AS recent, count(*) AS n FROM game.client_error_reports WHERE error_id=$1 AND arguments='extreme'", &[&id]).await.unwrap();
    assert_eq!(times.get::<_, i64>("n"), 2);
    assert_eq!(times.get::<_, f64>("min_t"), 0.0);
    assert!(times.get::<_, bool>("not_future"));
    assert!(times.get::<_, bool>("recent"));
    db.execute("UPDATE game.client_errors SET occurrences=$2 WHERE error_id=$1", &[&id, &i64::MAX]).await.unwrap();
    dispatch(&mut c, &call("BeanDelivery", &batch)).await.unwrap().unwrap();
    dispatch(&mut c, &call("SendClientStackTraceAlert", &upload)).await.unwrap().unwrap();
    let saturated: i64 = db.query_one("SELECT occurrences FROM game.client_errors WHERE error_id=$1", &[&id]).await.unwrap().get("occurrences");
    assert_eq!(saturated, i64::MAX);
    let crash = format!("smoke-crash-{hash}");
    let stats = [Value::None, Value::None, Value::None, Value::list([Value::str(&crash)])];
    dispatch(&mut c, &event(&stats)).await.unwrap().unwrap();
    let kwargs = Value::Dict(vec![(Value::str("isShutdown"), Value::Bool(true))]);
    let shutdown = Call { kwargs: &kwargs, ..event(&stats[..3]) };
    dispatch(&mut c, &shutdown).await.unwrap().unwrap();
    let invalid_crashes = [Value::None, Value::None, Value::None, Value::list([Value::str(&crash), Value::str("x".repeat(64 * 1024 + 1))])];
    dispatch(&mut c, &event(&invalid_crashes)).await.unwrap().unwrap();
    let many_crashes = [Value::None, Value::None, Value::None, Value::list(vec![Value::str(&crash); MAX_RECORDS + 1])];
    dispatch(&mut c, &event(&many_crashes)).await.unwrap().unwrap();
    let malformed_crashes = [Value::None, Value::None, Value::None, Value::Int(1)];
    dispatch(&mut c, &event(&malformed_crashes)).await.unwrap().unwrap();
    let crash_debug = format!("{:?}", Value::str(&crash));
    let n: i64 = db.query_one("SELECT count(*) AS n FROM game.client_crashes WHERE crash=$1", &[&crash_debug]).await.unwrap().get("n");
    assert_eq!(n, 1);
    db.execute("DELETE FROM game.client_crashes WHERE crash=$1", &[&crash_debug]).await.unwrap();
    let long_trace = format!("first\n{}end", "中".repeat(LOG_TRACE_BYTES / 3 + 2));
    let long_upload = [Value::tuple([Value::Int(hash), Value::str("stack")]), Value::str(&long_trace), Value::str("Warning"), Value::None];
    dispatch(&mut c, &call("SendClientStackTraceAlert", &long_upload)).await.unwrap().unwrap();
    drop(guards);
    let text = std::fs::read_to_string(log_path).unwrap();
    assert!(text.contains("first trace\nsecond\n\n"));
    let (header, body) = text.rsplit_once("==== ").unwrap().1.split_once('\n').unwrap();
    assert!(header.contains("truncated=true"));
    let trace_body = body.strip_suffix("\n\n").unwrap();
    assert_eq!(trace_body, tail(&long_trace, LOG_TRACE_BYTES));
    assert!(trace_body.len() <= LOG_TRACE_BYTES && trace_body.len() > LOG_TRACE_BYTES - 4);
    assert!(text.contains("count=2 args=args"));
    assert!(text.lines().any(|line| line.starts_with("---- ") && line.ends_with(&format!("error_id={id} count=1 args=first\\nsecond\\rthird"))));
    assert!(!text.contains("args=first\nsecond\rthird"));
    let server_path = std::fs::read_dir(&log_dir).unwrap().map(|e| e.unwrap().path()).find(|p| p.file_name().unwrap().to_string_lossy().starts_with("server")).unwrap();
    let server_log = std::fs::read_to_string(server_path).unwrap();
    let new_errors: Vec<_> = server_log.lines().filter(|line| line.contains("new client error") && line.contains(&format!("hash={hash}"))).collect();
    assert_eq!(new_errors.len(), 1);
    assert!(new_errors[0].contains("first trace"));
    assert!(!server_log.contains("second"));
    std::fs::remove_dir_all(log_dir).unwrap();
    db.execute("DELETE FROM game.client_error_reports WHERE error_id=$1", &[&id]).await.unwrap();
    db.execute("DELETE FROM game.client_errors WHERE error_id=$1", &[&id]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, own).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, foreign).await.unwrap();
}
