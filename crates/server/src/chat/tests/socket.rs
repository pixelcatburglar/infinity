//! Real XMPP stream authentication and admission checks.
use super::fixture;
use crate::{chat::{connection, Hub}, dao::chat_channels as dao, db::Db, sde::Sde, space::World};
use base64::Engine;
use marshal::Value;
use std::{sync::Arc, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}, task::JoinHandle};

pub(super) struct Tasks(pub(super) Vec<JoinHandle<()>>);
impl Drop for Tasks { fn drop(&mut self) { for task in &self.0 { task.abort(); } } }

pub(super) async fn read_until(stream: &mut TcpStream, needle: &str) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut text = Vec::new();
        while !text.ends_with(needle.as_bytes()) {
            let mut byte = [0; 1];
            let n = stream.read(&mut byte).await.unwrap();
            assert_ne!(n, 0, "socket closed before {needle}: {:?}", String::from_utf8_lossy(&text));
            text.push(byte[0]);
        }
        String::from_utf8(text).unwrap()
    }).await.expect("timed out waiting for XMPP reply")
}

pub(super) async fn open(listener: &TcpListener, hub: &Arc<Hub>, tasks: &mut Tasks, id: i64) -> TcpStream {
    let mut stream = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (accepted, _) = listener.accept().await.unwrap();
    let hub = hub.clone();
    let token = hub.token_for(id);
    tasks.0.push(tokio::spawn(async move { let _ = connection(accepted, hub).await; }));
    stream.write_all(b"<stream:stream xmlns:stream='http://etherx.jabber.org/streams'>").await.unwrap();
    read_until(&mut stream, "</stream:features>").await;
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("\0{id}\0{token}"));
    stream.write_all(format!("<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{auth}</auth>").as_bytes()).await.unwrap();
    assert!(read_until(&mut stream, "/>").await.contains("<success"));
    stream.write_all(b"<stream:stream xmlns:stream='http://etherx.jabber.org/streams'>").await.unwrap();
    read_until(&mut stream, "</stream:features>").await;
    stream.write_all(b"<iq type='set' id='bind'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><resource>test</resource></bind></iq>").await.unwrap();
    assert!(read_until(&mut stream, "</iq>").await.contains("type='result'"));
    stream
}

