use crate::dao::items::dogma as item_dogma;
use crate::services::combat;
use crate::services::drones::tests::fixture::Fixture;
use crate::services::effects::tests::fixture::{dogma_call, module_online, publish_fit};
use crate::services::fit::{runtime::RuntimeReceiverKey, Key};
use infinity_core::consts::dogma::{ATTR_BREACHER_POD_ACTIVATED_DAMAGE_RECEIVED_PERCENTAGE, ATTR_BREACHER_POD_DAMAGE_RESISTANCE,
    ATTR_CAPACITOR_CAPACITY, ATTR_CPU_OUTPUT, ATTR_DURATION, ATTR_HULL_DAMAGE, ATTR_POWER_OUTPUT, ATTR_RECHARGE_RATE,
    ATTR_STRUCTURE_HP, DBUFF_BPDC_DAMAGE_RECEIVED_PERCENTAGE, EFFECT_MODULE_BONUS_BREACHER_POD_DAMAGE_CONTROL};
use marshal::Value;

/// The real SDE breach-control module online on a piloted fixture ship.
struct InSpace { f: Fixture, module: i64, name: String, received: f64, rx: tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)> }

async fn scenario(body: impl std::ops::AsyncFnOnce(&mut InSpace)) {
    let mut f = Fixture::new().await;
    let module_type = f.sde.types.iter().find(|t| t.published == Some(true)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_MODULE_BONUS_BREACHER_POD_DAMAGE_CONTROL)).map(|t| t.type_id).expect("type");
    let module: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id",
        &[&module_type, &f.owner, &f.ship, &crate::services::effects::fitting::rack(&f.sde.types, module_type).map(|(_, _, first)| first).expect("rack")]).await.unwrap().get("item_id");
    for (i, a, v) in [(module, ATTR_DURATION, 1_200.0), (module, ATTR_STRUCTURE_HP, 55.0), (f.ship, ATTR_CAPACITOR_CAPACITY, 1e9),
        (f.ship, ATTR_RECHARGE_RATE, 1e6), (f.ship, ATTR_CPU_OUTPUT, 1e9), (f.ship, ATTR_POWER_OUTPUT, 1e9)] { f.set(i, a, v).await; }
    crate::services::skills::gm::apply(&mut f.client, f.owner, crate::services::skills::gm::Change::Set(
        crate::services::skills::requirements::requirements(&f.sde, module_type).collect())).await.unwrap();
    publish_fit(&f.db, &f.world, f.system, f.owner, f.ship).await;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel(); f.world.register_pilot(f.owner, tx);
    let mut space = InSpace { module, rx,
        name: f.sde.dogma.effect(EFFECT_MODULE_BONUS_BREACHER_POD_DAMAGE_CONTROL).and_then(|e| e.name.clone()).expect("name"),
        received: f.sde.types.get(module_type).and_then(|t| t.attribute(ATTR_BREACHER_POD_ACTIVATED_DAMAGE_RECEIVED_PERCENTAGE)).expect("received"), f };
    let result = crate::tests::catch_scenario(body(&mut space)).await;
    combat::reset(&space.f.client);
    space.f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

