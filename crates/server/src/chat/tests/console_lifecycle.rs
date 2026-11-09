//! Reserved console-room lifecycle regressions.
use super::{fixture, socket::{open, read_until, Tasks}};
use super::console::join_roster;
use super::super::{console, Hub, Occupant};
use crate::{client::Client, db::Db, sde::Sde, space::World};
use marshal::Value;
use std::{sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, net::TcpListener};

async fn setup(db: &Db, owner: fixture::Character) -> (Arc<Hub>, Client, TcpListener, Tasks) {
    let sde = Arc::new(Sde::load(db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let hub = Hub::new("chat.test", db.clone(), world.clone());
    let (mut client, _) = fixture::client(db.clone(), sde, world, hub.clone()).await;
    client.change_session(vec![("charid", Value::Int(owner.id))]);
    let session = hub.world.online_session(owner.id).unwrap();
    hub.set_profile(owner.id, "Console Pilot", 1, session.corporation_id.unwrap_or(1), session.alliance_id, session.faction_id, session.role);
    (hub, client, TcpListener::bind("127.0.0.1:0").await.unwrap(), Tasks(Vec::new()))
}

async fn wait_for_room_absent(hub: &Arc<Hub>, room: &str) {
    for _ in 0..100 {
        if !hub.rooms.lock().unwrap().entries.contains_key(room) { return; }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("room was not removed: {room}");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn console_pending_invite_socket_eof_removes_marker_and_reinvites() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (hub, _client, listener, mut tasks) = setup(&test_db, owner).await;
        let room = format!("{}@conference.chat.test", console::room(owner.id));
        hub.rooms.lock().unwrap().entries.insert("ordinary@conference.chat.test".into(), [
            ("ordinary".into(), Occupant {
                full_jid: "ordinary@chat.test/virtual".into(), tx: None,
                affiliation: "member".into(), role: "participant".into(),
            }),
        ].into_iter().collect());

        let mut socket = open(&listener, &hub, &mut tasks, owner.id).await;
        hub.send_system_private(owner.id, "first");
        assert!(read_until(&mut socket, "</message>").await.contains(&format!("from='{room}'")));
        assert!(read_until(&mut socket, "</message>").await.contains("messageText"));
        assert!(hub.rooms.lock().unwrap().entries[&room].contains_key(console::SYSTEM_NICK));
        let mut replacement = open(&listener, &hub, &mut tasks, owner.id).await;
        drop(socket);
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(hub.rooms.lock().unwrap().entries.contains_key(&room));
        hub.send_system_private(owner.id, "replacement");
        let replacement_reply = read_until(&mut replacement, "</message>").await;
        assert!(!replacement_reply.contains("<invite "));
        assert!(replacement_reply.contains("messageText"));
        drop(replacement);
        wait_for_room_absent(&hub, &room).await;
        assert!(hub.rooms.lock().unwrap().entries.contains_key("ordinary@conference.chat.test"));

        let mut fresh = open(&listener, &hub, &mut tasks, owner.id).await;
        hub.send_system_private(owner.id, "second");
        assert!(read_until(&mut fresh, "</message>").await.contains(&format!("from='{room}'")));
        assert!(read_until(&mut fresh, "</message>").await.contains("messageText"));
        assert!(hub.rooms.lock().unwrap().entries[&room].contains_key(console::SYSTEM_NICK));
        let (admins, roster) = join_roster(&mut fresh, &room, owner.id, "fresh-join").await;
        assert_eq!(admins, 1);
        assert_eq!(roster, [owner.id].into_iter().collect());
        read_until(&mut fresh, "</message>").await;
    }).await;
    fixture::cleanup(&db, &[owner]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn console_join_leave_disconnect_drops_stale_reply_and_reconnects() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (hub, _client, listener, mut tasks) = setup(&test_db, owner).await;
        let room = format!("{}@conference.chat.test", console::room(owner.id));
        let mut socket = open(&listener, &hub, &mut tasks, owner.id).await;
        hub.send_system_private(owner.id, "first");
        read_until(&mut socket, "</message>").await;
        read_until(&mut socket, "</message>").await;
        let (admins, roster) = join_roster(&mut socket, &room, owner.id, "join").await;
        assert_eq!(admins, 1);
        assert_eq!(roster, [owner.id].into_iter().collect());
        read_until(&mut socket, "</message>").await;
        hub.send_system_private(owner.id, "joined");
        assert!(read_until(&mut socket, "</message>").await.contains("messageText"));

        socket.write_all(format!("<presence id='leave' to='{room}/{}' type='unavailable'/>", owner.id).as_bytes()).await.unwrap();
        assert!(read_until(&mut socket, "</presence>").await.contains("type='unavailable'"));
        assert!(!hub.rooms.lock().unwrap().entries.contains_key(&room));

        hub.disconnect_character(owner.id);
        hub.send_system_private(owner.id, "stale");
        assert!(tokio::time::timeout(Duration::from_millis(100), read_until(&mut socket, "</message>")).await.is_err());
        drop(socket);

        let mut fresh = open(&listener, &hub, &mut tasks, owner.id).await;
        hub.send_system_private(owner.id, "reentry");
        assert!(read_until(&mut fresh, "</message>").await.contains(&format!("from='{room}'")));
        assert!(read_until(&mut fresh, "</message>").await.contains("messageText"));
        let (admins, roster) = join_roster(&mut fresh, &room, owner.id, "reconnect-join").await;
        assert_eq!(admins, 1);
        assert_eq!(roster, [owner.id].into_iter().collect());
        read_until(&mut fresh, "</message>").await;
    }).await;
    fixture::cleanup(&db, &[owner]).await;
    result.unwrap();
}
