//! Independent socket and durable authority regressions.
use super::{fixture, socket::{Tasks, open, presence, read_until, iq_request}};
use crate::{chat::{connection, Hub}, dao::chat_channels as dao, db::Db, sde::Sde, services::chat::authority::{self, RoomConfig}, space::World};
use base64::Engine;
use marshal::Value;
use std::sync::Arc;
use tokio::{io::AsyncWriteExt, net::{TcpListener, TcpStream}};

async fn stanza_containing(stream: &mut TcpStream, end: &str, needle: &str) -> String {
    for _ in 0..32 {
        let stanza = read_until(stream, end).await;
        if stanza.contains(needle) { return stanza; }
    }
    panic!("no stanza containing {needle}");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn session_categories_and_unbound_connections() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await;
    let other = fixture::create(&db).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&test_db).await.unwrap());
        let system = sde.map.systems().find(|s| s.solar_system_id > 0).unwrap().solar_system_id;
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let hub = Hub::new("chat.test", test_db.clone(), world.clone());
        let (mut client, _peer) = fixture::client(test_db.clone(), sde.clone(), world.clone(), hub.clone()).await;
        client.change_session(vec![("charid", Value::Int(actor.id)), ("solarsystemid2", Value::Int(system)), ("corpid", Value::Int(actor.id)), ("allianceid", Value::Int(actor.id)), ("warfactionid", Value::Int(actor.id)), ("fleetid", Value::Int(actor.id))]);
        let session = world.online_session(actor.id).unwrap();
        let channels = authority::system_channels(&sde, &session);
        assert_eq!(channels.len(), 5, "session facts: {session:?}");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut tasks = Tasks(Vec::new());
        let mut sock = open(&listener, &hub, &mut tasks, actor.id).await;
        for (i, name) in channels.iter().map(String::as_str).chain([authority::help(None), authority::ROOKIE]).enumerate() {
            let room = format!("{name}@conference.chat.test");
            assert!(presence(&mut sock, &room, actor.id, &format!("valid{i}")).await.contains("code='110'"));
        }
        let local = channels.iter().find(|c| c.ends_with(&format!("_{system}"))).unwrap();
        let alternate = if local.starts_with("local_") { "wormhole" } else { "local" };
        let forged = [format!("{alternate}_{system}"), format!("local_{}", other.id), format!("alliance_{}", other.id), format!("faction_{}", other.id), format!("alliance_+{}", actor.id), format!("faction_0{}", actor.id)];
        for (i, name) in forged.iter().enumerate() {
            let room = format!("{name}@conference.chat.test");
            assert!(presence(&mut sock, &room, actor.id, &format!("forged{i}")).await.contains("type='error'"));
            assert!(!hub.rooms.lock().unwrap().entries.contains_key(&room));
        }
        // Exercise each remap against actual SDE entries when present.
        let mut checked = std::collections::HashSet::new();
        for s in sde.map.systems() {
            let id = s.solar_system_id;
            if id <= 0 { continue; }
            let mut facts = session.clone(); facts.system_id = Some(id);
            let names = authority::system_channels(&sde, &facts);
            let expected = if (32_000_000..=32_999_999).contains(&id) || (34_000_000..=34_999_999).contains(&id) || (35_000_000..=35_000_100).contains(&id) || (36_000_000..=36_999_999).contains(&id) { "nolocal" } else if id == 30_100_000 || (crate::consts::WORMHOLE_SYSTEM_START..=31_999_999).contains(&id) { "wormhole" } else if s.faction_id == Some(500_026) { "triglavian" } else { "local" };
            assert_eq!(names.first().unwrap(), &format!("{expected}_{id}"));
            if checked.insert(expected) {
                assert!(authority::admit(&test_db, &sde, &facts, &names[0]).await.unwrap().is_some());
                let alternate = if expected == "local" { "wormhole" } else { "local" };
                assert!(authority::admit(&test_db, &sde, &facts, &format!("{alternate}_{id}")).await.unwrap().is_none());
            }
        }
        let mut raw = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        let raw_hub = hub.clone(); tasks.0.push(tokio::spawn(async move { let _ = connection(accepted, raw_hub).await; }));
        raw.write_all(b"<stream:stream xmlns:stream='http://etherx.jabber.org/streams'>").await.unwrap();
        read_until(&mut raw, "</stream:features>").await;
        let room = format!("alliance_{}@conference.chat.test", other.id);
        raw.write_all(format!("<presence id='unbound' to='{room}/{}'/>", actor.id).as_bytes()).await.unwrap();
        assert!(read_until(&mut raw, "</presence>").await.contains("not-authorized"));
        raw.write_all(format!("<message id='unbound-message' to='{room}' type='groupchat'><body>x</body></message>").as_bytes()).await.unwrap();
        assert!(read_until(&mut raw, "</message>").await.contains("not-authorized"));
        let token = hub.token_for(actor.id);
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("\0{}\0{token}", actor.id));
        raw.write_all(format!("<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{auth}</auth>").as_bytes()).await.unwrap();
        assert!(read_until(&mut raw, "/>").await.contains("<success"));
        raw.write_all(b"<stream:stream xmlns:stream='http://etherx.jabber.org/streams'>").await.unwrap();
        read_until(&mut raw, "</stream:features>").await;
        raw.write_all(format!("<presence id='before-bind' to='{room}/{}'/>", actor.id).as_bytes()).await.unwrap();
        assert!(read_until(&mut raw, "</presence>").await.contains("not-authorized"));
        raw.write_all(format!("<message id='before-bind-message' to='{room}' type='groupchat'><body>x</body></message>").as_bytes()).await.unwrap();
        assert!(read_until(&mut raw, "</message>").await.contains("not-authorized"));
        assert!(!hub.rooms.lock().unwrap().entries.contains_key(&room));
        let mut duplicate = open(&listener, &hub, &mut tasks, actor.id).await;
        let first = format!("{}@conference.chat.test", channels[0]);
        assert!(presence(&mut duplicate, &first, actor.id, "duplicate").await.contains("conflict"));
        drop(duplicate);
        // Synchronize teardown with the connection task, without a sleep.
        let task = tasks.0.pop().unwrap(); tokio::time::timeout(std::time::Duration::from_secs(5), task).await.unwrap().unwrap();
        assert!(hub.rooms.lock().unwrap().entries.get(&first).unwrap().contains_key(&actor.id.to_string()));
    }).await;
    fixture::cleanup(&db, &[actor, other]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn discovery_hides_closed_rooms_and_restart_preserves_metadata() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await; let guest = fixture::create(&db).await; let outsider = fixture::create(&db).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&test_db).await.unwrap()); let world = World::new(test_db.clone(), sde.clone(), 1); let hub = Hub::new("chat.test", test_db.clone(), world.clone());
        let (mut a, _a) = fixture::client(test_db.clone(), sde.clone(), world.clone(), hub.clone()).await;
        let (mut b, _b) = fixture::client(test_db.clone(), sde.clone(), world.clone(), hub.clone()).await;
        let (mut x, _x) = fixture::client(test_db.clone(), sde, world.clone(), hub.clone()).await;
        a.change_session(vec![("charid", Value::Int(owner.id)), ("corpid", Value::Int(owner.id)), ("fleetid", Value::Int(owner.id))]); b.change_session(vec![("charid", Value::Int(guest.id))]); x.change_session(vec![("charid", Value::Int(outsider.id))]);
        let c = dao::create_player(&test_db, owner.id, &format!("Hidden{}", owner.id)).await.unwrap();
        let private = dao::create_private(&test_db, owner.id, guest.id).await.unwrap();
        let s = world.online_session(owner.id).unwrap(); authority::invite(&test_db, &hub, &s, &c.name, guest.id).await.unwrap();
        let room = format!("{}@conference.chat.test", c.name); let corp = format!("corp_{}@conference.chat.test", owner.id); let fleet = format!("fleet_{}@conference.chat.test", owner.id);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap(); let mut tasks = Tasks(Vec::new());
        let mut os = open(&listener, &hub, &mut tasks, owner.id).await; let mut xs = open(&listener, &hub, &mut tasks, outsider.id).await;
        for (i, r) in [&room, &corp, &fleet].iter().enumerate() { assert!(presence(&mut os, r, owner.id, &format!("join{i}")).await.contains("code='110'")); }
        a.change_session(vec![("corpid", Value::Int(guest.id)), ("fleetid", Value::Int(guest.id))]);
        { let rooms = hub.rooms.lock().unwrap(); assert!(rooms.entries.contains_key(&room)); assert!(!rooms.entries.contains_key(&corp)); assert!(!rooms.entries.contains_key(&fleet)); }
        let list = iq_request(&mut xs, "forme-hidden", "conference.chat.test", "get", "<query xmlns='http://jabber.org/protocol/disco#items' node='forme'/>").await;
        assert!(!list.contains(&c.name) && !list.contains(&private.name));
        let byname = iq_request(&mut xs, "byname-hidden", "conference.chat.test", "get", &format!("<query xmlns='http://jabber.org/protocol/disco#items' node='byname/{}'/>", c.display_name)).await; assert!(!byname.contains(&c.name));
        for (i, name) in [&c.name, &private.name].iter().enumerate() {
            let target = format!("{name}@conference.chat.test");
            for (j, (to, query)) in [(target.as_str(), "<query xmlns='http://jabber.org/protocol/disco#info'/>".to_string()), (target.as_str(), "<query xmlns='http://jabber.org/protocol/disco#items' node='norole'/>".to_string()), ("conference.chat.test", format!("<query xmlns='http://jabber.org/protocol/disco#info' node='{name}'/>"))].iter().enumerate() {
                let reply = iq_request(&mut xs, &format!("hidden{i}-{j}"), to, "get", query).await; assert!(reply.contains("type='error'")); assert!(!reply.contains(&c.display_name));
            }
        }
        let restarted = Hub::new("chat.test", test_db.clone(), world.clone()); assert!(restarted.rooms.lock().unwrap().entries.is_empty());
        assert!(dao::get(&test_db, &c.name).await.unwrap().is_some()); assert!(dao::member(&test_db, &c.name, guest.id).await.unwrap().unwrap().invited);
        assert!(authority::admit(&test_db, world.sde(), &world.online_session(guest.id).unwrap(), &c.name).await.unwrap().is_some());
    }).await;
    fixture::cleanup(&db, &[owner, guest, outsider]).await; result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn moderated_messages_subject_rollback_and_manager_destroy() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await; let guest = fixture::create(&db).await; let outsider = fixture::create(&db).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&test_db).await.unwrap()); let world = World::new(test_db.clone(), sde.clone(), 1); let hub = Hub::new("chat.test", test_db.clone(), world.clone());
        let (mut a, _a) = fixture::client(test_db.clone(), sde.clone(), world.clone(), hub.clone()).await;
        let (mut b, _b) = fixture::client(test_db.clone(), sde.clone(), world.clone(), hub.clone()).await;
        let (mut x, _x) = fixture::client(test_db.clone(), sde, world.clone(), hub.clone()).await;
        a.change_session(vec![("charid", Value::Int(owner.id))]); b.change_session(vec![("charid", Value::Int(guest.id))]); x.change_session(vec![("charid", Value::Int(outsider.id))]);
        let c = dao::create_player(&test_db, owner.id, &format!("Moderated{}", owner.id)).await.unwrap();
        let other = dao::create_player(&test_db, owner.id, &format!("Duplicate{}", owner.id)).await.unwrap();
        let session = world.online_session(owner.id).unwrap();
        authority::configure(&test_db, &hub, &session, &c.name, RoomConfig { display_name: c.display_name.clone(), public: true, moderated: true, members_by_default: false }).await.unwrap();
        let room = format!("{}@conference.chat.test", c.name);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap(); let mut tasks = Tasks(Vec::new());
        let mut os = open(&listener, &hub, &mut tasks, owner.id).await; let mut gs = open(&listener, &hub, &mut tasks, guest.id).await; let mut xs = open(&listener, &hub, &mut tasks, outsider.id).await;
        assert!(presence(&mut os, &room, owner.id, "join-owner").await.contains("code='110'"));
        assert!(presence(&mut gs, &room, guest.id, "join-guest").await.contains("role='visitor'"));
        assert!(dao::member(&test_db, &c.name, guest.id).await.unwrap().is_none());
        let visitors = iq_request(&mut os, "visitor-list", &room, "get", "<query xmlns='http://jabber.org/protocol/muc#admin'><item role='visitor'/></query>").await;
        assert!(visitors.contains(&format!("{}@chat.test/test", guest.id)));
        xs.write_all(format!("<message id='nonoccupant' to='{room}' type='groupchat'><body>must-not-fan-out</body></message>").as_bytes()).await.unwrap();
        assert!(read_until(&mut xs, "</message>").await.contains("type='error'"));
        gs.write_all(format!("<message id='visitor' to='{room}' type='groupchat'><body>visitor-forbidden</body></message>").as_bytes()).await.unwrap();
        let denied = stanza_containing(&mut gs, "</message>", "id='visitor'").await;
        assert!(denied.contains("type='error'") && denied.contains("id='visitor'"));
        let fence = iq_request(&mut os, "fence", "chat.test", "get", "<ping xmlns='urn:xmpp:ping'/>").await;
        assert!(!fence.contains("must-not-fan-out") && !fence.contains("visitor-forbidden"));
        gs.write_all(format!("<message id='subject-denied' to='{room}' type='groupchat'><subject>bad</subject></message>").as_bytes()).await.unwrap();
        assert!(read_until(&mut gs, "</message>").await.contains("type='error'"));
        os.write_all(format!("<message to='{room}' type='groupchat'><subject>界 &amp; &lt;hello&gt; &quot;yes&quot;</subject></message>").as_bytes()).await.unwrap();
        let subject = stanza_containing(&mut gs, "</message>", "界 &amp; &lt;hello&gt;").await;
        assert!(subject.contains("界 &amp; &lt;hello&gt;") && subject.contains("yes"), "{subject}");
        assert_eq!(dao::get(&test_db, &c.name).await.unwrap().unwrap().motd, "界 & <hello> \"yes\"");
        let rename = format!("<query xmlns='http://jabber.org/protocol/muc#owner'><x xmlns='jabber:x:data' type='submit'><field var='FORM_TYPE'><value>http://jabber.org/protocol/muc#roomconfig</value></field><field var='muc#roomconfig_roomname'><value>{}</value></field><field var='muc#roomconfig_membersonly'><value>1</value></field></x></query>", other.display_name);
        assert!(iq_request(&mut os, "rollback", &room, "set", &rename).await.contains("type='error'"));
        let stored = dao::get(&test_db, &c.name).await.unwrap().unwrap();
        assert_eq!(stored.display_name, c.display_name); assert!(stored.public); assert_eq!(stored.motd, "界 & <hello> \"yes\"");
        { let rooms = hub.rooms.lock().unwrap(); let occupants = rooms.entries.get(&room).unwrap(); assert_eq!(occupants.len(), 2); assert_eq!(occupants.get(&guest.id.to_string()).unwrap().role, "visitor"); }
        let grant = format!("<query xmlns='http://jabber.org/protocol/muc#admin'><item jid='{}@chat.test' role='participant'/></query>", guest.id);
        assert!(iq_request(&mut os, "grant", &room, "set", &grant).await.contains("type='result'"));
        gs.write_all(format!("<message id='voiced' to='{room}' type='groupchat'><body>voiced &amp; heard</body></message>").as_bytes()).await.unwrap();
        let heard = stanza_containing(&mut os, "</message>", "voiced &amp; heard").await;
        assert!(heard.contains("voiced &amp; heard"), "{heard}");
        gs.write_all(format!("<presence type='unavailable' to='{room}/{}'/>", guest.id).as_bytes()).await.unwrap();
        read_until(&mut gs, "</presence>").await;
        assert!(presence(&mut gs, &room, guest.id, "rejoin").await.contains("code='110'"));
        let rejoined = stanza_containing(&mut gs, "</message>", "界 &amp; &lt;hello&gt;").await; assert!(rejoined.contains("界 &amp; &lt;hello&gt;"), "{rejoined}");
        let admin = format!("<query xmlns='http://jabber.org/protocol/muc#admin'><item jid='{}@chat.test' affiliation='admin'/></query>", guest.id);
        assert!(iq_request(&mut os, "admin", &room, "set", &admin).await.contains("type='result'"));
        let destroy = "<query xmlns='http://jabber.org/protocol/muc#owner'><destroy/></query>";
        assert!(iq_request(&mut xs, "destroy-denied", &room, "set", destroy).await.contains("type='error'"));
        assert_eq!(hub.rooms.lock().unwrap().entries.get(&room).unwrap().len(), 2);
        let destroyed = iq_request(&mut gs, "admin-destroy", &room, "set", destroy).await;
        assert!(destroyed.contains("type='result'") && destroyed.contains("type='unavailable'") && destroyed.contains("<destroy/>"), "{destroyed}");
        let notice = stanza_containing(&mut os, "</presence>", "<destroy/>").await;
        assert!(notice.contains("type='unavailable'") && notice.contains("<destroy/>"), "{notice}");
        assert!(dao::get(&test_db, &c.name).await.unwrap().is_none()); assert!(dao::members(&test_db, &c.name).await.unwrap().is_empty());
        assert!(presence(&mut gs, &room, guest.id, "destroyed-rejoin").await.contains("type='error'"));
    }).await;
    fixture::cleanup(&db, &[owner, guest, outsider]).await; result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn schema_identity_constraints_and_transaction_rollback() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await; let other = fixture::create(&db).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let room = dao::create_player(&test_db, owner.id, &format!("Schema{}", owner.id)).await.unwrap();
        let low = owner.id.min(other.id); let high = owner.id.max(other.id);
        let sql = "INSERT INTO game.chat_channels (name,category,display_name,owner_id,private_low,private_high,public) VALUES ($1,$2,$3,$4,$5,$6,$7)";
        let cases = [
            ("invalid_1".to_string(), "unknown", None, None, false),
            ("player_01".to_string(), "player", None, None, false),
            ("player_0".to_string(), "player", None, None, false),
            (format!("private_{high}_{low}"), "private", Some(high), Some(low), false),
            (format!("private_{low}_{high}"), "private", Some(low), Some(high), true),
            (format!("private_{low}_{}", high + 1), "private", Some(low), Some(high), false),
        ];
        for (name, category, a, b, public) in cases {
            assert!(test_db.execute(sql, &[&name, &category, &"invalid", &owner.id, &a, &b, &public]).await.is_err(), "accepted {name}");
        }
        let name = room.name.clone();
        let failure: Result<(), crate::db::Error> = test_db.transaction(|tx, _: &mut ()| {
            let name = name.clone();
            Box::pin(async move {
                tx.execute("UPDATE game.chat_channels SET public = true WHERE name = $1", &[&name]).await?;
                tx.execute("INSERT INTO game.chat_channels (name,category,display_name,owner_id) VALUES ('player_0','player','bad',$1)", &[&owner.id]).await?;
                Ok(())
            })
        }, |v, ()| async move { Ok(v) }).await;
        assert!(failure.is_err()); assert!(!dao::get(&test_db, &room.name).await.unwrap().unwrap().public);
    }).await;
    fixture::cleanup(&db, &[owner, other]).await; result.unwrap();
}

