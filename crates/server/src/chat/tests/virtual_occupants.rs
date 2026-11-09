//! Virtual system-room occupants share the real XMPP presence stream.
use super::{fixture, socket::{open, presence, read_until, Tasks}};
use infinity_core::session::OnlineSession;
use crate::{chat::Hub, db::Db, sde::Sde, services::chat::authority, space::World};
use marshal::Value;
use std::sync::Arc;
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::mpsc};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn virtual_presence_replay_broadcast_relocation_and_isolation() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let real = fixture::create(&db).await;
    let virtual_char = fixture::create(&db).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&test_db).await.unwrap());
        let mut categories = std::collections::HashMap::new();
        let mut session = OnlineSession { character_id: virtual_char.id, user_id: None, session_id: virtual_char.id, ship_id: None, station_id: None, system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 };
        for system in sde.map.systems() {
            session.system_id = Some(system.solar_system_id);
            if let Some(channel) = authority::system_channels(&sde, &session).first() {
                if let Some((prefix, _)) = channel.split_once('_') { categories.entry(prefix.to_owned()).or_insert(system.solar_system_id); }
            }
        }
        let system = *categories.get("local").expect("SDE needs a local system");
        let other = sde.map.systems().find(|system_entry| system_entry.solar_system_id != system && system_entry.solar_system_id > 0 && {
            let mut facts = session.clone(); facts.system_id = Some(system_entry.solar_system_id); !authority::system_channels(&sde, &facts).is_empty()
        }).unwrap().solar_system_id;
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let hub = Hub::new("chat.test", test_db.clone(), world.clone());
        let (mut client, _peer) = fixture::client(test_db.clone(), sde.clone(), world.clone(), hub.clone()).await;
        client.change_session(vec![("charid", Value::Int(real.id)), ("solarsystemid2", Value::Int(system))]);
        let (notify, mut notifications) = mpsc::unbounded_channel();
        world.register_pilot(virtual_char.id, notify);
        session.system_id = Some(system);
        world.set_online_session(virtual_char.id, Some(session.clone()));
        hub.set_profile(virtual_char.id, "Virtual Pilot", 1, 1, None, None, 0);
        let channel = authority::system_channels(&sde, &session).into_iter().next().unwrap();
        let room = format!("{channel}@conference.chat.test");
        hub.join_virtual(virtual_char.id).await.unwrap();
        hub.join_virtual(virtual_char.id).await.unwrap();
        assert!(hub.rooms.lock().unwrap().entries[&room][&virtual_char.id.to_string()].tx.is_none());
        assert!(!hub.tokens.lock().unwrap().contains_key(&virtual_char.id.to_string()));
        assert!(!hub.users.lock().unwrap().contains_key(&format!("{}@chat.test", virtual_char.id)));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut tasks = Tasks(Vec::new());
        let mut socket = open(&listener, &hub, &mut tasks, real.id).await;
        socket.write_all(format!("<presence id='replay' to='{room}/{}'/>", real.id).as_bytes()).await.unwrap();
        let mut replay = String::new();
        loop {
            let stanza = read_until(&mut socket, "</presence>").await;
            replay.push_str(&stanza);
            if stanza.contains("id='replay'") { break; }
        }
        assert!(replay.contains(&format!("{room}/{}", virtual_char.id)) && replay.contains("Virtual Pilot") && replay.contains("eve_user_data"), "{replay}");
        read_until(&mut socket, "</message>").await;
        let admission = authority::admit(&test_db, &sde, &session, &channel).await.unwrap().unwrap();
        let full = format!("{}@chat.test/virtual", virtual_char.id);
        let epoch = hub.rooms.lock().unwrap().epoch;
        let mut stale = session.clone();
        stale.system_id = Some(other);
        assert!(matches!(hub.commit_admissions(&stale, epoch, &[(channel.clone(), admission.clone())], &full, None), Err(crate::chat::rooms::JoinError::Forbidden)));
        assert!(matches!(hub.commit_admissions(&session, epoch.wrapping_add(1), &[(channel.clone(), admission.clone())], &full, None), Err(crate::chat::rooms::JoinError::Forbidden)));
        assert!(hub.rooms.lock().unwrap().entries[&room][&virtual_char.id.to_string()].tx.is_none());
        let mut same = open(&listener, &hub, &mut tasks, virtual_char.id).await;
        let conflict = presence(&mut same, &room, virtual_char.id, "same-conflict").await;
        assert!(conflict.contains("type='error'") && conflict.contains("conflict"), "{conflict}");
        same.write_all(format!("<presence id='same-leave' to='{room}/{}' type='unavailable'/>", virtual_char.id).as_bytes()).await.unwrap();
        same.write_all(format!("<message id='same-message' to='{room}' type='groupchat'><body>impersonation</body></message>").as_bytes()).await.unwrap();
        let denied = read_until(&mut same, "</message>").await;
        assert!(denied.contains("type='error'") && denied.contains("forbidden"), "{denied}");
        assert!(hub.rooms.lock().unwrap().entries[&room][&virtual_char.id.to_string()].tx.is_none());
        let mut with_corp = session.clone();
        with_corp.corporation_id = Some(1);
        world.set_online_session(virtual_char.id, Some(with_corp.clone()));
        let corp = format!("corp_{}", 1);
        let corp_room = format!("{corp}@conference.chat.test");
        assert!(presence(&mut same, &corp_room, virtual_char.id, "corp-real").await.contains("code='110'"));
        read_until(&mut same, "</message>").await;
        assert!(hub.join_virtual(virtual_char.id).await.is_err());
        let corp_admission = authority::admit(&test_db, &sde, &with_corp, &corp).await.unwrap().unwrap();
        let mut other_session = with_corp.clone();
        other_session.system_id = Some(other);
        let first = authority::system_channels(&sde, &other_session).into_iter().next().unwrap();
        let first_room = format!("{first}@conference.chat.test");
        let epoch = hub.rooms.lock().unwrap().epoch;
        assert!(matches!(hub.commit_admissions(&with_corp, epoch, &[(first, admission), (corp, corp_admission)], &full, None), Err(crate::chat::rooms::JoinError::Conflict)));
        {
            let rooms = hub.rooms.lock().unwrap();
            assert!(!rooms.entries.get(&first_room).is_some_and(|occupants| occupants.contains_key(&virtual_char.id.to_string())));
            assert!(rooms.entries[&corp_room][&virtual_char.id.to_string()].tx.is_some());
            assert!(rooms.entries[&room][&virtual_char.id.to_string()].tx.is_none());
        }
        world.set_online_session(virtual_char.id, Some(session.clone()));
        let forged = presence(&mut socket, &room, virtual_char.id, "forged").await;
        assert!(forged.contains("type='error'"));
        socket.write_all(format!("<presence id='forged-leave' to='{room}/{}' type='unavailable'/>", virtual_char.id).as_bytes()).await.unwrap();
        assert!(read_until(&mut socket, "</presence>").await.contains("type='error'"));
        hub.rooms.lock().unwrap().entries.get_mut(&room).unwrap().get_mut(&virtual_char.id.to_string()).unwrap().role = "visitor".into();
        hub.refresh_permissions(&channel).await.unwrap();
        let refreshed = read_until(&mut socket, "</presence>").await;
        assert!(refreshed.contains(&format!("{room}/{}", virtual_char.id)) && refreshed.contains("role='participant'"));
        {
            let rooms = hub.rooms.lock().unwrap();
            let occupant = &rooms.entries[&room][&virtual_char.id.to_string()];
            assert!(occupant.tx.is_none());
            assert_eq!(occupant.role, "participant");
        }
        world.unregister_pilot_generation(virtual_char.id, None);
        hub.disconnect_character(virtual_char.id);
        let unavailable = read_until(&mut socket, "</presence>").await;
        assert!(unavailable.contains("type='unavailable'") && unavailable.contains(&format!("{room}/{}", virtual_char.id)));
        let (notify, receiver) = mpsc::unbounded_channel();
        world.register_pilot(virtual_char.id, notify);
        let _receiver = receiver;
        world.set_online_session(virtual_char.id, Some(session.clone()));
        hub.join_virtual(virtual_char.id).await.unwrap();
        let joined = read_until(&mut socket, "</presence>").await;
        assert!(joined.contains(&format!("{room}/{}", virtual_char.id)) && joined.contains("Virtual Pilot"));
        session.system_id = Some(other);
        world.set_online_session(virtual_char.id, Some(session.clone()));
        hub.session_changed(virtual_char.id);
        assert!(read_until(&mut socket, "</presence>").await.contains("type='unavailable'"));
        hub.join_virtual(virtual_char.id).await.unwrap();
        let new_channel = authority::system_channels(&sde, &session).into_iter().next().unwrap();
        let new_room = format!("{new_channel}@conference.chat.test");
        client.change_session(vec![("solarsystemid2", Value::Int(other))]);
        socket.write_all(format!("<presence id='relocated' to='{new_room}/{}'/>", real.id).as_bytes()).await.unwrap();
        let mut moved = String::new();
        loop { let stanza = read_until(&mut socket, "</presence>").await; moved.push_str(&stanza); if stanza.contains("id='relocated'") { break; } }
        assert!(moved.contains(&format!("{new_room}/{}", virtual_char.id)) && moved.contains("Virtual Pilot"));
        assert!(notifications.try_recv().is_err());
        for prefix in ["local", "nolocal", "wormhole", "triglavian"] {
            if let Some(&id) = categories.get(prefix) {
                session.system_id = Some(id);
                world.set_online_session(virtual_char.id, Some(session.clone()));
                hub.session_changed(virtual_char.id);
                hub.join_virtual(virtual_char.id).await.unwrap();
                let channel = authority::system_channels(&sde, &session).into_iter().next().unwrap();
                assert!(channel.starts_with(&format!("{prefix}_")));
                assert!(hub.rooms.lock().unwrap().entries[&format!("{channel}@conference.chat.test")].contains_key(&virtual_char.id.to_string()));
            }
        }
        world.unregister_pilot_generation(virtual_char.id, None);
        hub.disconnect_character(virtual_char.id);
    }).await;
    fixture::cleanup(&db, &[real, virtual_char]).await;
    result.unwrap();
}
