//! Cancellation after a real boarding receipt, before caller-side session finish.
//! Exercises ship.Board and character selection against game data and the SDE.

use infinity_core::consts::dogma::{ATTR_AGILITY, ATTR_ARMOR_HP, ATTR_MASS, ATTR_MAX_VELOCITY, ATTR_REQUIRED_SKILLS, ATTR_REQUIRED_SKILL_LEVELS, ATTR_SHIELD_CAPACITY, ATTR_SIGNATURE_RADIUS, ATTR_STRUCTURE_HP};
use crate::dao::items::{creation, space_ships::FreeShip};
use crate::services::{Call, drones::tests::fixture::Fixture};
use marshal::Value;

/// Test tolerance for the fixture's one-metre surface separation, not the RPC limit.
const FIXTURE_GAP_TOLERANCE: f64 = 2.0;

fn flight_kind(f: &Fixture) -> i32 {
    f.sde.types.iter().filter(|t| t.published == Some(true)
        && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && [ATTR_SIGNATURE_RADIUS, ATTR_AGILITY, ATTR_MAX_VELOCITY, ATTR_MASS,
            ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP].iter()
            .all(|attr| t.fit_attribute(*attr).is_some_and(|v| v.is_finite() && v > 0.0))
        && crate::services::ship::balls::radius(&f.sde, t.type_id).is_ok())
        .map(|t| t.type_id).min().expect("published flight-capable SDE ship with positive HP")
}

pub(super) async fn prepare(f: &mut Fixture, kind: i32) {
    let system = f.sde.map.system(f.system).unwrap();
    crate::dao::characters::ship::location(&f.db, f.owner, f.system,
        system.constellation_id.unwrap(), system.region_id.unwrap()).await.unwrap();
    let radius = f64::from(crate::services::ship::balls::radius(&f.sde, kind).unwrap());
    let pos = f.world.with(f.system, move |s| {
        let bound = s.balls().filter(|b| b.pos[0].is_finite() && b.radius.is_finite())
            .map(|b| b.pos[0] + f64::from(b.radius)).fold(0.0_f64, f64::max);
        [bound + radius * 4.0 + 1.0, 0.0, 0.0]
    }).await.unwrap();
    let old = creation::positioned(&f.db, kind, f.owner, f.system, pos).await.unwrap();
    crate::dao::characters::selection::set_ship(&f.db, f.owner, old).await.unwrap();
    f.world.remove_ball(f.system, f.ship).await;
    f.ship = old;
    f.client.session.apply(vec![("shipid", Value::Int(old)), ("userid", Value::Int(f.account)),
        ("solarsystemid2", Value::Int(f.system))]);
    let online = infinity_core::session::OnlineSession::from_session(&f.client.session).unwrap();
    f.world.set_online_session(f.owner, Some(online));
    let row = FreeShip { item_id: old, type_id: kind, owner_id: f.owner, corporation_id: 1, alliance_id: -1, pos };
    let pilot = crate::services::ship::balls::piloted(&f.db, f.sde.clone(), &row, f.owner, 0.0).await.unwrap();
    f.world.with(f.system, move |s| pilot.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    f.world.observe(f.system, f.client.login.client_id, old, f.client.destiny_tx.clone()).await.unwrap();
    let ty = f.sde.types.get(kind).unwrap();
    let levels: Vec<_> = ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS).filter_map(|(&skill, level)| {
        let id = ty.attribute(skill).unwrap_or(0.0) as i32;
        let level = ty.attribute(level).unwrap_or(0.0) as i32;
        (id > 0 && level > 0).then_some((id, level))
    }).collect();
    let (sde, world, owner) = (f.sde.clone(), f.world.clone(), f.owner);
    let market = crate::services::market::lock().await;
    let fences = crate::services::effects::online::capture_online_fences(&world, &[owner]).await;
    f.db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone(); let levels = levels.clone(); let fences = fences.clone();
        Box::pin(async move { crate::services::skills::gm::set_levels(tx, outbox, &sde, &fences, owner, &levels, crate::client::filetime_now()).await })
    }, |(), outbox| async move { crate::services::effects::online::dispatch_changed_world((), outbox, &world) }).await.unwrap();
    drop(market);
}

pub(super) async fn free_ship(f: &Fixture, kind: i32) -> i64 {
    let radius = f64::from(crate::services::ship::balls::radius(&f.sde, kind).unwrap());
    let old = f.ship;
    let pos = f.world.with(f.system, move |s| {
        let pilot = s.ball(old).expect("fixture pilot ball");
        let distance = f64::from(pilot.radius) + radius + 1.0;
        let directions = [[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, -1.0]];
        directions.into_iter().map(|d| std::array::from_fn(|i| pilot.pos[i] + d[i] * distance))
            .find(|pos| s.balls().filter(|b| b.id != old).all(|b|
                crate::space::dist(*pos, b.pos) > radius + f64::from(b.radius)))
            .expect("clear in-range fixture position")
    }).await.unwrap();
    let id = creation::positioned(&f.db, kind, f.owner, f.system, pos).await.unwrap();
    let row = FreeShip { item_id: id, type_id: kind, owner_id: f.owner, corporation_id: 1, alliance_id: -1, pos };
    let ship = crate::services::ship::balls::unpiloted(&f.db, f.sde.clone(), &row).await.unwrap();
    f.world.with(f.system, move |s| ship.install(s, |s, ball| s.add_persistent_ship(ball))).await.unwrap();
    id
}
pub(super) async fn cargo(f: &Fixture) -> i64 {
    let kind = f.sde.types.iter().find(|t| t.published == Some(true)
        && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && f.sde.is_tradable(t.type_id)).unwrap().type_id;
    crate::dao::items::transfers::insert(&f.db, kind, f.owner, f.ship, infinity_core::consts::FLAG_CARGO, 3).await.unwrap()
}