async fn set_motd(c: &mut crate::client::Client, room: &str, value: &str) -> Result<Value, crate::services::CallError> {
    let args = [Value::unicode(room), Value::unicode(value)];
    let kwargs = Value::Dict(vec![]);
    crate::services::dispatch(c, &crate::services::Call { service: "XmppChatMgr", method: "SetMotd", args: &args, kwargs: &kwargs, bound: false, bind_params: None }).await
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn corporation_and_alliance_motd_require_real_owning_authority() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await; let director = fixture::create(&db).await; let ordinary = fixture::create(&db).await; let foreign = fixture::create(&db).await;
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('chat-motd-' || nextval('game.corporations_corporation_id_seq')::text, 'CM' || currval('game.corporations_corporation_id_seq')::text, $1,$1,1,1) RETURNING corporation_id", &[&ceo.id]).await.unwrap().get("corporation_id");
    let other_corp: i64 = db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('chat-motd-' || nextval('game.corporations_corporation_id_seq')::text, 'CM' || currval('game.corporations_corporation_id_seq')::text, $1,$1,1,1) RETURNING corporation_id", &[&foreign.id]).await.unwrap().get("corporation_id");
    let alliance: i64 = db.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('chat-motd-' || nextval('game.alliances_alliance_id_seq')::text, 'CM' || currval('game.alliances_alliance_id_seq')::text, $1,$1,$2,1) RETURNING alliance_id", &[&corp, &ceo.id]).await.unwrap().get("alliance_id");
    db.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id = ANY($2)", &[&corp, &vec![ceo.id, director.id, ordinary.id]]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&other_corp, &foreign.id]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$1 WHERE character_id=$2", &[&crate::consts::ROLE_DIRECTOR, &director.id]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&test_db).await.unwrap()); let world = World::new(test_db.clone(), sde.clone(), 1); let hub = Hub::new("chat.test", test_db.clone(), world.clone());
        let (mut client, _peer) = fixture::client(test_db.clone(), sde, world, hub).await;
        let corp_name = format!("corp_{corp}"); let alliance_name = format!("alliance_{alliance}");
        let domain = client.cfg.chat.domain.clone();
        let corp_jid = format!("{corp_name}@conference.{domain}"); let alliance_jid = format!("{alliance_name}@conference.{domain}");
        client.change_session(vec![("charid", Value::Int(ceo.id)), ("corpid", Value::Int(corp)), ("allianceid", Value::Int(alliance))]);
        assert!(set_motd(&mut client, &corp_jid, "ceo").await.is_ok()); assert!(set_motd(&mut client, &alliance_jid, "executor").await.is_ok());
        client.change_session(vec![("charid", Value::Int(director.id))]);
        assert!(set_motd(&mut client, &corp_jid, "director").await.is_ok()); assert!(set_motd(&mut client, &alliance_jid, "director executor").await.is_ok());
        client.change_session(vec![("charid", Value::Int(ordinary.id))]);
        assert!(set_motd(&mut client, &corp_jid, "ordinary").await.is_err()); assert!(set_motd(&mut client, &alliance_jid, "ordinary").await.is_err());
        client.change_session(vec![("charid", Value::Int(foreign.id)), ("corpid", Value::Int(other_corp))]);
        assert!(set_motd(&mut client, &corp_jid, "mismatch").await.is_err()); assert!(set_motd(&mut client, &alliance_jid, "nonexecutor").await.is_err());
        client.change_session(vec![("corpid", Value::Int(corp))]);
        assert!(set_motd(&mut client, &corp_jid, "forged membership").await.is_err());
        assert_eq!(dao::get_system_motd(&test_db, &corp_name).await.unwrap().as_deref(), Some("director"));
        assert_eq!(dao::get_system_motd(&test_db, &alliance_name).await.unwrap().as_deref(), Some("director executor"));
    }).await;
    db.execute("DELETE FROM game.chat_system_motds WHERE channel_name = ANY($1)", &[&vec![format!("corp_{corp}"), format!("alliance_{alliance}")]]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1, corp_roles=0 WHERE character_id = ANY($1)", &[&vec![ceo.id, director.id, ordinary.id, foreign.id]]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id = ANY($1)", &[&vec![corp, other_corp]]).await.unwrap();
    db.execute("DELETE FROM game.alliances WHERE alliance_id=$1", &[&alliance]).await.unwrap();
    fixture::cleanup(&db, &[ceo, director, ordinary, foreign]).await; result.unwrap();
}