pub(super) async fn presence(stream: &mut TcpStream, room: &str, nick: i64, id: &str) -> String {
    stream.write_all(format!("<presence id='{id}' to='{room}/{nick}'/>").as_bytes()).await.unwrap();
    loop {
        let stanza = read_until(stream, "</presence>").await;
        if stanza.contains(&format!("id='{id}'")) { return stanza; }
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn authenticated_socket_rejects_forged_rooms_without_occupants() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await;
    let outsider = fixture::create(&db).await;
    let third = fixture::create(&db).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&test_db).await.unwrap());
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let hub = Hub::new("chat.test", test_db.clone(), world.clone());
        let (mut owner_client, _owner_game_socket) = fixture::client(test_db.clone(), sde.clone(), world.clone(), hub.clone()).await;
        let (mut outsider_client, _outsider_game_socket) = fixture::client(test_db.clone(), sde, world, hub.clone()).await;
        owner_client.change_session(vec![("charid", Value::Int(owner.id)), ("corpid", Value::Int(owner.id)), ("fleetid", Value::Int(owner.id))]);
        outsider_client.change_session(vec![("charid", Value::Int(outsider.id)), ("corpid", Value::Int(outsider.id)), ("fleetid", Value::Int(outsider.id))]);
        let private = dao::create_private(&test_db, owner.id, outsider.id).await.unwrap();
        let restricted_private = dao::create_private(&test_db, owner.id, third.id).await.unwrap();
        // A separate, server-created room excludes the outsider.
        let closed = dao::create_player(&test_db, owner.id, &format!("Socket room {}", owner.id)).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut tasks = Tasks(Vec::new());
        let mut owner_socket = open(&listener, &hub, &mut tasks, owner.id).await;
        let mut outsider_socket = open(&listener, &hub, &mut tasks, outsider.id).await;
        let own_room = format!("{}@conference.chat.test", closed.name);
        assert!(presence(&mut owner_socket, &own_room, owner.id, "owner").await.contains("code='110'"));
        let corp_room = format!("corp_{}@conference.chat.test", owner.id);
        assert!(presence(&mut owner_socket, &corp_room, owner.id, "current").await.contains("code='110'"));
        owner_client.change_session(vec![("corpid", Value::Int(third.id))]);
        {
            let rooms = hub.rooms.lock().unwrap();
            assert!(!rooms.entries.contains_key(&corp_room));
            assert!(rooms.entries.get(&own_room).unwrap().contains_key(&owner.id.to_string()));
        }
        let forged = [format!("corp_{}", owner.id), format!("fleet_{}", owner.id), closed.name.clone(), restricted_private.name.clone()];
        for (index, name) in forged.iter().enumerate() {
            let room = format!("{name}@conference.chat.test");
            let response = presence(&mut outsider_socket, &room, outsider.id, &format!("deny{index}")).await;
            assert!(response.contains("type='error'") && response.contains("forbidden"), "{response}");
            let rooms = hub.rooms.lock().unwrap();
            if name == &closed.name { assert_eq!(rooms.entries.get(&room).unwrap().len(), 1); assert!(!rooms.entries.get(&room).unwrap().contains_key(&outsider.id.to_string())); }
            else { assert!(!rooms.entries.contains_key(&room)); }
        }
        let malformed = format!("corp_{}@conference.chat.test.evil", outsider.id);
        assert!(presence(&mut outsider_socket, &malformed, outsider.id, "domain").await.contains("type='error'"));
        let noncanonical = format!("corp_0{}@conference.chat.test", outsider.id);
        assert!(presence(&mut outsider_socket, &noncanonical, outsider.id, "numeric").await.contains("type='error'"));
        outsider_socket.write_all(format!("<message id='send' to='{own_room}' type='groupchat'><body>forged</body></message>").as_bytes()).await.unwrap();
        assert!(read_until(&mut outsider_socket, "</message>").await.contains("type='error'"));
        outsider_socket.write_all(format!("<presence id='password' to='corp_{}@conference.chat.test/{}'><x xmlns='http://jabber.org/protocol/muc'><password>secret</password></x></presence>", outsider.id, outsider.id).as_bytes()).await.unwrap();
        let password = read_until(&mut outsider_socket, "</presence>").await;
        assert!(password.contains("type='error'") && password.contains(crate::messages::chat::UNSUPPORTED_PASSWORD));
        let response = presence(&mut outsider_socket, &format!("{}@conference.chat.test", private.name), owner.id, "impersonate").await;
        assert!(response.contains("type='error'"));
        assert!(!hub.rooms.lock().unwrap().entries.contains_key(&format!("{}@conference.chat.test", private.name)));
        outsider_client.change_session(vec![("charid", Value::None)]);
        let disconnected = presence(&mut outsider_socket, &format!("corp_{}@conference.chat.test", outsider.id), outsider.id, "disconnected").await;
        assert!(disconnected.contains("type='error'") && disconnected.contains("not-authorized"));
    }).await;
    fixture::cleanup(&db, &[owner, outsider, third]).await;
    result.unwrap();
}