pub(super) async fn cargo_retained(f: &Fixture, item: i64, ship: i64) -> bool {
    crate::dao::items::loading::load_items(&f.db, crate::dao::items::loading::ItemQuery::Id(item)).await.unwrap()
        .first().is_some_and(|row| row.owner_id == f.owner && row.location_id == ship
            && row.flag_id == infinity_core::consts::FLAG_CARGO && row.quantity == 3)
}

async fn board_diagnostics(db: &crate::db::Db, world: &crate::space::World,
    system: i64, owner: i64, old: i64, target: i64, account: i64, sid: i64) -> (bool, String) {
    let selected = crate::dao::characters::selection::ship_id(db, owner).await.unwrap();
    let items = crate::dao::items::loading::load_items(db, crate::dao::items::loading::ItemQuery::Ids(&[old, target])).await.unwrap();
    let items: Vec<_> = items.iter().map(|i| (i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton)).collect();
    let online = world.online_session(owner);
    let facts = world.with(system, move |s| {
        let describe = |id| s.ball(id).map(|b| (b.id, b.owner_id, b.char_id, b.mode, b.cloak, b.interactive, b.pos, b.radius,
            s.persistent_generation(id), s.combat_membership_generation(id), s.combat_phase(id)));
        let refusal = s.authorize_board(owner, old, target, FIXTURE_GAP_TOLERANCE).err();
        let live = [old, target].into_iter().all(|id| s.combat_phase(id) == Some(crate::space::combat::CombatPhase::Live));
        (refusal.is_none() && live, format!("refusal={refusal:?} pilot={:?} old={:?} target={:?}", s.pilot_ball(owner).map(|b| b.id), describe(old), describe(target)))
    }).await;
    let (admitted, actor) = facts.unwrap_or((false, "system stopped".into()));
    let session_matches = online.as_ref().is_some_and(|s| s.character_id == owner && s.user_id == Some(account)
        && s.session_id == sid && s.ship_id == Some(old) && s.system_id == Some(system) && s.station_id.is_none());
    (admitted && selected == Some(old) && session_matches, format!("account={account} sid={sid} system={system} selected={selected:?} online={online:?} items={items:?} {actor}"))
}


