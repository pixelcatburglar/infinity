//! Real actor and transaction coverage for space repair.
use super::*;
use crate::services::{combat::groups_health, effects::{heat, tests::fixture as actor_fixture}, slash::tests::support::{register_online, rpc, wire}};
use infinity_core::session::OnlineSession;
use crate::space::combat::tests::{cached_fitting, cached_module_attribute, cached_overloaded};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn space_heal_commits_before_guarded_publication_and_retries() {
    init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = fixture::create(&db).await.unwrap();
    let (station, ship, modules, cargo, hp, _) = docked_ship(&db, sde.clone(), fixture.character).await;
    let owner = fixture.character;
    let system = sde.map.stations().find(|s| s.station_id == station).unwrap().solar_system_id.unwrap();
    let ship_type: i32 = db.query_one("SELECT type_id FROM game.items WHERE item_id = $1", &[&ship]).await.unwrap().get("type_id");
    db.execute("UPDATE game.items SET online = true WHERE item_id = $1", &[&modules[0]]).await.unwrap();
    assert!(dogma::set_override(&db, modules[0], ATTR_HULL_DAMAGE, hp[0] * 0.5).await.unwrap());
    crate::dao::characters::ship::undock(&db, owner, system).await.unwrap();
    crate::dao::items::ship::undock(&db, ship, system, [0.0; 3]).await.unwrap();
    let balance = wallets::balance(&db, owner).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let (mut client, _peer) = crate::services::tests::client(db.clone(), 0).await;
    client.world = world.clone(); client.sde = sde.clone(); client.login.role = GM_ROLES;
    client.session.apply(vec![("charid", Value::Int(owner)), ("shipid", Value::Int(ship)), ("solarsystemid2", Value::Int(system))]);
    let expected = OnlineSession::from_session(&client.session).unwrap();
    let mut initial_rx = register_online(&world, expected.clone());
    actor_fixture::admit(&db, &world, system, actor_fixture::ball(ship, ship_type, owner, Some(owner), 0.0)).await;
    let (destiny, mut updates) = tokio::sync::mpsc::unbounded_channel();
    world.observe(system, client.login.client_id, ship, destiny).await.unwrap();
    let capture = expected.clone();
    let (generation, gate) = world.with(system, move |s| s.capture_space_heal(&capture)).await.flatten().unwrap();
    let fit = Fit::load(&db, sde.clone(), owner, ship).await.unwrap();
    // A second actor may assign the same numeric generation to this ship.
    let other = World::new(db.clone(), sde.clone(), 1);
    let _other_rx = register_online(&other, expected.clone());
    actor_fixture::admit(&db, &other, system, actor_fixture::ball(ship, ship_type, owner, Some(owner), 0.0)).await;
    let other_expected = expected.clone();
    let (other_generation, other_gate) = other.with(system, move |s| s.capture_space_heal(&other_expected)).await.flatten().unwrap();
    assert_eq!(other_generation, generation);
    let guard = market::lock().await;
    let rejected = [(expected.clone(), other_gate), (OnlineSession { session_id: expected.session_id + 1, ..expected.clone() }, gate.clone())]
        .map(|(session, actor_gate)| queue_space_heal(&world, session, generation, actor_gate, &modules));
    drop(guard);
    for ack in rejected { assert!(!ack.await.unwrap()); }
    other.remove_ball(system, ship).await;
    let capacity = fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
    let tau = fit.value(Key::Ship, ATTR_RECHARGE_RATE);
    let heat_values: [f64; 3] = heat::empty_ship_state(&fit, crate::client::filetime_now()).iter().map(|rack| rack.capacity * 0.5).collect::<Vec<_>>().try_into().unwrap();
    assert!(heat_values.iter().all(|value| *value > 0.0));
    let seed = move |s: &mut crate::space::System, fit: Fit| {
        let stats = groups_health::ShipStats::from_fit(&fit, Key::Ship);
        let at = s.now_ms();
        heat::tests::seed_ship_heat(s, ship, owner, &fit, heat_values, at);
        s.install_condition(ship, groups_health::Condition { now: stats.max.map(|value| value * 0.5), stats, shield_at: crate::client::filetime_now() }, fit);
        s.fitted_module_active(ship, modules[0], true);
        s.fitted_module_overloaded(ship, modules[0], true);
        assert_eq!(cached_fitting(s, ship, modules[0], ATTR_MASS).unwrap().1, Some((true, true)));
        assert_eq!(cached_overloaded(s, ship, modules[0]), Some(true));
        s.refresh_projection_target(ship);
        modules.map(|module| cached_module_attribute(s, ship, module, ATTR_HULL_DAMAGE).unwrap())
    };
    let before = world.with(system, move |s| seed(s, fit)).await.unwrap();
    assert!(before.iter().all(|(damage, revision)| *damage > 0.0 && revision.is_some()));
    groups_health::drain_warp_capacitor(ship, capacity, tau, capacity * 0.5, crate::client::filetime_now());
    while let Ok(delivery) = updates.try_recv() {
        match delivery {
            crate::space::SceneDelivery::Destiny(_) => {}
            crate::space::SceneDelivery::Notify { event, .. } => assert_eq!(event, "OnFleetStateChange"),
        }
    }
    assert_eq!(wire(rpc(&mut client, "/heal").await), wire(Ok(Value::unicode(messages::gm::GM_HEALED_SPACE))));
    let after = world.with(system, move |s| modules.map(|module| cached_module_attribute(s, ship, module, ATTR_HULL_DAMAGE).unwrap())).await.unwrap();
    assert_eq!(after, before.map(|(_, revision)| (0.0, revision)));
    let mut health_notice = false;
    while let Ok(delivery) = updates.try_recv() {
        let events = match delivery {
            crate::space::SceneDelivery::Destiny(events) => events,
            crate::space::SceneDelivery::Notify { event, .. } => { assert_eq!(event, "OnFleetStateChange"); continue; }
        };
        health_notice |= events.iter().any(|event| matches!(event, Value::Tuple(outer)
            if matches!(&outer[1], Value::Tuple(inner) if inner[0].as_text().as_deref() == Some("OnDamageStateChange"))));
    }
    assert!(health_notice);
    let cap = groups_health::capacitor_snapshot(&db, &world, system, ship).await.unwrap().unwrap();
    assert_eq!(cap.level, cap.capacity);
    assert!(heat::ship_state(&world, system, ship).await.unwrap().iter().all(|rack| rack.heat == 0.0));
    let mut changes = Vec::new();
    while let Ok((event, args)) = initial_rx.try_recv() {
        if event == "OnModuleAttributeChanges" { changes.extend(args); }
    }
    let rows = changes.iter().filter_map(|entry| match entry { Value::List(rows) => Some(rows), _ => None })
        .flatten().filter_map(|row| match row { Value::Tuple(fields) => Some(fields), _ => None });
    for (item, attr) in modules.into_iter().map(|id| (id, ATTR_HULL_DAMAGE)).chain(
        [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE, ATTR_CHARGE, ATTR_HEAT_LOW, ATTR_HEAT_MED, ATTR_HEAT_HI].into_iter().map(|attr| (ship, attr)),
    ) {
        assert!(rows.clone().any(|fields| fields[2].as_int() == Some(item) && fields[3].as_int() == Some(i64::from(attr))));
    }
    for (id, online) in modules.into_iter().zip([true, false, false]) {
        assert_eq!(db.query_one("SELECT online FROM game.items WHERE item_id = $1", &[&id]).await.unwrap().get::<_, bool>("online"), online);
    }
    world.flush_positions().await.unwrap();
    let reload = Fit::load_in_space(&db, &world, system, owner, ship).await.unwrap();
    assert_eq!(crate::services::combat::load_condition_with_fit(&db, ship, &reload).await.unwrap(), [1.0; 3]);
    for id in modules { assert!(dogma::set_override(&db, id, ATTR_HULL_DAMAGE, 2.0).await.unwrap()); }
    let replacement = Fit::load(&db, sde.clone(), owner, ship).await.unwrap();
    groups_health::drain_warp_capacitor(ship, capacity, tau, capacity, crate::client::filetime_now());

    // The test barrier is after the production transaction body and before COMMIT.
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let mut newer = expected.clone(); newer.session_id += 1;
    let transaction = async {
        let _market = market::lock().await;
        db.transaction(|tx, outbox| {
            let (sde, expected, entered, resume) = (sde.clone(), expected.clone(), entered.clone(), resume.clone());
            Box::pin(async move {
                heal_space_in(tx, outbox, &expected, &sde).await?;
                entered.notify_one(); resume.notified().await;
                Ok::<(), CallError>(())
            })
        }, |(), outbox| std::future::ready(Ok(dispatch_space_heal(&world, expected.clone(), generation, gate, outbox)))).await.unwrap()
    };
    let replace = async {
        entered.notified().await;
        let mut new_rx = register_online(&world, newer.clone());
        let baseline = world.with(system, move |s| {
            s.remove_ball(ship);
            s.add_ship(actor_fixture::ball(ship, ship_type, owner, Some(owner), 0.0));
            seed(s, replacement)
        }).await.unwrap();
        while new_rx.try_recv().is_ok() {}
        resume.notify_one();
        (baseline, new_rx)
    };
    let (ack, (baseline, mut new_rx)) = tokio::join!(transaction, replace);
    assert!(!ack.await.unwrap_or(false));
    let after_partial = world.with(system, move |s| modules.map(|module| cached_module_attribute(s, ship, module, ATTR_HULL_DAMAGE).unwrap())).await.unwrap();
    assert!(baseline.iter().all(|(damage, revision)| *damage > 0.0 && revision.is_some()));
    assert_eq!(after_partial, baseline);
    let racks = heat::ship_state(&world, system, ship).await.unwrap();
    assert!(racks.iter().any(|rack| rack.heat > 0.0));
    let state = world.with(system, move |s| cached_fitting(s, ship, modules[0], ATTR_MASS).unwrap().1).await.unwrap();
    assert_eq!(state, Some((true, true)));
    assert!(new_rx.try_recv().is_err());
    for id in modules {
        assert_eq!(dogma::overrides(&db, id).await.unwrap().iter().find(|row| row.attribute_id == ATTR_HULL_DAMAGE).unwrap().value, 0.0);
    }
    let health = world.with(system, move |s| s.combat_health_scalars(ship).unwrap()).await.unwrap();
    assert!(health.1[1] > 0.0 && health.1[2] > 0.0);
    assert!(groups_health::capacitor_snapshot(&db, &world, system, ship).await.unwrap().unwrap().level < capacity);
    client.session.sid = newer.session_id;
    assert_eq!(wire(rpc(&mut client, "/heal").await), wire(Ok(Value::unicode(messages::gm::GM_HEALED_SPACE))));
    let condition = world.with(system, move |s| (s.ship_condition(ship).unwrap(), s.ball(ship).unwrap().health)).await.unwrap();
    assert_eq!((condition.0.1, condition.0.2), (0.0, 0.0));
    assert_eq!(condition.1, [1.0; 3]);
    let cap = groups_health::capacitor_snapshot(&db, &world, system, ship).await.unwrap().unwrap();
    assert_eq!(cap.level, cap.capacity);
    assert!(heat::ship_state(&world, system, ship).await.unwrap().iter().all(|rack| rack.heat == 0.0));
    let after = world.with(system, move |s| (modules.map(|module| cached_module_attribute(s, ship, module, ATTR_HULL_DAMAGE).unwrap()), cached_fitting(s, ship, modules[0], ATTR_MASS).unwrap().1)).await.unwrap();
    assert_eq!(after.0, baseline.map(|(_, revision)| (0.0, revision)));
    assert_eq!(after.1, state);
    let mut notified = false;
    while let Ok((event, _)) = new_rx.try_recv() { notified |= event == "OnModuleAttributeChanges"; }
    assert!(notified);
    world.flush_positions().await.unwrap();
    for attribute in [ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE] {
        assert_eq!(dogma::overrides(&db, ship).await.unwrap().iter().find(|row| row.attribute_id == attribute).unwrap().value, 0.0);
    }
    assert_eq!(dogma::overrides(&db, cargo).await.unwrap().iter().find(|row| row.attribute_id == ATTR_HULL_DAMAGE).unwrap().value, 6.0);
    assert_eq!(dogma::overrides(&db, ship).await.unwrap().iter().find(|row| row.attribute_id == ATTR_MASS).unwrap().value, 99.0);
    assert_eq!(wallets::balance(&db, owner).await.unwrap(), balance);
    assert!(matches!(heal_space(&newer, &db, &world, &sde).await.unwrap(), SpaceHealOutcome::Complete));
    world.remove_ball(system, ship).await;
    fixture::cleanup(&db, fixture).await.unwrap();
}
