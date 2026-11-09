use super::*;
use super::super::broadcasts::allowed;
use crate::space;

#[test]
fn hierarchy_matches_client_superior_and_subordinate_rules() {
    let leader = Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_NONE, 0);
    let wing = Member::new(10, NONE_ID, ROLE_WING_COMMANDER, JOB_NONE, 0);
    let squad = Member::new(10, 11, ROLE_SQUAD_COMMANDER, JOB_NONE, 0);
    let peer = Member::new(10, 11, ROLE_MEMBER, JOB_NONE, 0);
    let other_squad = Member::new(10, 12, ROLE_MEMBER, JOB_NONE, 0);
    let other_wing = Member::new(20, 21, ROLE_SQUAD_COMMANDER, JOB_NONE, 0);
    assert!(allowed(2, 4, &peer, 1, &leader));
    assert!(allowed(2, 4, &peer, 2, &wing));
    assert!(allowed(2, 4, &peer, 3, &squad));
    assert!(!allowed(2, 4, &peer, 6, &other_wing));
    assert!(!allowed(2, 2, &wing, 3, &squad));
    assert!(allowed(1, 2, &wing, 5, &other_squad));
    assert!(!allowed(1, 3, &squad, 5, &other_squad));
    assert!(allowed(1, 3, &squad, 4, &peer));
    assert!(allowed(1, 1, &leader, 6, &other_wing));
    assert!(!allowed(2, 1, &leader, 2, &wing));
    assert!(allowed(3, 4, &peer, 6, &other_wing));
    assert!(allowed(2, 4, &peer, 4, &peer));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn bubble_broadcast_delivers_only_to_live_visible_observers() {
    use std::sync::Arc;
    use super::super::broadcasts::{send, BroadcastDestination};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = space::World::new(db.clone(), sde.clone(), 1);
    let system = -1;
    let sender = 90_000_001;
    let recipient = 90_000_002;
    let sender_ship = 101;
    let recipient_ship = 102;
    let target = 103;
    let sender_connection = 1_000_000_000_001;
    let recipient_connection = 1_000_000_000_002;
    let make_ball = |id, pilot| space::Ball {
        id, type_id: 1, owner_id: pilot, corp_id: 1, alliance_id: -1, char_id: Some(pilot),
        mode: space::MODE_STOP, radius: 1.0, signature_radius: None, mass: 1e6, max_vel: 100.0,
        agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false,
        massive: false, interactive: true, warp: space::WarpState::default(), 
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: vec![],
    };
    world.system(system).await.unwrap();
    world.with(system, move |s| {
        s.insert(make_ball(sender_ship, sender));
        s.insert(make_ball(recipient_ship, recipient));
        let mut ball = make_ball(target, 0);
        ball.char_id = None;
        s.insert(ball);
    }).await.unwrap();
    let (notify, mut notifications) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(recipient, notify);
    let (sender_tx, _sender_rx) = tokio::sync::mpsc::unbounded_channel();
    let (recipient_tx, recipient_rx) = tokio::sync::mpsc::unbounded_channel();
    world.observe(system, sender_connection, sender_ship, sender_tx).await.unwrap();
    world.observe(system, recipient_connection, recipient_ship, recipient_tx).await.unwrap();
    let fleet_id = next_id();
    let mut fleet = Fleet::new(sender, vec![]);
    fleet.members.insert(sender, Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_NONE, 0));
    fleet.members.insert(recipient, Member::new(NONE_ID, NONE_ID, ROLE_MEMBER, JOB_NONE, 0));
    { let mut st = state(); st.fleets.insert(fleet_id, fleet); st.of.insert(sender, fleet_id); st.of.insert(recipient, fleet_id); }
    let cfg = Arc::new(crate::config::Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: sender_connection, session_id: 1, language_id: "EN".into() };
    let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, db, sde, chat, world.clone(), login);
    client.session.apply(vec![("charid", Value::Int(sender)), ("shipid", Value::Int(sender_ship)), ("solarsystemid", Value::Int(system))]);
    let args = [Value::str("Target"), Value::Int(3), Value::Int(target)];
    let call = Call { service: "fleet", method: "SendBroadcast", args: &args, kwargs: &Value::None, bound: false, bind_params: None };
    assert!(send(&mut client, Some(fleet_id), &call, BroadcastDestination::Bubble).await.is_ok());
    assert_eq!(notifications.try_recv().unwrap(), ("OnFleetBroadcast".to_string(),
        vec![Value::str("Target"), Value::Int(3), Value::Int(sender), Value::Int(system), Value::Int(target), Value::None]));
    world.with(system, move |s| s.unobserve(recipient_connection)).await.unwrap();
    let (stale_tx, _stale_rx) = tokio::sync::mpsc::unbounded_channel();
    world.observe(system, recipient_connection, 999, stale_tx).await.unwrap();
    assert!(send(&mut client, Some(fleet_id), &call, BroadcastDestination::Bubble).await.is_ok());
    assert!(notifications.try_recv().is_err());
    let (closed_tx, closed_rx) = tokio::sync::mpsc::unbounded_channel();
    world.observe(system, recipient_connection, recipient_ship, closed_tx).await.unwrap();
    drop(closed_rx);
    assert!(send(&mut client, Some(fleet_id), &call, BroadcastDestination::Bubble).await.is_ok());
    assert!(notifications.try_recv().is_err());
    let (live_tx, _live_rx) = tokio::sync::mpsc::unbounded_channel();
    world.observe(system, recipient_connection, recipient_ship, live_tx).await.unwrap();
    world.with(system, move |s| s.unobserve(sender_connection)).await.unwrap();
    let (stale_sender, _stale_sender_rx) = tokio::sync::mpsc::unbounded_channel();
    world.observe(system, sender_connection, 999, stale_sender).await.unwrap();
    assert!(send(&mut client, Some(fleet_id), &call, BroadcastDestination::Bubble).await.is_err());
    assert!(notifications.try_recv().is_err());
    let (sender_tx, _sender_rx) = tokio::sync::mpsc::unbounded_channel();
    world.observe(system, sender_connection, sender_ship, sender_tx).await.unwrap();
    world.with(system, move |s| {
        let mut ball = make_ball(target, 0);
        ball.char_id = None;
        ball.cloak = 1;
        s.insert(ball);
    }).await.unwrap();
    assert!(send(&mut client, Some(fleet_id), &call, BroadcastDestination::Bubble).await.is_err());
    assert!(notifications.try_recv().is_err());
    drop(recipient_rx);
    { let mut st = state(); st.fleets.remove(&fleet_id); st.of.remove(&sender); st.of.remove(&recipient); }
}