pub(super) async fn cancel_after_receipt(f: &mut Fixture, method: &str, args: &[Value], bound: bool) -> (i64, f64) {
    let config = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut blocker, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
    let driver = tokio::spawn(connection);
    let pid: i32 = blocker.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
    let tx = blocker.transaction().await.unwrap();
    let kwargs = Value::None;
    let params = Value::tuple([Value::Int(f.system), Value::Int(infinity_core::consts::GROUP_SOLAR_SYSTEM)]);
    let call = Call { service: "ship", method, args, kwargs: &kwargs, bound, bind_params: bound.then_some(&params) };
    let db = f.db.clone();
    let world = f.world.clone();
    let (owner, system, old) = (f.owner, f.system, f.ship);
    let (account, sid) = (f.client.login.user_id, f.client.session.sid);
    let client_facts = format!("client userid={:?} charid={:?} shipid={:?} system={:?} system2={:?} station={:?}",
        f.client.session.int("userid"), f.client.session.int("charid"), f.client.session.int("shipid"),
        f.client.session.int("solarsystemid"), f.client.session.int("solarsystemid2"), f.client.session.int("stationid"));
    let target = (method == "Board").then(|| args[0].as_int().expect("Board target"));
    if let Some(target) = target {
        let (admitted, facts) = board_diagnostics(&db, &world, system, owner, old, target, account, sid).await;
        assert!(admitted, "boarding precondition: {facts}; {client_facts}");
    }
    let mut rpc = Box::pin(crate::services::dispatch(&mut f.client, &call));
    // The market mutex is fair: enqueue this RPC first so a periodic checkpoint
    // cannot become the row-lock waiter mistaken for the boarding worker.
    let test_market = crate::services::market::lock().await;
    std::future::poll_fn(|cx| match std::future::Future::poll(rpc.as_mut(), cx) {
        std::task::Poll::Pending => std::task::Poll::Ready(()),
        std::task::Poll::Ready(result) => panic!("boarding completed before market admission: {result:?}"),
    }).await;
    assert!(crate::dao::items::ship::lock_reload_ship(&tx, old).await.unwrap());
    drop(test_market);
    let blocked = async {
        loop {
            let row = db.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))) AS blocked", &[&pid]).await.unwrap();
            if row.get::<_, bool>("blocked") { break; }
            tokio::task::yield_now().await;
        }
    };
    tokio::select! {
        _ = blocked => {},
        result = &mut rpc => {
            let facts = if let Some(target) = target {
                board_diagnostics(&db, &world, system, owner, old, target, account, sid).await.1
            } else { format!("account={account} sid={sid} system={system} old={old}") };
            panic!("boarding completed before row lock: {result:?}; {facts}; {client_facts}");
        },
    }
    tx.rollback().await.unwrap();
    let market = crate::services::market::lock().await;
    let selected = crate::dao::characters::selection::ship_id(&db, owner).await.unwrap().unwrap();
    assert_ne!(selected, old);
    assert_eq!(world.with(system, move |s| s.pilot_ball(owner).map(|b| b.id)).await.unwrap(), Some(selected));
    let generation = crate::space::targeting::next_generation();
    let weak = std::sync::Arc::downgrade(&world);
    let (raw, mut attempt) = world.with(system, move |s| {
        let condition = s.combat_condition_snapshot(selected).unwrap().condition;
        let channel = (0..4).find(|&i| condition.stats.resonance[0][i] > 0.0 && condition.stats.resonance[1][i] > 0.0).unwrap();
        let shield = s.ship_condition(selected).unwrap().0;
        let mut raw = [0.0; 4];
        raw[channel] = shield / condition.stats.resonance[0][channel]
            + condition.stats.max[1] * 0.1 / condition.stats.resonance[1][channel];
        let facts = s.prepare_combat_source(selected, None, None, None,
            crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1,
            move |s| s.pilot_ball(owner).is_some_and(|b| b.id == selected)).unwrap();
        (raw, crate::services::effects::space_source::PreparedCombatAttempt::new(facts, weak, system))
    }).await.unwrap();
    let facts = attempt.facts();
    let outcome = crate::services::combat::damage::apply_damage_guarded(&db, &world, system, selected, raw,
        crate::services::combat::damage::DamageSource { attacker_id: Some(owner),
            attacker_ball_id: Some(selected), source_ship_id: Some(selected),
            source_ship_type: Some(facts.entity.type_id), facts: Some(facts), ..Default::default() },
        false, |_| true).await.unwrap().unwrap();
    assert!(outcome.dealt > 0.0 && !outcome.lethal);
    let armor = world.with(system, move |s| {
        s.finish_combat_attempt(&facts);
        s.retire_combat_source(generation).unwrap();
        s.ship_condition(selected).unwrap().1
    }).await.unwrap();
    attempt.disarm();
    assert!(armor > 0.0);
    drop(rpc);
    assert_eq!(f.client.session.int("shipid"), Some(old));
    drop(market);
    drop(blocker);
    driver.await.unwrap().unwrap();
    (selected, armor)
}

pub(super) async fn leave_and_select(f: &mut Fixture, selected: i64, armor: f64) -> Result<(), String> {
    f.client.leave_world().await.map_err(|e| e.to_string())?;
    f.client.change_session(vec![("charid", Value::None), ("shipid", Value::None),
        ("solarsystemid", Value::None), ("solarsystemid2", Value::None)]);
    let args = [Value::Int(f.owner)];
    let kwargs = Value::None;
    let call = Call { service: "charUnboundMgr", method: "SelectCharacterID", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::dispatch(&mut f.client, &call).await.map_err(|e| format!("{e:?}"))?;
    if f.client.session.int("shipid") != Some(selected) { return Err("wrong selected hull".into()); }
    let owner = f.owner;
    let count = f.world.with(f.system, move |s| (s.pilot_ball(owner).map(|b| b.id), s.pilots().filter(|b| b.char_id == Some(owner)).count(), s.ship_condition(selected).map(|c| c.1))).await;
    if count != Some((Some(selected), 1, Some(armor))) { return Err(format!("wrong pilot membership or armor: {count:?}")); }
    Ok(())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn cancelled_board_receipt_is_checkpointed_before_reselection() {
    let mut f = Fixture::real_system().await;
    let kind = flight_kind(&f);
    prepare(&mut f, kind).await;
    let target = free_ship(&f, kind).await;
    let old = f.ship;
    let cargo_id = cargo(&f).await;
    let balance = crate::dao::wallets::characters::balance(&f.db, f.owner).await.unwrap();
    let (selected, armor) = cancel_after_receipt(&mut f, "Board", &[Value::Int(target), Value::Int(old)], true).await;
    let result = leave_and_select(&mut f, selected, armor).await;
    let after_balance = crate::dao::wallets::characters::balance(&f.db, f.owner).await.unwrap();
    let retained = crate::dao::items::loading::load_items(&f.db, crate::dao::items::loading::ItemQuery::Ids(&[old, target])).await.unwrap().len();
    let cargo_ok = cargo_retained(&f, cargo_id, old).await;
    f.cleanup().await;
    assert_eq!(selected, target);
    assert_eq!(balance, after_balance);
    assert_eq!(retained, 2);
    assert!(cargo_ok);
    assert_eq!(result, Ok(()));
}