/// Activate once, then wait for the actor-observable end state.
async fn burnt(space: &mut InSpace) {
    let (module, ship, system) = (space.module, space.f.ship, space.f.system);
    dogma_call(&mut space.f.client, "Activate", &[Value::Int(module), Value::str(&space.name), Value::None, Value::Int(0)]).await.ok().expect("activate");
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let done = space.f.world.with(system, move |s| (
                s.with_combat_fit(ship, |fit, _, _| fit.module_state(module).is_some_and(|(online, _)| !online)),
                s.runtime_collection_state(RuntimeReceiverKey::Ship(ship)).iter().any(|(c, _, _)| *c == DBUFF_BPDC_DAMAGE_RECEIVED_PERCENTAGE),
                combat::state().active.contains_key(&module))).await.unwrap();
            if done == (Some(true), true, false) { break; }
            assert!(space.f.world.after(system, 100).await);
        }
    }).await.expect("committed burn, offline and landing");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn breach_control_activation_burns_out_module_offlines_and_lands_dbuff() {
    scenario(async |space| {
        let (module, ship, expected) = (space.module, space.f.ship, 1.0 + space.received / crate::services::fit::PERCENT_SCALE);
        burnt(space).await;
        assert!((item_dogma::overrides(&space.f.db, module).await.unwrap().into_iter()
            .find(|r| r.attribute_id == ATTR_HULL_DAMAGE).map(|r| r.value).expect("burn") - 55.0).abs() < 1e-9, "burn to effective hp");
        assert_eq!(space.f.db.query_opt("SELECT online FROM game.items WHERE item_id=$1", &[&module]).await.unwrap().map(|r| r.get::<_, bool>("online")), Some(false));
        let (resident, state) = space.f.world.with(space.f.system, move |s| (
            s.with_combat_fit(ship, |fit, _, _| (fit.module_state(module).map(|(online, _)| online), fit.value(Key::Ship, ATTR_BREACHER_POD_DAMAGE_RESISTANCE))),
            s.runtime_collection_state(RuntimeReceiverKey::Ship(ship)))).await.unwrap();
        assert_eq!(resident.and_then(|(online, _)| online), Some(false), "the resident fit must publish the offline");
        assert!((resident.map(|(_, r)| r).unwrap_or_default() - expected).abs() < 0.01, "effective resistance must fold through the evaluator");
        assert!(state.iter().any(|(c, _, e)| *c == DBUFF_BPDC_DAMAGE_RECEIVED_PERCENTAGE && e.is_some()), "the landed source carries a deadline");
        let ev: Vec<_> = std::iter::from_fn(|| space.rx.try_recv().ok()).collect();
        assert!(ev.iter().any(|(n, a)| n == "OnDbuffUpdated" && *a == crate::services::effects::notify::command_burst_dbuff_updated(ship, &state)), "complete dbuff state");
        // The stopped, offlined module keeps the buff until the shared deadline.
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while space.f.world.with(space.f.system, move |s| s.runtime_collection_state(RuntimeReceiverKey::Ship(ship))
                .iter().any(|(c, _, _)| *c == DBUFF_BPDC_DAMAGE_RECEIVED_PERCENTAGE)).await.unwrap() {
                assert!(space.f.world.after(space.f.system, 100).await); }
        }).await.expect("independent expiry");
        let cleared = space.f.world.with(space.f.system, move |s| s.runtime_collection_state(RuntimeReceiverKey::Ship(ship))).await.unwrap();
        let ev: Vec<_> = std::iter::from_fn(|| space.rx.try_recv().ok()).collect();
        assert!(ev.iter().any(|(n, a)| n == "OnDbuffUpdated" && *a == crate::services::effects::notify::command_burst_dbuff_updated(ship, &cleared)), "cleared dbuff state");
        let restored = space.f.world.with(space.f.system, move |s| s.with_combat_fit(ship, |fit, key, _| fit.value(key, ATTR_BREACHER_POD_DAMAGE_RESISTANCE))).await.flatten().expect("resident fit");
        assert!((restored - 1.0).abs() < 1e-9, "resistance restores after expiry");
    }).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn burnt_breach_control_refuses_reactivation() {
    scenario(async |space| {
        let (module, ship) = (space.module, space.f.ship);
        burnt(space).await;
        assert!(module_online(&space.f.client, ship, module, true).await.is_err(), "a burnt module must refuse online");
        assert!(dogma_call(&mut space.f.client, "Activate", &[Value::Int(module), Value::str(&space.name), Value::None, Value::Int(0)]).await.is_err(), "a burnt module must refuse activation");
        assert_eq!(item_dogma::overrides(&space.f.db, module).await.unwrap().iter().filter(|r| r.attribute_id == ATTR_HULL_DAMAGE).count(), 1, "no second damage write");
        let landed = space.f.world.with(space.f.system, move |s| s.runtime_collection_state(RuntimeReceiverKey::Ship(ship))
            .iter().find(|(c, _, expiry)| *c == DBUFF_BPDC_DAMAGE_RECEIVED_PERCENTAGE && expiry.is_some_and(|d| d > s.now_ms())).is_some()).await.unwrap();
        assert!(landed, "the refused reuse left the original buff untouched");
    }).await;
}
