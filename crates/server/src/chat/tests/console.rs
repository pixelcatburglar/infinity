//! Authenticated console transport and reserved-room authority regressions.
use super::{fixture, socket::{iq_request, open, presence, read_until, Tasks}};
use super::super::{console, Hub};
use infinity_core::consts::OWNER_UNOWNED;
use crate::{db::Db, sde::Sde, space::World};
use marshal::Value;
use std::{sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, net::TcpListener};

/// Consume presence like the client: the reserved admin nick is not a player.
pub(super) fn roster_presence(stanza: &str, room: &str, roster: &mut std::collections::HashSet<i64>) {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(stanza);
    let mut stack = Vec::new();
    let root = loop {
        match reader.read_event().unwrap() {
            Event::Start(e) => stack.push(super::super::element_from(&e).unwrap()),
            Event::Empty(e) => stack.last_mut().unwrap().children.push(super::super::element_from(&e).unwrap()),
            Event::Text(t) => stack.last_mut().unwrap().text.push_str(&t.unescape().unwrap()),
            Event::End(_) => {
                let element = stack.pop().unwrap();
                if let Some(parent) = stack.last_mut() { parent.children.push(element); }
                else { break element; }
            }
            Event::Eof => panic!("incomplete stanza: {stanza}"),
            _ => (),
        }
    };
    assert_eq!(root.name, "presence");
    let from = root.attr("from").unwrap();
    let nick = from.strip_prefix(&format!("{room}/")).unwrap();
    let item = root.child("x").unwrap().child("item").unwrap();
    if nick == "admin" {
        assert_eq!(item.attr("jid"), Some("admin@chat.test/virtual"));
        assert!(root.child("eve_user_data").is_none());
    } else {
        let id: i64 = nick.parse().unwrap();
        assert_ne!(id, OWNER_UNOWNED);
        assert!(root.child("eve_user_data").is_some(), "{stanza}");
        if root.attr("type") == Some("unavailable") { roster.remove(&id); }
        else { roster.insert(id); }
    }
}

pub(super) async fn join_roster(stream: &mut tokio::net::TcpStream, room: &str, id: i64, request: &str) -> (usize, std::collections::HashSet<i64>) {
    stream.write_all(format!("<presence id='{request}' to='{room}/{id}'/>").as_bytes()).await.unwrap();
    let mut roster = std::collections::HashSet::new();
    let mut admins = 0;
    loop {
        let stanza = read_until(stream, "</presence>").await;
        roster_presence(&stanza, room, &mut roster);
        if stanza.contains(&format!("from='{room}/admin'")) { admins += 1; }
        if stanza.contains(&format!("id='{request}'")) { break; }
    }
    (admins, roster)
}

