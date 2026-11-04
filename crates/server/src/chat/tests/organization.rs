//! Database-current organization authorization on authenticated chat sockets.
use super::{fixture, socket::{open, presence, read_until, Tasks}};
use crate::{chat::Hub, db::Db, sde::Sde, services::chat::authority, space::World};
use marshal::Value;
use std::sync::Arc;
use tokio::{io::AsyncWriteExt, net::TcpListener};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn stale_session_cannot_authorize_old_organization() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await;
    let other = fixture::create(&db).await;
    let (old_corp, old_alliance) = fixture::organization(&db, actor.id).await;
    let (new_corp, new_alliance) = fixture::organization(&db, other.id).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&test_db).await.unwrap());
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let hub = Hub::new("chat.test", test_db.clone(), world.clone());
        let (mut client, _peer) = fixture::client(test_db.clone(), sde.clone(), world.clone(), hub.clone()).await;
        client.change_session(vec![("charid", Value::Int(actor.id)), ("corpid", Value::Int(old_corp)), ("allianceid", Value::Int(old_alliance))]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut tasks = Tasks(Vec::new());
        let mut sock = open(&listener, &hub, &mut tasks, actor.id).await;
        let old_room = format!("corp_{old_corp}@conference.chat.test");
        assert!(presence(&mut sock, &old_room, actor.id, "old").await.contains("code='110'"));
        read_until(&mut sock, "</message>").await;
        test_db.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&new_corp, &actor.id]).await.unwrap();
        let stale = world.online_session(actor.id).unwrap();
        for (prefix, old, new) in [("corp", old_corp, new_corp), ("alliance", old_alliance, new_alliance)] {
            assert!(authority::admit(&test_db, &sde, &stale, &format!("{prefix}_{old}")).await.unwrap().is_none());
            assert!(authority::admit(&test_db, &sde, &stale, &format!("{prefix}_{new}")).await.unwrap().is_some());
        }
        sock.write_all(format!("<message id='old-send' to='{old_room}' type='groupchat'><body>denied</body></message>").as_bytes()).await.unwrap();
        assert!(read_until(&mut sock, "</message>").await.contains("type='error'"));
        assert!(presence(&mut sock, &old_room, actor.id, "old-rejoin").await.contains("type='error'"));
        let new_room = format!("corp_{new_corp}@conference.chat.test");
        assert!(presence(&mut sock, &new_room, actor.id, "new").await.contains("code='110'"));
    }).await;
    fixture::cleanup_organization(&db, old_corp, old_alliance).await;
    fixture::cleanup_organization(&db, new_corp, new_alliance).await;
    fixture::cleanup(&db, &[actor, other]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with initialized SDE"]
async fn organization_change_refreshes_profile_and_invites_once() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let hub = Hub::new("chat.test", db.clone(), world.clone());
    let (mut client, _peer) = fixture::client(db, sde, world, hub.clone()).await;
    client.change_session(vec![("charid", Value::Int(10)), ("corpid", Value::Int(11)), ("allianceid", Value::Int(14))]);
    hub.set_profile(10, "A & B", 1, 11, Some(14), None, 0);
    let (user_tx, mut user_rx) = tokio::sync::mpsc::unbounded_channel();
    hub.users.lock().unwrap().insert("10@chat.test".into(), ("10@chat.test/test".into(), user_tx));
    let (observer_tx, mut observer_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut rooms = hub.rooms.lock().unwrap();
    let occupants = rooms.entries.entry("help_zh@conference.chat.test".into()).or_default();
    occupants.insert("10".into(), crate::chat::Occupant { full_jid: "10@chat.test/test".into(), tx: None, affiliation: "member".into(), role: "participant".into() });
    occupants.insert("20".into(), crate::chat::Occupant { full_jid: "20@chat.test/test".into(), tx: Some(observer_tx), affiliation: "member".into(), role: "participant".into() });
    for room in ["corp_11@conference.chat.test", "alliance_14@conference.chat.test"] {
        rooms.entries.entry(room.into()).or_default().insert("10".into(), crate::chat::Occupant {
            full_jid: "10@chat.test/test".into(), tx: None, affiliation: "member".into(), role: "participant".into(),
        });
    }
    drop(rooms);
    client.change_session(vec![("corpid", Value::Int(12)), ("allianceid", Value::Int(13))]);
    assert!(!hub.rooms.lock().unwrap().entries.contains_key("corp_11@conference.chat.test"));
    assert!(!hub.rooms.lock().unwrap().entries.contains_key("alliance_14@conference.chat.test"));
    let profile = observer_rx.try_recv().unwrap();
    assert!(profile.contains("corpid='12'") && profile.contains("allianceid='13'") && profile.contains("A &amp; B"), "{profile}");
    let invites = [user_rx.try_recv().unwrap(), user_rx.try_recv().unwrap()];
    assert!(invites.iter().any(|s| s.contains("corp_12@")) && invites.iter().any(|s| s.contains("alliance_13@")));
    client.change_session(vec![("shipid", Value::Int(99))]);
    assert!(observer_rx.try_recv().is_err() && user_rx.try_recv().is_err());
    let cached = hub.profile("10");
    assert!(cached.contains("corpid='12'") && cached.contains("allianceid='13'"));
    let identity = crate::services::identity::Identity::of_client(&client);
    let old = client.world.online_session(10).unwrap();
    let mut replacement = old.clone();
    replacement.session_id += 1;
    let (replacement_tx, _replacement_rx) = tokio::sync::mpsc::unbounded_channel();
    crate::services::presence::online(&identity, &client.world, &hub, replacement_tx.clone(), None, replacement.clone());
    let mut stale = old.clone();
    stale.corporation_id = Some(11);
    stale.alliance_id = Some(14);
    crate::services::presence::online(&identity, &client.world, &hub, replacement_tx, Some(&old), stale);
    assert_eq!(client.world.online_session(10).unwrap(), replacement);
    assert_eq!(hub.profile("10"), cached);
    assert!(observer_rx.try_recv().is_err() && user_rx.try_recv().is_err());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with initialized SDE"]
async fn fresh_login_profile_reloads_persisted_organization() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await;
    let (corp, alliance) = fixture::organization(&db, actor.id).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&test_db).await.unwrap());
        let system = sde.map.systems().find(|s| s.constellation_id.is_some() && s.region_id.is_some()).unwrap();
        test_db.execute("UPDATE game.characters SET solar_system_id=$1,constellation_id=$2,region_id=$3 WHERE character_id=$4", &[&system.solar_system_id, &system.constellation_id.unwrap(), &system.region_id.unwrap(), &actor.id]).await.unwrap();
        let selected = crate::dao::characters::selection::selected(&test_db, actor.id, actor.account).await.unwrap().unwrap();
        assert_eq!((selected.corporation_id, selected.alliance_id), (corp, Some(alliance)));
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let hub = Hub::new("chat.test", test_db.clone(), world.clone());
        let (mut client, _peer) = fixture::client(test_db, sde, world.clone(), hub.clone()).await;
        client.change_session(vec![("charid", Value::Int(actor.id)), ("corpid", Value::Int(selected.corporation_id)), ("allianceid", Value::Int(selected.alliance_id.unwrap()))]);
        hub.set_profile(actor.id, &selected.name, i64::from(selected.gender_id), selected.corporation_id, selected.alliance_id, None, 0);
        let fresh = world.online_session(actor.id).unwrap();
        assert_eq!((fresh.corporation_id, fresh.alliance_id), (Some(corp), Some(alliance)));
        let profile = hub.profile(&actor.id.to_string());
        assert!(profile.contains(&format!("corpid='{corp}'")) && profile.contains(&format!("allianceid='{alliance}'")), "{profile}");
    }).await;
    fixture::cleanup_organization(&db, corp, alliance).await;
    fixture::cleanup(&db, &[actor]).await;
    result.unwrap();
}
