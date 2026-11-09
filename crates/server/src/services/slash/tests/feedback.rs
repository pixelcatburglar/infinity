//! Slash RPC results and their private chat publication.
use super::*;
use base64::Engine;
use std::time::Duration;
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}};

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) { self.0.abort(); }
}

struct TempDir(std::path::PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

async fn read_until(stream: &mut TcpStream, end: &str) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        while !bytes.ends_with(end.as_bytes()) {
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).await.unwrap(), 1);
            bytes.push(byte[0]);
        }
        String::from_utf8(bytes).unwrap()
    }).await.unwrap()
}

async fn receive_text(stream: &mut TcpStream, expected: &str) {
    let mut text = String::new();
    while text.len() < expected.len() {
        let stanza = read_until(stream, "</message>").await;
        if stanza.contains("<invite ") { continue; }
        let body = stanza.split_once("<body>").unwrap().1.split_once("</body>").unwrap().0;
        let json = quick_xml::escape::unescape(body).unwrap();
        let payload: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(payload["charid"], infinity_core::consts::OWNER_UNOWNED);
        let rendered = payload["messageText"].as_str().unwrap().replace("<br>", "\n");
        text.push_str(&quick_xml::escape::unescape(&rendered).unwrap());
    }
    assert_eq!(text, expected);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, external listener flock, and an isolated test process for Lua HOST"]
async fn slash_rpc_feedback_preserves_wire_and_autocomplete_is_silent() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let id = fixture.character;
    let (mut client, _peer) = crate::services::tests::client(test_db, id).await;
    client.login.user_id = fixture.account;
    client.login.role = GM_ROLES;
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).expect("SDE station");
    let station_id = station.station_id;
    let system_id = station.solar_system_id.unwrap();
    let type_id = client.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP))
        .map(|t| t.type_id).min().expect("published SDE ship");
    client.db.execute("UPDATE game.characters SET station_id=$2, solar_system_id=$3 WHERE character_id=$1", &[&id, &station_id, &system_id]).await.unwrap();
    client.session.apply(vec![("charid", Value::None)]);
    client.change_session(vec![("userid", Value::Int(fixture.account)), ("charid", Value::Int(id)), ("stationid", Value::Int(station_id)), ("solarsystemid2", Value::Int(system_id))]);
    let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let hub = client.chat.clone();
    let token = hub.token_for(id);
    let _server = Server(tokio::spawn(async move { crate::chat::serve(address.to_string(), hub).await.unwrap() }));
    let mut socket = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match TcpStream::connect(address).await {
                Ok(stream) => break stream,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    }).await.expect("chat listener did not start");
    socket.write_all(b"<stream:stream xmlns:stream='http://etherx.jabber.org/streams'>").await.unwrap();
    read_until(&mut socket, "</stream:features>").await;
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("\0{id}\0{token}"));
    socket.write_all(format!("<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{auth}</auth>").as_bytes()).await.unwrap();
    assert!(read_until(&mut socket, "/>").await.contains("<success"));
    socket.write_all(b"<stream:stream xmlns:stream='http://etherx.jabber.org/streams'>").await.unwrap();
    read_until(&mut socket, "</stream:features>").await;
    socket.write_all(b"<iq type='set' id='bind'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><resource>test</resource></bind></iq>").await.unwrap();
    assert!(read_until(&mut socket, "</iq>").await.contains("type='result'"));

    let kwargs = Value::Dict(vec![]);
    for (line, role) in [("/help", GM_ROLES), ("/not-a-command", GM_ROLES), ("/\"", GM_ROLES), ("/help", 0), ("/spawn", GM_ROLES), ("/where", GM_ROLES)] {
        client.login.role = role;
        let args = [Value::unicode(line)];
        let call = Call { service: "slash", method: "SlashCmd", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let before = execute(&mut client, &call).await.unwrap();
        let expected = match &before {
            Ok(Value::Unicode(text)) => text.clone(),
            Err(CallError::User { dict, .. }) => match &dict[0].1 { Value::Unicode(text) => text.clone(), _ => panic!("expected text") },
            _ => panic!("unexpected result"),
        };
        let original = support::wire(before);
        let after = support::rpc(&mut client, line).await;
        assert_eq!(support::wire(after), original, "{line}");
        receive_text(&mut socket, &expected).await;
    }
    for line in ["/", "///", "  /  "] {
        let args = [Value::unicode(line)];
        let call = Call { service: "slash", method: "SlashCmd", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let before = support::wire(execute(&mut client, &call).await.unwrap());
        assert_eq!(support::wire(dispatch(&mut client, &call).await.unwrap()), before);
        assert!(tokio::time::timeout(Duration::from_millis(100), socket.read_u8()).await.is_err());
    }
    client.login.role = GM_ROLES;
    let line = format!("/spawn {type_id}");
    let args = [Value::unicode(line)];
    let call = Call { service: "slash", method: "SlashCmd", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    let before_count: i64 = client.db.query_one("SELECT count(*) AS count FROM game.items WHERE owner_id=$1", &[&id]).await.unwrap().get("count");
    let spawned = dispatch(&mut client, &call).await.unwrap();
    let ship_id = match &spawned { Ok(Value::Int(id)) => *id, _ => panic!("spawn did not return an integer") };
    receive_text(&mut socket, &messages::gm::gm_spawn_feedback(ship_id)).await;
    let bytes = match &spawned { Ok(value) => marshal::dumps(value), _ => unreachable!() };
    super::super::feedback::publish(&client, &spawned);
    assert_eq!(match &spawned { Ok(value) => marshal::dumps(value), _ => unreachable!() }, bytes);
    assert_eq!(bytes, marshal::dumps(&Value::Int(ship_id)));
    receive_text(&mut socket, &messages::gm::gm_spawn_feedback(ship_id)).await;
    let rows = client.db.query("SELECT item_id,type_id,location_id,flag_id,singleton,quantity FROM game.items WHERE owner_id=$1", &[&id]).await.unwrap();
    assert_eq!(rows.len() as i64, before_count + 1);
    let row = rows.iter().find(|row| row.get::<_, i64>("item_id") == ship_id).expect("returned hull");
    assert_eq!(row.get::<_, i32>("type_id"), type_id);
    assert_eq!(row.get::<_, i64>("location_id"), station_id);
    assert_eq!(row.get::<_, i32>("flag_id"), infinity_core::consts::FLAG_HANGAR);
    assert!(row.get::<_, bool>("singleton"));
    assert_eq!(row.get::<_, i64>("quantity"), 1);
    let directory = TempDir(std::env::temp_dir().join(format!("slash-feedback-{}", std::process::id())));
    std::fs::create_dir(&directory.0).unwrap();
    std::fs::write(directory.0.join("feedback.lua"), r#"
        register.command{name='feedbackok', help='', gm=false, fn=function() return 'Lua 输出' end}
        register.command{name='feedbackfail', help='', gm=false, fn=function() error('private diagnostic') end}
    "#).unwrap();
    let mut config = crate::config::Config::default();
    config.lua.enabled = true;
    config.lua.scripts_dir = directory.0.to_string_lossy().into_owned();
    crate::lua::start(&config, &client.db, &client.sde, &client.world, &client.chat).await.unwrap();
    for (line, expected) in [("/feedbackok", "Lua 输出"), ("/feedbackfail", messages::gm::GM_FEEDBACK_ERROR)] {
        let args = [Value::unicode(line)];
        let call = Call { service: "slash", method: "SlashCmd", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let before = execute(&mut client, &call).await.unwrap();
        let after = dispatch(&mut client, &call).await.unwrap();
        match (before, after) {
            (Ok(before), Ok(after)) => assert_eq!(marshal::dumps(&before), marshal::dumps(&after)),
            (Err(CallError::Internal(_)), Err(CallError::Internal(_))) => {},
            _ => panic!("Lua result changed"),
        }
        receive_text(&mut socket, expected).await;
    }
    }).await;
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", crate::services::skills::completion::KIND, fixture.character)]).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    assert_eq!(crate::services::tests::fixture::remaining(&db, fixture).await.unwrap(), 0);
    result.expect("slash feedback test task failed");
}