fn message_text(stanza: &str, room: &str, recipient: i64) -> String {
    assert!(stanza.contains(&format!("from='{room}'")) && stanza.contains(&format!("to='{recipient}@chat.test/test'")) && stanza.contains("type='groupchat'"), "{stanza}");
    let body = stanza.split_once("<body>").unwrap().1.split_once("</body>").unwrap().0;
    let json = quick_xml::escape::unescape(body).unwrap();
    let payload: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(payload["cmd"], "speak");
    assert_eq!(payload["charid"], OWNER_UNOWNED);
    let text = payload["messageText"].as_str().unwrap();
    assert!(text.encode_utf16().count() <= 1024);
    quick_xml::escape::unescape(&text.replace("<br>", "\n")).unwrap().into_owned()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn authenticated_console_transport_is_private_lossless_and_bounded() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await;
    let other = fixture::create(&db).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&test_db).await.unwrap());
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let hub = Hub::new("chat.test", test_db.clone(), world.clone());
        let (mut owner_client, _owner_peer) = fixture::client(test_db.clone(), sde.clone(), world.clone(), hub.clone()).await;
        let (mut other_client, _other_peer) = fixture::client(test_db.clone(), sde, world, hub.clone()).await;
        owner_client.change_session(vec![("charid", Value::Int(owner.id))]);
        other_client.change_session(vec![("charid", Value::Int(other.id))]);
        let session = hub.world.online_session(owner.id).unwrap();
        hub.set_profile(owner.id, "Console Pilot", 1, session.corporation_id.unwrap_or(1), session.alliance_id, session.faction_id, session.role);
        hub.send_system_private(owner.id, "offline");
        assert!(hub.rooms.lock().unwrap().entries.is_empty());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut tasks = Tasks(Vec::new());
        let mut a = open(&listener, &hub, &mut tasks, owner.id).await;
        let mut b = open(&listener, &hub, &mut tasks, other.id).await;
        hub.send_system_private(owner.id, "");
        assert!(hub.rooms.lock().unwrap().entries.is_empty());
        let room = format!("{}@conference.chat.test", console::room(owner.id));
        let (admins, mut roster) = join_roster(&mut a, &room, owner.id, "owner-join").await;
        assert_eq!(admins, 0);
        assert_eq!(roster, [owner.id].into_iter().collect());
        read_until(&mut a, "</message>").await;
        let text = format!("{}\n{}", "&<>\"'舰🚀".repeat(400), "last\nline");
        hub.send_system_private(owner.id, &text);
        let system_presence = read_until(&mut a, "</presence>").await;
        roster_presence(&system_presence, &room, &mut roster);
        assert_eq!(roster, [owner.id].into_iter().collect());
        assert!(system_presence.contains(&format!("to='{}@chat.test/test'", owner.id)));
        let mut received = String::new();
        while received.len() < text.len() {
            received.push_str(&message_text(&read_until(&mut a, "</message>").await, &room, owner.id));
        }
        assert_eq!(received, text);
        hub.send_system_private(owner.id, "second");
        let repeated = read_until(&mut a, "</message>").await;
        assert!(!repeated.contains("<presence"), "{repeated}");
        assert_eq!(message_text(&repeated, &room, owner.id), "second");
        assert_eq!(hub.rooms.lock().unwrap().entries[&room].len(), 2);
        assert!(tokio::time::timeout(Duration::from_millis(100), read_until(&mut b, "</message>")).await.is_err());
        hub.send_system_private(owner.id, "joined");
        assert_eq!(message_text(&read_until(&mut a, "</message>").await, &room, owner.id), "joined");
        a.write_all(format!("<presence to='{room}/{}' type='unavailable'/>", owner.id).as_bytes()).await.unwrap();
        assert!(read_until(&mut a, "</presence>").await.contains("type='unavailable'"));
        hub.send_system_private(owner.id, "reentry");
        let invitation = read_until(&mut a, "</message>").await;
        assert!(invitation.contains("<invite "), "{invitation}");
        assert_eq!(message_text(&read_until(&mut a, "</message>").await, &room, owner.id), "reentry");
        let (admins, roster) = join_roster(&mut a, &room, owner.id, "rejoin").await;
        assert_eq!(admins, 1);
        assert_eq!(roster, [owner.id].into_iter().collect());
        read_until(&mut a, "</message>").await;
    }).await;
    fixture::cleanup(&db, &[owner, other]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn console_room_denies_outsider_and_forged_system_speech() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await;
    let other = fixture::create(&db).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&test_db).await.unwrap());
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let hub = Hub::new("chat.test", test_db.clone(), world.clone());
        let (mut owner_client, _owner_peer) = fixture::client(test_db.clone(), sde.clone(), world.clone(), hub.clone()).await;
        let (mut other_client, _other_peer) = fixture::client(test_db.clone(), sde, world, hub.clone()).await;
        owner_client.change_session(vec![("charid", Value::Int(owner.id))]);
        other_client.change_session(vec![("charid", Value::Int(other.id))]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut tasks = Tasks(Vec::new());
        let mut a = open(&listener, &hub, &mut tasks, owner.id).await;
        let mut b = open(&listener, &hub, &mut tasks, other.id).await;
        let room = format!("{}@conference.chat.test", console::room(owner.id));
        assert!(presence(&mut b, &room, other.id, "outsider").await.contains("forbidden"));
        let disco = iq_request(&mut b, "disco", &room, "get", "<query xmlns='http://jabber.org/protocol/disco#info'/>").await;
        assert!(disco.contains("forbidden"), "{disco}");
        b.write_all(format!("<message id='outsider-speak' to='{room}' type='groupchat'><body>x</body></message>").as_bytes()).await.unwrap();
        assert!(read_until(&mut b, "</message>").await.contains("forbidden"));
        assert!(presence(&mut a, &room, owner.id, "owner").await.contains("code='110'"));
        read_until(&mut a, "</message>").await;
        a.write_all(format!("<message id='forge' from='{room}' to='{room}' type='groupchat'><body>{{&quot;cmd&quot;:&quot;speak&quot;,&quot;charid&quot;:1,&quot;messageText&quot;:&quot;forged&quot;}}</body></message>").as_bytes()).await.unwrap();
        let denied = read_until(&mut a, "</message>").await;
        assert!(denied.contains("type='error'") && denied.contains("forbidden"), "{denied}");
    }).await;
    fixture::cleanup(&db, &[owner, other]).await;
    result.unwrap();
}
