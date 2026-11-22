use super::*;
use infinity_core::{consts::{dogma::ATTR_FRIGATE_ESCAPE_BAY_CAPACITY, CATEGORY_IMPLANT, CATEGORY_SHIP, FLAG_HANGAR, FLAG_IMPLANT, GROUP_BOOSTER}, session::OnlineSession};
use crate::{dao::{characters::selection, items::{brain, creation, space_ships::FreeShip, transfers}}, services::combat::damage::{apply_damage_guarded, DamageSource}};
use crate::tests::catch_scenario;
use crate::dao::items::parent_locks::tests::{character_before_items, wait_blocked};

mod body;
mod nested_contents;
mod ship_stance;

/// Open a raw connection to the dedicated test database for cross-connection
/// lock barriers; each test spawns the connection driver itself.
async fn connection() -> tokio_postgres::Client {
    let cfg = crate::config::tests::test_database();
    let (client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    client
}
async fn cleanup(db: &Db, fixture: crate::services::tests::fixture::Fixture, victims: &[i64]) {
    let kills: Vec<i64> = db.query("SELECT kill_id FROM game.killmails WHERE victim_id=$1 AND victim_item_id=ANY($2)", &[&fixture.character, &victims]).await.unwrap().iter().map(|row| row.get("kill_id")).collect();
    db.execute("DELETE FROM game.map_events WHERE source=$1 AND source_id=ANY($2)", &[&(crate::dao::map_telemetry::EventSource::Killmail as i16), &kills]).await.unwrap();
    for kill in &kills {
        let job = format!("{}:{kill}", crate::services::bounty::payout::KIND);
        db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap();
    }
    db.execute("DELETE FROM game.bounty_payouts WHERE killmail_id=ANY($1)", &[&kills]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=$1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.killmail_wars WHERE kill_id=ANY($1)", &[&kills]).await.unwrap();
    db.execute("DELETE FROM game.killmails WHERE kill_id=ANY($1)", &[&kills]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(db, fixture).await.unwrap();
}

fn resume_scenario(result: std::thread::Result<()>) {
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

/// Attach one live informed binding on observer client 1 to `ship` and drain
/// its initial delivery; `observer_ego(1)` then reports its desired ego.
async fn watch_loss(world: &World, system: i64, ship: i64) -> tokio::sync::mpsc::UnboundedReceiver<crate::space::SceneDelivery> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    world.observe(system, 1, ship, tx).await.unwrap();
    while rx.try_recv().is_ok() {}
    rx
}

/// Flatten pending scene deliveries into their Destiny events.
fn drain_destiny(rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::space::SceneDelivery>) -> Vec<Value> {
    std::iter::from_fn(|| rx.try_recv().ok()).flat_map(|delivery| match delivery {
        crate::space::SceneDelivery::Destiny(batch) => batch,
        crate::space::SceneDelivery::Notify { event, .. } => { assert_eq!(event, "OnFleetStateChange"); Vec::new() }
    }).collect()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn headless_loss_publishes_capsule_and_committed_session() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let mut victims = Vec::new();
    let result = catch_scenario(async {
    let system = sde.map.systems().next().unwrap().solar_system_id;
    // An escape-capable host with an empty bay still takes the capsule path.
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.type_id != TYPE_CAPSULE
        && t.attribute(ATTR_FRIGATE_ESCAPE_BAY_CAPACITY).is_some_and(|v| v > 0.0)).unwrap().type_id;
    let ship = creation::positioned(&db, hull, character, system, [1.0, 2.0, 3.0]).await.unwrap();
    victims.push(ship);
    let home = sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).unwrap().station_id;
    db.execute("UPDATE game.characters SET station_id=NULL, home_station_id=$4, solar_system_id=$2, ship_id=$3 WHERE character_id=$1", &[&character, &system, &ship, &home]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 0);
    world.system(system).await.unwrap();
    let row = FreeShip { item_id: ship, type_id: hull, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [1.0, 2.0, 3.0] };
    assert_eq!(crate::dao::characters::npc_control::set(&db, character, fixture.account, Some(true)).await.unwrap(), Some(true));
    let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
    world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    assert!(world.with(system, move |s| s.npc_target_ignored(ship)).await.unwrap());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(character, tx);
    world.set_online_session(character, Some(OnlineSession { character_id: character, user_id: None, session_id: 1, ship_id: Some(ship), station_id: None, system_id: Some(system), corporation_id: Some(1), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
    for invalid in [f64::NAN, f64::INFINITY] {
        let row = FreeShip { item_id: ship, type_id: hull, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [1.0, 2.0, 3.0] };
        let mut prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
        prepared.ball.pos = [invalid, 0.0, 0.0];
        prepared.ball.old_pos = prepared.ball.pos;
        world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        assert!(apply_damage_guarded(&db, &world, system, ship, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true, |_| true).await.is_err());
        assert_eq!(selection::ship_id(&db, character).await.unwrap(), Some(ship));
    }
    let row = FreeShip { item_id: ship, type_id: hull, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [1.0, 2.0, 3.0] };
    let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
    world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    let mut scene = watch_loss(&world, system, ship).await;
    let result = character_before_items(&db, character, &[ship], async { apply_damage_guarded(&db, &world, system, ship, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true, |_| true).await.ok().unwrap().unwrap() }).await;
    assert!(result.lethal);
    let capsule = selection::ship_id(&db, character).await.unwrap().unwrap();
    assert_ne!(capsule, ship);
    assert_eq!(world.with(system, move |s| s.pilot_ball(character).map(|b| b.id)).await.flatten(), Some(capsule));
    assert!(world.with(system, move |s| s.npc_target_ignored(capsule)).await.unwrap());
    let session = world.online_session(character).unwrap();
    assert_eq!((session.ship_id, session.station_id, session.system_id), (Some(capsule), None, Some(system)));
    assert_eq!(db.query("SELECT item_id FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&character, &TYPE_CAPSULE]).await.unwrap().len(), 1);
    world.with(system, move |s| crate::space::system::tests::events::publish_scene_prefix(s, crate::space::collision::enabled())).await.unwrap();
    assert_eq!(world.with(system, move |s| s.observer_ego(1)).await.flatten(), Some(capsule));
    let events = drain_destiny(&mut scene);
    let names: Vec<String> = events.iter().map(|v| v.items().unwrap()[1].items().unwrap()[0].as_text().unwrap().into_owned()).collect();
    let pos = |n: &str| names.iter().position(|v| v.as_str() == n).unwrap_or_else(|| panic!("missing {n} event"));
    assert!(pos("TerminalPlayDestructionEffect") < pos("RemoveBalls") && pos("RemoveBalls") < pos("AddBalls"),
        "terminal, removal and successor Add in order: {names:?}");
    let slim = &events[pos("AddBalls")].items().unwrap()[1].items().unwrap()[1].items().unwrap()[0].items().unwrap()[1].items().unwrap()[0];
    assert_eq!(crate::services::value_field(slim, "itemID").unwrap().as_int(), Some(capsule));
    world.remove_ball(system, capsule).await;
    world.unregister_pilot_generation(character, None);
    let headless_ship = creation::positioned(&db, hull, character, system, [1.0, 2.0, 3.0]).await.unwrap();
    victims.push(headless_ship);
    selection::set_ship(&db, character, headless_ship).await.unwrap();
    let row = FreeShip { item_id: headless_ship, type_id: hull, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [1.0, 2.0, 3.0] };
    let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
    world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    assert!(apply_damage_guarded(&db, &world, system, headless_ship, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true, |_| true).await.ok().unwrap().unwrap().lethal);
    let headless_capsule = selection::ship_id(&db, character).await.unwrap().unwrap();
    assert_ne!(headless_capsule, headless_ship);
    assert_eq!(world.with(system, move |s| s.pilot_ball(character).map(|b| b.id)).await.flatten(), Some(headless_capsule));
    world.remove_ball(system, headless_capsule).await;
    }).await;
    cleanup(&db, fixture, &victims).await;
    resume_scenario(result);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn stale_reservation_refreshes_session_without_replacing_actor() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let mut victims = Vec::new();
    let result = catch_scenario(async {
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let ship = creation::positioned(&db, TYPE_CAPSULE, character, system, [0.0; 3]).await.unwrap();
    victims.push(ship);
    let world = World::new(db.clone(), sde.clone(), 0);
    world.system(system).await.unwrap();
    let capsule = creation::positioned(&db, TYPE_CAPSULE, character, system, [1.0; 3]).await.unwrap();
    victims.push(capsule);
    let row = FreeShip { item_id: ship, type_id: TYPE_CAPSULE, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
    let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
    let (token, generation) = world.with(system, move |s| { prepared.install(s, |s, ball| s.add_ship(ball)); let snapshot = s.apply_damage(ship, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true).unwrap().unwrap().first_lethal.unwrap(); (snapshot.token, snapshot.generation) }).await.unwrap();
    let replacement_ball = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
    world.with(system, move |s| replacement_ball.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(character, tx);
    world.set_online_session(character, Some(OnlineSession { character_id: character, user_id: None, session_id: 1, ship_id: Some(ship), station_id: None, system_id: Some(system), corporation_id: Some(1), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
    let expected = world.online_session(character);
    let mut scene = watch_loss(&world, system, ship).await;
    selection::set_ship(&db, character, capsule).await.unwrap();
    let mut replacement = expected.clone().unwrap();
    replacement.session_id += 1;
    let (replacement_tx, _replacement_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(character, replacement_tx);
    world.set_online_session(character, Some(replacement.clone()));
    let identity = Identity { char_id: character, corp_id: 1, ship_id: Some(ship), station_id: None, system_id: Some(system) };
    assert!(!publish_loss(&identity, &db, &world, &sde, LossOutcome { replacement_ship: capsule, home: None, position: [1.0; 3] }, token, generation, None).await.ok().unwrap());
    assert_eq!(world.online_session(character), Some(replacement.clone()));
    assert!(!publish_loss(&identity, &db, &world, &sde, LossOutcome { replacement_ship: capsule, home: None, position: [1.0; 3] }, token, generation, expected.clone()).await.ok().unwrap());
    assert_eq!(world.online_session(character), Some(replacement));
    assert_eq!(selection::ship_id(&db, character).await.unwrap(), Some(capsule));
    assert_eq!(world.with(system, move |s| s.pilot_ball(character).map(|b| b.id)).await.flatten(), Some(ship));
    assert!(world.ball(system, capsule).await.is_none());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(character, tx);
    world.set_online_session(character, expected.clone());
    assert!(!publish_loss(&identity, &db, &world, &sde, LossOutcome { replacement_ship: capsule, home: None, position: [1.0; 3] }, token, generation, expected).await.ok().unwrap());
    assert_eq!(world.online_session(character).unwrap().ship_id, Some(capsule));
    assert_eq!(world.with(system, move |s| s.pilot_ball(character).map(|b| b.id)).await.flatten(), Some(ship));
    assert!(world.ball(system, capsule).await.is_none());
    assert_eq!(world.with(system, move |s| s.observer_ego(1)).await.flatten(), Some(ship));
    assert!(drain_destiny(&mut scene).is_empty(), "refused publishes never touched the live binding");
    let prepared = crate::services::ship::balls::selected(&identity, &db, &world, &sde, Some(system), None, capsule, [1.0; 3]).await.ok().unwrap();
    world.with(system, move |s| { s.remove_ball(ship); prepared.install(s, |s, ball| s.add_ship(ball)); }).await.unwrap();
    assert_eq!(world.with(system, move |s| s.pilot_ball(character).map(|b| b.id)).await.flatten(), Some(capsule));
    let station = sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).unwrap();
    let home = (station.station_id, station.solar_system_id.unwrap(), station.constellation_id.unwrap(), station.region_id.unwrap());
    let docked = creation::rookie_ship(&db, TYPE_CAPSULE, character, home.0, FLAG_HANGAR).await.unwrap();
    let expected = world.online_session(character);
    let current = crate::dao::characters::ship::current(&db, character).await.unwrap().unwrap();
    assert!(crate::dao::characters::ship::set_home(&db, character, current.ship.unwrap(), current.station, docked, home.0, home.1, home.2, home.3).await.unwrap());
    let (token, generation) = world.with(system, move |s| { let snapshot = s.apply_damage(capsule, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true).unwrap().unwrap().first_lethal.unwrap(); (snapshot.token, snapshot.generation) }).await.unwrap();
    let capsule_identity = Identity { ship_id: Some(capsule), ..identity };
    assert!(publish_loss(&capsule_identity, &db, &world, &sde, LossOutcome { replacement_ship: docked, home: Some(home), position: [1.0; 3] }, token, generation, expected).await.ok().unwrap());
    let session = world.online_session(character).unwrap();
    assert_eq!((session.ship_id, session.station_id, session.system_id), (Some(docked), Some(home.0), Some(home.1)));
    assert!(world.with(system, move |s| s.pilot_ball(character).is_none() && !s.contains(docked)).await.unwrap());
    let mut replacement = session.clone();
    replacement.session_id += 1;
    let (replacement_tx, _replacement_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(character, replacement_tx);
    world.set_online_session(character, Some(replacement.clone()));
    let mut stale = session.clone();
    stale.ship_id = Some(ship);
    assert!(!world.replace_online_session(character, Some(&session), Some(stale)));
    assert_eq!(world.online_session(character), Some(replacement));
    world.unregister_pilot_generation(character, None);
    }).await;
    cleanup(&db, fixture, &victims).await;
    resume_scenario(result);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn committed_loss_reconciliation_publishes_existing_replacement() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let mut victims = Vec::new();
    let result = catch_scenario(async {
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let world = World::new(db.clone(), sde.clone(), 0);
    world.system(system).await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(character, tx);
    for capsule in [false, true] {
        let hull = if capsule { TYPE_CAPSULE } else { sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.type_id != TYPE_CAPSULE && t.attribute(ATTR_FRIGATE_ESCAPE_BAY_CAPACITY).is_some_and(|v| v > 0.0) && t.group_id.is_some_and(|g| sde.repairable_ship.enters_recovery(t.type_id, g, CATEGORY_SHIP) && !sde.repairable_ship.unrepairable(CATEGORY_SHIP, g, t.type_id))).unwrap().type_id };
        let ship = creation::positioned(&db, hull, character, system, [1.0; 3]).await.unwrap();
        victims.push(ship);
        db.execute("UPDATE game.characters SET home_station_id=$2, station_id=NULL, solar_system_id=$3, ship_id=$4 WHERE character_id=$1",
            &[&character, &sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).unwrap().station_id, &system, &ship]).await.unwrap();
        // The capsule iteration dies with a real unprotected active implant.
        let mut lost_implant = None;
        if capsule {
            let kind = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER)).unwrap().type_id;
            let item = transfers::insert(&db, kind, character, character, FLAG_IMPLANT, 1).await.unwrap();
            db.execute("UPDATE game.items SET singleton=true, brain_slot=1 WHERE item_id=$1", &[&item]).await.unwrap();
            lost_implant = Some((item, kind));
        }
        let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &FreeShip { item_id: ship, type_id: hull, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [1.0; 3] }, character, 0.0).await.ok().unwrap();
        world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        world.set_online_session(character, Some(OnlineSession { character_id: character, user_id: None, session_id: 1, ship_id: Some(ship), station_id: None, system_id: Some(system), corporation_id: Some(1), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
        let mut scene = if capsule { None } else { Some(watch_loss(&world, system, ship).await) };
        let _market = crate::services::market::lock().await;
        let fences = if capsule { Some(crate::services::effects::online::capture_online_fences(&world, &[character]).await) } else { None };
        let mut gate_systems = vec![system];
        if let Some(fences) = &fences { gate_systems.extend(fences.systems()); }
        gate_systems.sort_unstable();
        gate_systems.dedup();
        let mut _gates = Vec::with_capacity(gate_systems.len());
        for id in gate_systems { _gates.push(world.lock_condition_snapshot(id).await.unwrap()); }
        let snapshot = world.with(system, move |s| s.apply_damage(ship, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true).unwrap().unwrap().first_lethal.unwrap()).await.unwrap();
        let pending = Arc::new(facts::PlayerLossContext::new(None, [0.0; 3]));
        assert!(world.with(system, { let (pending, token, generation) = (pending.clone(), snapshot.token, snapshot.generation); move |s| s.install_player_loss_context(ship, token, generation, pending) }).await.unwrap());
        let input = LossInput { position: snapshot.position, source: snapshot.source.clone(), lethal_at: snapshot.lethal_at, contributions: snapshot.contributions.clone(), victim: snapshot.victim, context: snapshot.victim_context, conflict: snapshot.conflict, pending };
        assert!(db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| { let (sde, input, fences) = (sde.clone(), input.clone(), fences.clone());
            Box::pin(async move { destroy_ship_in_transaction(tx, outbox, &Identity { char_id: character, corp_id: 1, ship_id: Some(ship), station_id: None, system_id: Some(system) }, &sde, &input, fences.as_ref(), &[]).await })
        }, |_value, _| async move { Err::<(), _>(crate::services::CallError::Internal(anyhow::anyhow!("publication failed"))) }).await.is_err());
        let outcome = input.pending.prepared.lock().unwrap().clone().unwrap().outcome;
        drop(_gates);
        drop(_market);
        let frozen = input.pending.recovery_source.lock().unwrap().clone().unwrap();
        if crate::dao::repairable_ship::source_proof(&db, &frozen).await.unwrap().group.is_some() {
        db.execute("UPDATE game.items SET x=2 WHERE item_id=$1", &[&ship]).await.unwrap();
        crate::services::combat::damage::reconcile_pending_loss(db.clone(), world.clone(), system, ship, snapshot.clone()).await;
        assert_eq!(*input.pending.recovery_source.lock().unwrap(), Some(frozen.clone()));
        assert_eq!(selection::ship_id(&db, character).await.unwrap(), Some(outcome.replacement_ship));
        db.execute("UPDATE game.items SET x=$2 WHERE item_id=$1", &[&ship, &frozen.shape.x]).await.unwrap();
        }
        crate::services::combat::damage::reconcile_pending_loss(db.clone(), world.clone(), system, ship, snapshot.clone()).await;
        if let Some(destiny) = &mut scene {
            world.with(system, move |s| crate::space::system::tests::events::publish_scene_prefix(s, crate::space::collision::enabled())).await.unwrap();
            assert_eq!(world.with(system, move |s| s.observer_ego(1)).await.flatten(), Some(outcome.replacement_ship));
            let events = drain_destiny(destiny);
            let add = events.iter().position(|v| v.items().unwrap()[1].items().unwrap()[0].as_text().as_deref() == Some("AddBalls")).expect("replacement Add");
            let slim = &events[add].items().unwrap()[1].items().unwrap()[1].items().unwrap()[0].items().unwrap()[1].items().unwrap()[0];
            assert_eq!(crate::services::value_field(slim, "itemID").unwrap().as_int(), Some(outcome.replacement_ship));
        }
        let receipt = input.pending.prepared.lock().unwrap().clone().unwrap().outcome;
        assert_eq!((receipt.replacement_ship, receipt.home, receipt.position), (outcome.replacement_ship, outcome.home, outcome.position));
        let notices: Vec<(String, Vec<Value>)> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let expected: Vec<Value> = [Value::Int(ship), Value::Float(outcome.position[0]), Value::Float(outcome.position[1]), Value::Float(outcome.position[2]), Value::Int(outcome.replacement_ship)].into_iter().chain(outcome.home.iter().flat_map(|h| [h.0, h.1, h.2, h.3].map(Value::Int))).collect();
        assert_eq!(notices.iter().filter(|(n, _)| n == "__ship_destroyed").map(|(_, a)| a).collect::<Vec<_>>(), vec![&expected]);
        let names: Vec<&str> = notices.iter().map(|(n, _)| n.as_str()).collect();
        if let Some((item, kind)) = lost_implant {
            // The committed body's refresh replays once, after the session
            // change; nothing is deleted or settled a second time.
            assert_eq!(names, ["__ship_destroyed", "OnCloneDestruction", "OnServerImplantsChanged", "OnSkillForcedRefresh", crate::client::BRAIN_REFRESH, "OnModuleAttributeChanges"]);
            assert!(brain::get(&db, character, item).await.unwrap().is_none());
            let blob: String = db.query_one("SELECT blob FROM game.killmails WHERE victim_item_id=$1", &[&ship]).await.unwrap().get("blob");
            assert!(blob.contains(&format!("t={kind} f={FLAG_IMPLANT}")));
            assert!(db.query("SELECT 1 FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1)", &[&character]).await.unwrap().is_empty());
        } else {
            assert_eq!(names, ["__ship_destroyed", "OnModuleAttributeChanges"]);
        }
        assert_eq!(crate::db::outbox::tests::events(&db, "LossSettled", character).await.iter().filter(|event| matches!(event, GameEvent::LossSettled { victim, .. } if victim.item_id == ship)).count(), 1);
        assert_eq!(selection::ship_id(&db, character).await.unwrap(), Some(outcome.replacement_ship));
        assert!(world.ball(system, ship).await.is_none());
        assert_eq!(world.with(system, move |s| s.pilot_ball(character).map(|b| b.id)).await.flatten(), outcome.home.is_none().then_some(outcome.replacement_ship));
        crate::services::combat::damage::reconcile_pending_loss(db.clone(), world.clone(), system, ship, snapshot).await;
        assert_eq!(selection::ship_id(&db, character).await.unwrap(), Some(outcome.replacement_ship));
        assert!(rx.try_recv().is_err());
        if let Some(destiny) = &mut scene {
            world.with(system, move |s| crate::space::system::tests::events::publish_scene_prefix(s, crate::space::collision::enabled())).await.unwrap();
            assert!(!drain_destiny(destiny).iter().any(|v| v.items().unwrap()[1].items().unwrap()[0].as_text().as_deref() == Some("AddBalls")),
                "a repeated reconcile replays no successor Add");
        }
        if outcome.home.is_none() { world.remove_ball(system, outcome.replacement_ship).await; }
    }
    world.unregister_pilot_generation(character, None);
    }).await;
    cleanup(&db, fixture, &victims).await;
    resume_scenario(result);
}