pub(super) async fn iq_request(stream: &mut TcpStream, id: &str, to: &str, kind: &str, query: &str) -> String {
    stream.write_all(format!("<iq type='{kind}' id='{id}' to='{to}'>{query}</iq>").as_bytes()).await.unwrap();
    let reply = read_until(stream, "</iq>").await;
    assert!(reply.contains(&format!("id='{id}'")), "{reply}");
    reply
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn socket_expiry_role_display_and_destroy() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await;
    let guest = fixture::create(&db).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&test_db).await.unwrap());
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let hub = Hub::new("chat.test", test_db.clone(), world.clone());
        let (mut owner_client, _owner_peer) = fixture::client(test_db.clone(), sde.clone(), world.clone(), hub.clone()).await;
        let (mut guest_client, _guest_peer) = fixture::client(test_db.clone(), sde, world, hub.clone()).await;
        owner_client.change_session(vec![("charid", Value::Int(owner.id))]);
        guest_client.change_session(vec![("charid", Value::Int(guest.id))]);
        let channel = dao::create_player(&test_db, owner.id, &format!("SocketDisplay{}", owner.id)).await.unwrap();
        let room = format!("{}@conference.chat.test", channel.name);
        let session = crate::session::OnlineSession { character_id: owner.id, station_id: None, system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 };
        crate::services::chat::authority::configure(&test_db, &hub, &session, &channel.name, crate::services::chat::authority::RoomConfig { display_name: channel.display_name.clone(), public: true, moderated: true, members_by_default: false }).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut tasks = Tasks(Vec::new());
        let mut a = open(&listener, &hub, &mut tasks, owner.id).await;
        let mut b = open(&listener, &hub, &mut tasks, guest.id).await;
        assert!(presence(&mut a, &room, owner.id, "join-owner").await.contains("code='110'"));
        assert!(presence(&mut b, &room, guest.id, "join-guest").await.contains("code='110'"));
        let form = iq_request(&mut a, "form", &room, "get", "<query xmlns='http://jabber.org/protocol/muc#owner'/>").await;
        assert!(form.contains("var='muc#roomconfig_membersonly' type='boolean'><value>0</value>"), "{form}");
        assert!(form.contains("var='muc#roomconfig_moderatedroom' type='boolean'><value>1</value>"), "{form}");
        let info = iq_request(&mut a, "info", "conference.chat.test", "get", &format!("<query xmlns='http://jabber.org/protocol/disco#info' node='{}'/>", channel.name)).await;
        assert!(info.contains(&format!("name='{}'", channel.display_name)), "{info}");
        let list = iq_request(&mut a, "forme", "conference.chat.test", "get", "<query xmlns='http://jabber.org/protocol/disco#items' node='forme'/>").await;
        assert!(list.contains(&room), "{list}");
        let byname = iq_request(&mut a, "byname", "conference.chat.test", "get", &format!("<query xmlns='http://jabber.org/protocol/disco#items' node='byname/{}'/>", channel.display_name)).await;
        assert!(byname.contains(&room), "{byname}");
        let visitors = iq_request(&mut a, "visitors", &room, "get", "<query xmlns='http://jabber.org/protocol/muc#admin'><item role='visitor'/></query>").await;
        assert!(visitors.contains(&format!("{}@chat.test/test", guest.id)), "{visitors}");
        let norole = iq_request(&mut a, "norole", &room, "get", "<query xmlns='http://jabber.org/protocol/disco#items' node='norole'/>").await;
        assert!(norole.contains(&format!("name='{}'", guest.id)), "{norole}");
        assert!(dao::member(&test_db, &channel.name, guest.id).await.unwrap().is_none());
        let role = |value: &str| format!("<query xmlns='http://jabber.org/protocol/muc#admin'><item jid='{}@chat.test' role='{value}'/></query>", guest.id);
        let expiry = |action: &str, kind: &str, duration: &str| format!("<query xmlns='urn:xmpp:expiring_record' room='{room}' jid='{}@chat.test' category='{kind}' action='{action}' {duration}/>", guest.id);
        assert!(iq_request(&mut a, "absent", "chat.test", "set", &expiry("set", "mute", "duration='60'")).await.contains("type='error'"));
        assert!(iq_request(&mut a, "mute", &room, "set", &role("visitor")).await.contains("type='result'"));
        assert!(iq_request(&mut a, "expiry", "chat.test", "set", &expiry("set", "mute", "duration='315360000'")).await.contains("type='result'"));
        assert!(dao::member(&test_db, &channel.name, guest.id).await.unwrap().unwrap().voice_expires_at.is_some());
        let job = format!("chat-restriction:mute:{}:{}", channel.name, guest.id);
        assert!(test_db.query_opt("SELECT job_id FROM game.jobs WHERE job_id = $1", &[&job]).await.unwrap().is_some());
        let search = iq_request(&mut a, "search", "chat.test", "get", &format!("<query xmlns='urn:xmpp:expiring_record#search' room='{room}' category='mute'/>")).await;
        assert!(search.contains(&format!("jid='{}@chat.test'", guest.id)), "{search}");
        assert!(iq_request(&mut a, "unmute", &room, "set", &role("participant")).await.contains("type='result'"));
        assert!(dao::member(&test_db, &channel.name, guest.id).await.unwrap().unwrap().voice_expires_at.is_some());
        assert!(test_db.query_opt("SELECT job_id FROM game.jobs WHERE job_id = $1", &[&job]).await.unwrap().is_some());
        let guest_session = hub.world.online_session(guest.id).unwrap();
        assert_eq!(crate::services::chat::authority::admit(&test_db, hub.world.sde(), &guest_session, &channel.name).await.unwrap().unwrap().role, "visitor");
        assert!(iq_request(&mut a, "delete", "chat.test", "set", &expiry("delete", "mute", "")).await.contains("type='result'"));
        assert!(iq_request(&mut a, "delete-again", "chat.test", "set", &expiry("delete", "mute", "")).await.contains("type='result'"));
        assert_eq!(crate::services::chat::authority::admit(&test_db, hub.world.sde(), &guest_session, &channel.name).await.unwrap().unwrap().role, "participant");
        assert!(dao::member(&test_db, &channel.name, guest.id).await.unwrap().unwrap().voice_expires_at.is_none());
        assert!(test_db.query_opt("SELECT job_id FROM game.jobs WHERE job_id = $1", &[&job]).await.unwrap().is_none());
        for (id, query) in [("badkind", expiry("set", "other", "duration='60'")), ("noduration", expiry("set", "mute", ""))] {
            assert!(iq_request(&mut a, id, "chat.test", "set", &query).await.contains("type='error'"));
        }
        let ban = format!("<query xmlns='http://jabber.org/protocol/muc#admin'><item jid='{}@chat.test' affiliation='outcast'/></query>", guest.id);
        assert!(iq_request(&mut a, "ban", &room, "set", &ban).await.contains("type='result'"));
        assert!(iq_request(&mut a, "ban-expiry", "chat.test", "set", &expiry("set", "ban", "duration='60'")).await.contains("type='result'"));
        assert!(dao::member(&test_db, &channel.name, guest.id).await.unwrap().unwrap().ban_expires_at.is_some());
        let unban = format!("<query xmlns='http://jabber.org/protocol/muc#admin'><item jid='{}@chat.test' affiliation='none'/></query>", guest.id);
        assert!(iq_request(&mut a, "unban", &room, "set", &unban).await.contains("type='result'"));
        assert!(dao::member(&test_db, &channel.name, guest.id).await.unwrap().unwrap().ban_expires_at.is_some());
        assert!(crate::services::chat::authority::admit(&test_db, hub.world.sde(), &guest_session, &channel.name).await.unwrap().is_none());
        assert!(presence(&mut b, &room, guest.id, "still-banned").await.contains("type='error'"));
        assert!(iq_request(&mut a, "ban-delete", "chat.test", "set", &expiry("delete", "ban", "")).await.contains("type='result'"));
        assert!(crate::services::chat::authority::admit(&test_db, hub.world.sde(), &guest_session, &channel.name).await.unwrap().is_some());
        assert!(iq_request(&mut b, "destroy-denied", &room, "set", "<query xmlns='http://jabber.org/protocol/muc#owner'><destroy/></query>").await.contains("type='error'"));
        assert!(dao::get(&test_db, &channel.name).await.unwrap().is_some());
        assert!(iq_request(&mut a, "destroy", &room, "set", "<query xmlns='http://jabber.org/protocol/muc#owner'><destroy/></query>").await.contains("type='result'"));
        assert!(dao::get(&test_db, &channel.name).await.unwrap().is_none());
        assert!(dao::members(&test_db, &channel.name).await.unwrap().is_empty());
        assert!(presence(&mut a, &room, owner.id, "rejoin").await.contains("type='error'"));
    }).await;
    let rows = db.query("SELECT name FROM game.chat_channels WHERE owner_id = $1", &[&owner.id]).await.unwrap();
    for row in rows {
        let name: String = row.get("name");
        for kind in ["mute", "ban"] {
            crate::scheduler::cancel(&db, &format!("chat-restriction:{kind}:{name}:{}", guest.id)).await.unwrap();
        }
    }
    fixture::cleanup(&db, &[owner, guest]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn private_first_message_invites_only_the_other_participant() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await;
    let target = fixture::create(&db).await;
    let third = fixture::create(&db).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&test_db).await.unwrap());
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let hub = Hub::new("chat.test", test_db.clone(), world.clone());
        let (mut owner_client, _owner_peer) = fixture::client(test_db.clone(), sde.clone(), world.clone(), hub.clone()).await;
        let (mut target_client, _target_peer) = fixture::client(test_db.clone(), sde.clone(), world.clone(), hub.clone()).await;
        let (mut third_client, _third_peer) = fixture::client(test_db.clone(), sde, world, hub.clone()).await;
        owner_client.change_session(vec![("charid", Value::Int(owner.id))]);
        target_client.change_session(vec![("charid", Value::Int(target.id))]);
        third_client.change_session(vec![("charid", Value::Int(third.id))]);
        let args = [Value::Int(target.id)];
        let kwargs = Value::Dict(vec![]);
        let name = crate::services::dispatch(&mut owner_client, &crate::services::Call {
            service: "XmppChatMgr", method: "CreatePrivateChannel", args: &args,
            kwargs: &kwargs, bound: false, bind_params: None,
        }).await.unwrap().as_text().unwrap().into_owned();
        let room = format!("{name}@conference.chat.test");
        let before = dao::get(&test_db, &name).await.unwrap().unwrap();
        assert_eq!(before.owner_id, owner.id);
        assert!(!before.public);
        assert_eq!(before.private_low, Some(owner.id.min(target.id)));
        assert_eq!(before.private_high, Some(owner.id.max(target.id)));
        assert!(dao::members(&test_db, &name).await.unwrap().is_empty());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut tasks = Tasks(Vec::new());
        let mut a = open(&listener, &hub, &mut tasks, owner.id).await;
        let mut b = open(&listener, &hub, &mut tasks, target.id).await;
        let mut c = open(&listener, &hub, &mut tasks, third.id).await;
        assert!(presence(&mut a, &room, owner.id, "join-a").await.contains("code='110'"));
        a.write_all(format!("<message to='{room}'><x xmlns='http://jabber.org/protocol/muc#user'><invite to='{}@chat.test'><reason>Private chat</reason></invite></x></message>", target.id).as_bytes()).await.unwrap();
        a.write_all(format!("<message to='{room}' type='groupchat'><body>first private message</body></message>").as_bytes()).await.unwrap();
        let invitation = read_until(&mut b, "</message>").await;
        assert!(invitation.contains(&format!("from='{room}'")) && invitation.contains("<invite "), "{invitation}");
        let first_fence = iq_request(&mut a, "first-fence", "chat.test", "get", "<ping xmlns='urn:xmpp:ping'/>").await;
        assert!(first_fence.contains("type='result'"), "{first_fence}");
        assert!(presence(&mut b, &room, target.id, "join-b").await.contains("code='110'"));
        a.write_all(format!("<message to='{room}' type='groupchat'><body>after joining</body></message>").as_bytes()).await.unwrap();
        loop {
            let message = read_until(&mut b, "</message>").await;
            assert!(!message.contains("first private message"), "unexpected history: {message}");
            if message.contains("after joining") {
                assert!(message.contains(&format!("from='{room}/{}'", owner.id)), "{message}");
                break;
            }
        }
        let after_invite = dao::get(&test_db, &name).await.unwrap().unwrap();
        assert_eq!(after_invite.owner_id, before.owner_id);
        assert_eq!(after_invite.public, before.public);
        assert_eq!(after_invite.private_low, before.private_low);
        assert_eq!(after_invite.private_high, before.private_high);
        assert!(dao::members(&test_db, &name).await.unwrap().is_empty());

        a.write_all(format!("<message id='third-invite' to='{room}'><x xmlns='http://jabber.org/protocol/muc#user'><invite to='{}@chat.test'><reason>Private chat</reason></invite></x></message>", third.id).as_bytes()).await.unwrap();
        loop {
            let message = read_until(&mut a, "</message>").await;
            if message.contains("id='third-invite'") {
                assert!(message.contains("type='error'") && message.contains("forbidden"), "{message}");
                break;
            }
        }
        let fence = iq_request(&mut c, "invite-fence", "chat.test", "get", "<ping xmlns='urn:xmpp:ping'/>").await;
        assert!(fence.contains("type='result'") && !fence.contains("<invite") && !fence.contains(&room), "{fence}");
        let denied = presence(&mut c, &room, third.id, "join-c").await;
        assert!(denied.contains("type='error'") && denied.contains("forbidden"), "{denied}");
        {
            let rooms = hub.rooms.lock().unwrap();
            let occupants = rooms.entries.get(&room).unwrap();
            assert_eq!(occupants.len(), 2);
            assert!(occupants.contains_key(&owner.id.to_string()));
            assert!(occupants.contains_key(&target.id.to_string()));
            assert!(!occupants.contains_key(&third.id.to_string()));
        }
        let after_denial = dao::get(&test_db, &name).await.unwrap().unwrap();
        assert_eq!(after_denial.owner_id, before.owner_id);
        assert_eq!(after_denial.public, before.public);
        assert_eq!(after_denial.private_low, before.private_low);
        assert_eq!(after_denial.private_high, before.private_high);
        assert!(dao::members(&test_db, &name).await.unwrap().is_empty());
    }).await;
    fixture::cleanup(&db, &[owner, target, third]).await;
    result.unwrap();
}
