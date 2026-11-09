//! Native dispatch against the real Hub fixture. The synthetic claim below tests
//! lifecycle settlement only; the preceding native Entosis cycle tests warmup.
use std::{sync::{Arc, Mutex}, time::Duration};
use marshal::Value;
use crate::{client::Client, services::{self, Call, CallError, combat::{Ctx, targeting_charges}, identity::Identity}, space::sovereignty::links};
use super::{fixture::{self, Fixture}, scenarios};

async fn client(f: &Fixture, pilot: usize, system: i64) -> (Client, tokio::net::TcpStream) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let config = Arc::new(crate::config::Config::default());
    let login = crate::handshake::Login { user_id: f.ids.account, user_name: "fixture".into(), role: 0, client_id: f.ids.characters[pilot], session_id: f.ids.characters[pilot], language_id: "EN".into() };
    let r = &f.resources;
    let chat = crate::chat::Hub::new(&config.chat.domain, r.db.clone(), r.world.clone());
    let mut c = Client::new(crate::conn::Conn::new(stream, address), config, r.db.clone(), r.sde.clone(), chat, r.world.clone(), login);
    c.change_session(vec![("userid", Value::Int(f.ids.account)), ("charid", Value::Int(f.ids.characters[pilot])), ("shipid", Value::Int(f.ids.ships[pilot])), ("corpid", Value::Int(f.ids.corps[pilot])), ("allianceid", Value::Int(f.ids.alliances[usize::from(pilot == 2)])), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
    (c, peer)
}

async fn call(c: &mut Client, service: &str, method: &str, args: &[Value], bound: bool, params: Option<&Value>, kwargs: &Value) -> Result<Value, CallError> {
    services::dispatch(c, &Call { service, method, args, kwargs, bound, bind_params: params }).await
}
fn drop_args(rows: Vec<(i64, i64)>) -> Vec<Value> {
    vec![Value::list(rows.into_iter().map(|(id, qty)| Value::tuple([Value::Int(id), Value::Int(qty)]))), Value::None, Value::Bool(false)]
}
fn launched(value: Value, source: i64) -> i64 {
    let Value::Dict(rows) = value else { panic!("Drop did not return a dictionary") };
    let [(key, Value::List(ids))] = rows.as_slice() else { panic!("Drop did not return one source sequence") };
    assert_eq!(*key, Value::Int(source));
    let [Value::Int(id)] = ids.as_slice() else { panic!("Drop did not return one ID") };
    *id
}
async fn run(f: &Fixture) {
    let ids = f.ids; let r = &f.resources;
    let (mut c, _peer) = client(f, 0, ids.systems[0]).await;
    let empty = Value::Dict(vec![]);
    let ship_bind = Value::tuple([Value::Int(ids.systems[0]), Value::Int(infinity_core::consts::GROUP_SOLAR_SYSTEM)]);
    let park_bind = Value::Int(ids.systems[0]);
    let bad = drop_args(vec![(ids.sources[0], 1), (ids.sources[0], 1)]);
    assert!(matches!(call(&mut c, "ship", "Drop", &bad, true, Some(&ship_bind), &empty).await, Err(CallError::User { .. })));
    for (source, quantity) in [(ids.sources[0], 2), (ids.sources[1], 1), (ids.fuel[0], 1)] {
        let before = crate::dao::items::loading::load_items(&r.db, crate::dao::items::loading::ItemQuery::Id(source)).await.unwrap().remove(0).quantity;
        let value = call(&mut c, "ship", "Drop", &drop_args(vec![(source, quantity)]), true, Some(&ship_bind), &empty).await.unwrap();
        assert!(matches!(value, Value::Dict(rows) if matches!(rows.as_slice(), [(Value::Int(id), Value::List(v))] if *id == source && matches!(v.as_slice(), [Value::Tuple(error)] if matches!(error.as_slice(), [Value::Bytes(_), Value::Dict(_)])))));
        let after = crate::dao::items::loading::load_items(&r.db, crate::dao::items::loading::ItemQuery::Id(source)).await.unwrap().remove(0).quantity;
        assert_eq!(after, before);
    }
    let row = crate::dao::items::loading::load_items(&r.db, crate::dao::items::loading::ItemQuery::Id(ids.sources[0])).await.unwrap().remove(0);
    assert_eq!(row.quantity, 1);
    let mut args = drop_args(vec![(ids.sources[0], 1)]);
    args[1] = Value::Int(ids.characters[0]);
    assert!(matches!(call(&mut c, "ship", "Drop", &args, false, None, &empty).await, Err(CallError::User { .. })));
    assert!(matches!(call(&mut c, "ship", "Drop", &args, true, Some(&Value::Int(ids.systems[1])), &empty).await, Err(CallError::User { .. })));
    let hub = launched(call(&mut c, "ship", "Drop", &args, true, Some(&ship_bind), &empty).await.unwrap(), ids.sources[0]);
    assert_eq!(hub, ids.sources[0]);
    assert_eq!(launched(call(&mut c, "ship", "Drop", &args, true, Some(&ship_bind), &empty).await.unwrap(), ids.sources[0]), hub);
    let snapshot = crate::dao::sovereignty::structures::snapshots(&r.db, Some(ids.systems[0]), None).await.unwrap().remove(0);
    assert_eq!(snapshot.owner_id, r.sde.sovereignty.hub().deploy.default_owner);
    assert!(snapshot.system.claim_time.is_none());
    assert!(r.world.with(ids.systems[0], move |s| s.ball(hub).is_some()).await.unwrap());

    // Stage against a real static ball, separated by collision radii. SetBallPosition
    // preserves motion, so stop both ships at the same destiny boundary.
    let positions = r.world.with(ids.systems[0], move |s| {
        let gate = s.sampled_ball(ids.gate).unwrap();
        let ship = s.sampled_ball(ids.ships[0]).unwrap();
        let capital = s.sampled_ball(ids.ships[2]).unwrap();
        let target = s.sampled_ball(hub).unwrap();
        let gap = 100.0;
        let ship_x = gate.pos[0] + f64::from(gate.radius) + f64::from(ship.radius) + gap;
        let capital_x = ship_x + f64::from(ship.radius) + f64::from(capital.radius) + gap;
        let hub_x = capital_x + f64::from(capital.radius) + f64::from(target.radius) + gap;
        let positions = [[ship_x, gate.pos[1], gate.pos[2]], [hub_x, gate.pos[1], gate.pos[2]], [capital_x, gate.pos[1], gate.pos[2]]];
        for (id, pos) in [(ids.ships[0], positions[0]), (ids.ships[2], positions[2])] {
            s.set_ball_position(id, pos, |_, _| true);
            s.command(id, |ball, _, _| { ball.stop(); ball.vel = [0.0; 3]; Vec::new() });
        }
        s.set_ball_position(hub, positions[1], |_, _| true);
        positions
    }).await.unwrap();
    let ball_ids = [ids.ships[0], hub, ids.ships[2]];
    let positioned = tokio::time::timeout(Duration::from_secs(30), async {
        while !r.world.with(ids.systems[0], move |s| ball_ids.iter().zip(positions).all(|(id, pos)|
            s.ball(*id).is_some_and(|b| crate::space::dist(b.pos, pos) < 1.0))).await.unwrap() {
            assert!(r.world.after(ids.systems[0], 100).await);
        }
    }).await;
    if positioned.is_err() {
        let states = r.world.with(ids.systems[0], move |s| ball_ids.map(|id|
            s.ball(id).map(|b| (b.pos, b.mode, b.vel, b.radius, b.position_at(s.now_ms()))))).await;
        panic!("positioning deadline: ids={ball_ids:?}, expected={positions:?}, actual (raw, mode, velocity, radius, sampled)={states:?}");
    }
    let fit = crate::services::fit::Fit::load_in_space(&r.db, &r.world, ids.systems[0], ids.characters[0], ids.ships[0]).await.unwrap();
    let (lock_gap, surface_gap) = r.world.with(ids.systems[0], move |s| {
        let ship = s.sampled_ball(ids.ships[0]).unwrap();
        let target = s.sampled_ball(hub).unwrap();
        let gap = crate::space::dist(ship.pos, target.pos) - f64::from(target.radius);
        (gap, (gap - f64::from(ship.radius)).max(0.0))
    }).await.unwrap();
    let lock_range = fit.value(crate::services::fit::Key::Ship, infinity_core::consts::dogma::ATTR_MAX_TARGET_RANGE);
    assert!(lock_gap <= lock_range, "Hub outside fitted lock range: gap={lock_gap}, range={lock_range}");
    let metadata = crate::services::effects::registry::resolve(&r.sde, infinity_core::consts::dogma::EFFECT_ENTOSIS_LINK).unwrap();
    let entosis_range = fit.value(crate::services::fit::Key::Module(ids.modules[0]), metadata.attribute(crate::services::effects::registry::EffectAttribute::Range).unwrap());
    assert!(surface_gap <= entosis_range, "Hub outside fitted Entosis range: gap={surface_gap}, range={entosis_range}");
    r.world.register_pilot(ids.characters[0], c.notify_tx.clone());
    let identity = Identity::of_client(&c);
    let (lock_tx, mut lock_rx) = tokio::sync::mpsc::unbounded_channel();
    let cx = Ctx::from_identity(&identity, &r.db, &r.world, Some(ids.systems[0]), lock_tx).unwrap();
    let request = targeting_charges::add_target_ctx(&cx, hub, true).await.unwrap();
    let locked = tokio::time::timeout(Duration::from_secs(120), async {
        while !targeting_charges::targets_of(ids.characters[0]).contains(&hub) {
            assert!(r.world.after(ids.systems[0], 100).await);
        }
    }).await;
    if locked.is_err() {
        let states = r.world.with(ids.systems[0], move |s| ball_ids.map(|id|
            s.ball(id).map(|b| (b.pos, b.mode, b.vel, b.radius, b.position_at(s.now_ms()), b.cloak, b.char_id)))).await;
        let mut notices = Vec::new();
        while let Ok(notice) = lock_rx.try_recv() { notices.push(notice); }
        panic!("Hub target lock deadline: request={request:?}, targets={:?}, notices={notices:?}, ids={ball_ids:?}, expected={positions:?}, actual (raw, mode, velocity, radius, sampled, cloak, pilot)={states:?}", targeting_charges::targets_of(ids.characters[0]));
    }
    let effect = r.sde.dogma.effect(infinity_core::consts::dogma::EFFECT_ENTOSIS_LINK).unwrap().name.clone().unwrap();
    let duration_attr = metadata.attribute(crate::services::effects::registry::EffectAttribute::Duration).unwrap();
    let duration_ms = fit.value(crate::services::fit::Key::Module(ids.modules[0]), duration_attr);
    assert!(duration_ms.is_finite() && duration_ms > 0.0 && duration_ms <= 300_000.0, "fixture cycle exceeds test budget: {duration_ms} ms");
    let stop_timeout = Duration::from_secs_f64(duration_ms / 1000.0) + Duration::from_secs(30);
    let dogma_bind = ship_bind.clone();
    let fuel_before: i64 = r.db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&ids.fuel[0]]).await.unwrap().get("quantity");
    let activate = [Value::Int(ids.modules[0]), Value::str(&effect), Value::Int(hub), Value::Int(10)];
    call(&mut c, "dogmaIM", "Activate", &activate, true, Some(&dogma_bind), &empty).await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async { while !r.world.with(ids.systems[0], move |s| links::blocks(s, ids.ships[0])).await.unwrap() { assert!(r.world.after(ids.systems[0], 100).await); } }).await.unwrap();
    let fuel_after: i64 = r.db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&ids.fuel[0]]).await.unwrap().get("quantity");
    assert!(fuel_after < fuel_before);
    let team = r.world.with(ids.systems[0], move |s| s.ball(hub).and_then(|b| b.slim.iter().find(|(name, _)| *name == "entosis_score")).and_then(|(_, v)| v.items()).and_then(|v| v.get(1)).and_then(Value::as_int)).await.unwrap();
    assert_eq!(team, None, "first paid cycle is warmup");
    call(&mut c, "dogmaIM", "Deactivate", &[Value::Int(ids.modules[0]), Value::str(&effect)], true, Some(&dogma_bind), &empty).await.unwrap();
    assert!(r.world.with(ids.systems[0], move |s| links::blocks(s, ids.ships[0])).await.unwrap());
    tokio::time::timeout(stop_timeout, async { while r.world.with(ids.systems[0], move |s| links::blocks(s, ids.ships[0])).await.unwrap() { assert!(r.world.after(ids.systems[0], 100).await); } }).await.unwrap();

    // Trusted synthetic endpoint, not evidence of real capture scoring.
    scenarios::claim_for(f, hub, 0).await;
    let before = crate::dao::sovereignty::structures::snapshots(&r.db, Some(ids.systems[0]), None).await.unwrap().remove(0);
    let transfer = [Value::Int(hub), Value::Int(ids.corps[0]), Value::Int(ids.corps[1])];
    for (bound, selector, args) in [(false, None, transfer.as_slice()), (true, Some(&Value::Int(ids.systems[1])), transfer.as_slice()), (true, Some(&park_bind), &transfer[..2])] {
        assert!(matches!(call(&mut c, "beyonce", "CmdTransferCorpAssetOwnership", args, bound, selector, &empty).await, Err(CallError::User { .. })));
    }
    let wrong_owner = [Value::Int(hub), Value::Int(ids.corps[1]), Value::Int(ids.corps[0])];
    assert!(matches!(call(&mut c, "beyonce", "CmdTransferCorpAssetOwnership", &wrong_owner, true, Some(&park_bind), &empty).await, Err(CallError::User { .. })));
    call(&mut c, "beyonce", "CmdTransferCorpAssetOwnership", &transfer, true, Some(&park_bind), &empty).await.unwrap();
    let after = crate::dao::sovereignty::structures::snapshots(&r.db, Some(ids.systems[0]), None).await.unwrap().remove(0);
    assert_eq!(after.owner_id, ids.corps[1]); assert_eq!(after.system.claim_time, before.system.claim_time);
    // The Hub was staged in space above; its durable deployment coordinates
    // are not the actor's current position used by the destruction range guard.
    let owner_radius = r.world.with(ids.systems[1], move |s| s.ball(ids.ships[1]).unwrap().radius).await.unwrap();
    let owner_position = r.world.with(ids.systems[0], move |s| {
        let target = s.sampled_ball(hub).unwrap();
        [target.pos[0], target.pos[1], target.pos[2] + f64::from(target.radius) + f64::from(owner_radius) + 100.0]
    }).await.unwrap();
    f.install_pilot(1, 0, owner_position).await;
    let ready = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let in_range = r.world.with(ids.systems[0], move |s| {
                s.pilot_ball(ids.characters[1]).and_then(|pilot| s.sampled_ball(pilot.id))
                    .zip(s.sampled_ball(hub)).is_some_and(|(ship, target)| {
                        let gap = crate::space::dist(ship.pos, target.pos) - f64::from(ship.radius) - f64::from(target.radius);
                        ship.id == ids.ships[1] && gap.is_finite() && gap <= infinity_core::consts::DRONE_SCOOP_RANGE
                    })
            }).await.unwrap();
            if in_range { break; }
            assert!(r.world.after(ids.systems[0], 100).await);
        }
    }).await;
    if ready.is_err() {
        let states = r.world.with(ids.systems[0], move |s| [ids.ships[1], hub].map(|id|
            s.ball(id).map(|b| (b.pos, b.mode, b.vel, b.radius, b.position_at(s.now_ms()), b.char_id)))).await;
        panic!("owner positioning deadline: expected={owner_position:?}, actual (raw, mode, velocity, radius, sampled, pilot)={states:?}");
    }
    let (mut owner, _peer2) = client(f, 1, ids.systems[0]).await;
    let item = [Value::Int(hub)];
    call(&mut owner, "beyonce", "CmdSelfDestructStructure", &item, true, Some(&park_bind), &empty).await.unwrap();
    let requested = crate::dao::sovereignty::structures::read(&r.db, hub).await.unwrap().unwrap();
    assert!(requested.self_destruct_due_at.is_some());
    let job = crate::services::sovereignty::jobs::destruct_id(hub);
    let count: i64 = r.db.query_one("SELECT count(*) AS count FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap().get("count");
    assert_eq!(count, 1);
    call(&mut owner, "beyonce", "CmdAbortSelfDestructStructure", &item, true, Some(&park_bind), &empty).await.unwrap();
    assert!(crate::dao::sovereignty::structures::read(&r.db, hub).await.unwrap().unwrap().self_destruct_due_at.is_none());
    let count: i64 = r.db.query_one("SELECT count(*) AS count FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap().get("count"); assert_eq!(count, 0);
    let notices: i64 = r.db.query_one("SELECT count(*) AS count FROM game.notifications WHERE receiver_id=$1 AND type_id IN (167,168)", &[&ids.characters[1]]).await.unwrap().get("count"); assert_eq!(notices, 2);
    let star = r.sde.map.celestials(ids.systems[1]).find(|c| c.group_id == Some(i64::from(infinity_core::consts::GROUP_SUN))).unwrap();
    let position = [star.x.unwrap() + r.sde.sovereignty.hub().deploy.fixed_star_distance_m as f64, star.y.unwrap(), star.z.unwrap()];
    f.install_pilot(1, 1, position).await;
    // A separate system permits a real stack split and replay.
    let (mut other, _peer3) = client(f, 1, ids.systems[1]).await;
    let bind = Value::tuple([Value::Int(ids.systems[1]), Value::Int(infinity_core::consts::GROUP_SOLAR_SYSTEM)]);
    let stack = drop_args(vec![(ids.sources[1], 1)]);
    let split = launched(call(&mut other, "ship", "Drop", &stack, true, Some(&bind), &empty).await.unwrap(), ids.sources[1]);
    assert_ne!(split, ids.sources[1]);
    assert_eq!(launched(call(&mut other, "ship", "Drop", &stack, true, Some(&bind), &empty).await.unwrap(), ids.sources[1]), split);
    let row = crate::dao::items::loading::load_items(&r.db, crate::dao::items::loading::ItemQuery::Id(ids.sources[1])).await.unwrap().remove(0); assert_eq!(row.quantity, 1);
}

#[test]
#[ignore = "requires a dedicated initialized INFINITY_TEST_DATABASE_URL and real SDE; run this exact test in its own process"]
fn native_hub_lifecycle_dispatch() {
    crate::tests::init_config();
    let state = Arc::new(Mutex::new(fixture::Recorded::default()));
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let shared = state.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(async {
        let mut task = tokio::spawn(async move { let f = Fixture::new(shared).await; run(&f).await; });
        let result = tokio::time::timeout(Duration::from_secs(420), &mut task).await;
        if result.is_err() { task.abort(); let _ = task.await; }
        if let Some(world) = state.lock().unwrap().world.clone() { crate::space::sovereignty::close(&world); }
        let _fence = tokio::time::timeout(Duration::from_secs(10), services::market::lock()).await.expect("market cleanup fence timed out");
        match result { Ok(Ok(())) => {}, Ok(Err(e)) if e.is_panic() => std::panic::resume_unwind(e.into_panic()), Ok(Err(e)) => panic!("native integration task cancelled: {e}"), Err(_) => panic!("native integration timed out") }
    })));
    drop(runtime);
    let ids = state.lock().unwrap().ids;
    let cleanup = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let cleaned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cleanup.block_on(async { tokio::time::timeout(Duration::from_secs(40), fixture::cleanup(ids)).await.expect("fixture cleanup timed out") })));
    if let Err(error) = cleaned { if let Err(original) = result { std::panic::resume_unwind(original); } std::panic::resume_unwind(error); }
    if let Err(error) = result { std::panic::resume_unwind(error); }
}
