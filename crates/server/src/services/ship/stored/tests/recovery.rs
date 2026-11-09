//! Bound stored-ship recovery and admission against the real world and database.
//! The ship client uses LaunchFromContainer, StoreVessel and BoardStoredShip;
//! restored free hulls come from game items and SDE dogma. This exercises reload
//! after publication, not the separate pre-publication reconciliation path.

use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn launched_hull_survives_actor_replacement_without_duplicate_or_selection_change() {
    let mut b = BayFixture::new().await;
    let (old, host, owner, system) = (b.f.ship, b.host, b.f.owner, b.f.system);
    let target = b.stored(b.host_owner).await;
    assert!(crate::dao::items::dogma::set_override(&b.f.db, target, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, 1.0).await.unwrap());
    let actor_condition = active_armor_hull(&b, old).await.expect("live actor condition");
    let session: Vec<_> = b.f.client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
    assert_eq!(b.call("LaunchFromContainer", &[Value::Int(host), Value::Int(target)]).await.unwrap(), Value::None);
    assert_eq!(active_armor_hull(&b, old).await, Some(actor_condition));
    assert_eq!(b.f.world.with(system, move |s| s.combat_phase(old)).await.flatten(), Some(crate::space::combat::CombatPhase::Live));
    assert_eq!(session, b.f.client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>());
    selected(&b, old).await;
    let target_condition = active_armor_hull(&b, target).await.expect("launched condition");
    let item = row(&b, target).await;
    assert_eq!((item.owner_id, item.location_id, item.flag_id), (owner, system, 0));

    // The mailbox closure stops the actor only after the bound call has returned.
    assert!(b.f.world.with(system, |_| -> () { panic!("stop actor after launch publication") }).await.is_none());
    let market = crate::services::market::lock().await;
    b.f.world.system_locked(system, &market).await.unwrap();
    drop(market);
    b.f.world.with(system, move |s| {
        let ball = s.ball(target).expect("restored launched hull");
        assert_eq!((ball.owner_id, ball.char_id, ball.interactive), (owner, None, false));
        assert_eq!(s.balls().filter(|ball| ball.id == target).count(), 1);
    }).await.unwrap();
    assert_eq!(active_armor_hull(&b, target).await, Some(target_condition));
    assert_eq!(crate::dao::characters::selection::ship_id(&b.f.db, owner).await.unwrap(), Some(old));
    assert_eq!(session, b.f.client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>());

    let generation = b.f.world.with(system, move |s| {
        assert!(s.set_condition_fraction_mask(target, 0b110, 0.5));
        s.persistent_generation(target).unwrap()
    }).await.unwrap();
    let changed = active_armor_hull(&b, target).await.expect("restored condition");
    let market = crate::services::market::lock().await;
    b.f.world.system_locked(system, &market).await.unwrap();
    drop(market);
    assert_ne!(changed, target_condition, "replacement must carry a distinct live condition");
    assert_eq!(active_armor_hull(&b, target).await, Some(changed));
    assert_eq!(b.f.world.with(system, move |s| (s.persistent_generation(target), s.balls().filter(|ball| ball.id == target).count())).await.unwrap(), (Some(generation), 1));

    let kwargs = Value::None;
    let args = [Value::Int(owner)];
    let call = Call { service: "charUnboundMgr", method: "SelectCharacterID", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::dispatch(&mut b.f.client, &call).await.unwrap();
    b.f.world.observe(system, b.f.client.login.client_id, old, b.f.client.destiny_tx.clone()).await.unwrap();
    selected(&b, old).await;
    assert_eq!(active_armor_hull(&b, target).await, Some(changed));
    assert_eq!(b.f.world.with(system, move |s| (s.persistent_generation(target), s.balls().filter(|ball| ball.id == target).count())).await.unwrap(), (Some(generation), 1));
    assert_eq!(active_armor_hull(&b, old).await, Some(actor_condition));
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn replaced_request_session_refuses_store_and_board_before_mutation() {
    let mut b = BayFixture::new().await;
    b.capture_refusals().await;
    let (owner, old, host) = (b.f.owner, b.f.ship, b.host);
    let target = b.stored(b.host_owner).await;
    let mut replacement = infinity_core::session::OnlineSession::from_session(&b.f.client.session).unwrap();
    replacement.session_id = b.f.client.session.sid.checked_add(1).unwrap();
    let previous = b.f.world.online_session(owner);
    assert_eq!(previous, infinity_core::session::OnlineSession::from_session(&b.f.client.session));
    assert!(previous.is_some(), "fixture must register the request session");
    let (replacement_tx, mut replacement_rx) = tokio::sync::mpsc::unbounded_channel();
    b.f.world.register_pilot(owner, replacement_tx);
    b.f.world.set_online_session(owner, Some(replacement.clone()));
    assert_ne!(replacement.session_id, b.f.client.session.sid);
    assert_eq!(b.f.world.online_session(owner), Some(replacement.clone()));
    for (method, args) in [
        ("StoreVessel", vec![Value::Int(host)]),
        ("BoardStoredShip", vec![Value::Int(host), Value::Int(target)]),
    ] {
        b.refuse(method, &args, None).await;
        assert!(matches!(replacement_rx.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
        assert_eq!(b.f.world.online_session(owner), Some(replacement.clone()));
        assert!(b.f.world.ball(b.f.system, target).await.is_none());
        selected(&b, old).await;
    }
    assert!(b.f.world.replace_online_session(owner, Some(&replacement), None));
    assert_eq!(b.f.world.online_session(owner), None);
    for (method, args) in [
        ("StoreVessel", vec![Value::Int(host)]),
        ("BoardStoredShip", vec![Value::Int(host), Value::Int(target)]),
    ] {
        b.refuse(method, &args, None).await;
        assert!(matches!(replacement_rx.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
        assert_eq!(b.f.world.online_session(owner), None);
        assert!(b.f.world.ball(b.f.system, target).await.is_none());
        selected(&b, old).await;
    }
    b.cleanup().await;
}
