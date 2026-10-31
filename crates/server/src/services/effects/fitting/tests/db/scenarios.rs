//! Real database regressions for skill-driven fitting cascades and bank teardown.
//! Exercises skillsvc GM mutations and dogma reconciliation against disposable game rows.
//! Actor-driven weapon cycles are covered by the separate lifecycle bank integration test.
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

use crate::{
    client::Client,
    consts::{ATTR_CPU, ATTR_CPU_LOAD, ATTR_CPU_OUTPUT, ATTR_POWER, ATTR_POWER_LOAD, ATTR_POWER_OUTPUT, CATEGORY_SKILL, EFFECT_ONLINE, FLAG_CARGO, FLAG_HI_SLOT_FIRST},
    dao::items::{combat as item_combat, dogma},
    sde::Sde,
    services::{self, CallError, effects::online::{self, OnlineChanged}, fit::{Fit, Key}, skills::gm::{self, Change}},
};
use marshal::Value;
use tokio::sync::mpsc::UnboundedReceiver;

struct ActiveMarker(i64);

impl Drop for ActiveMarker {
    fn drop(&mut self) {
        let mut state = services::combat::state();
        if let Some(active) = state.active.remove(&self.0) {
            active.stop.store(true, Ordering::Relaxed);
        }
        state.activation_generation.remove(&self.0);
    }
}

struct ForeignCaller<'a> {
    client: &'a mut Client,
    owner: i64,
    char_id: Value,
    sender: crate::space::NotifyTx,
}

impl Drop for ForeignCaller<'_> {
    fn drop(&mut self) {
        self.client.world.unregister_pilot(self.owner);
        self.client.session.apply(vec![("charid", self.char_id.clone())]);
        self.client.notify_tx = self.sender.clone();
    }
}

pub(super) async fn exercise(client: &mut Client, f: &super::fixture::Fixture, sde: &Arc<Sde>, events: &mut UnboundedReceiver<(String, Vec<Value>)>) {
    let f = *f;
    let db = client.db.clone();
    let ship_type: i32 = db.query_one("SELECT type_id FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&f.ship, &f.owner]).await.unwrap().get("type_id");
    let module_type: i32 = db.query_one("SELECT type_id FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&f.modules[0], &f.owner]).await.unwrap().get("type_id");
    let station: i64 = db.query_one("SELECT location_id FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&f.ship, &f.owner]).await.unwrap().get("location_id");
    let other_ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,0,1,true) RETURNING item_id", &[&ship_type, &f.owner, &station]).await.unwrap().get("item_id");
    let other_module: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,false) RETURNING item_id", &[&module_type, &f.owner, &other_ship, &FLAG_HI_SLOT_FIRST]).await.unwrap().get("item_id");
    db.execute("UPDATE game.items SET online=false WHERE item_id=ANY($1) AND owner_id=$2", &[&&f.modules[..], &f.owner]).await.unwrap();
    for ship in [f.ship, other_ship] {
        assert!(dogma::set_override(&db, ship, ATTR_CPU_OUTPUT, 1000.0).await.unwrap());
        assert!(dogma::set_override(&db, ship, ATTR_POWER_OUTPUT, 1000.0).await.unwrap());
    }
    for module in [f.modules[0], other_module] {
        assert!(dogma::set_override(&db, module, ATTR_CPU, 0.0).await.unwrap());
        assert!(dogma::set_override(&db, module, ATTR_POWER, 0.0).await.unwrap());
    }

    // Find an actual SDE skill that increases this hull's output; evaluate the
    // real fit rather than assuming a particular client type ID or bonus formula.
    let candidates: Vec<_> = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SKILL) && t.published == Some(true))
        .filter(|t| [ATTR_CPU_OUTPUT, ATTR_POWER_OUTPUT].into_iter().any(|attr|
            t.modifiers(&sde.dogma, attr).any(|(_, m)| m.func == "ItemModifier" && m.domain == "shipID")))
        .map(|t| t.type_id).collect();
    let mut selected = None;
    for skill in candidates {
        gm::apply(client, f.owner, Change::Set(vec![(skill, 1)])).await.ok().expect("seed fitting skill");
        let low = Fit::load(&db, sde.clone(), f.owner, f.ship).await.ok().unwrap();
        let low_values = [low.value(Key::Ship, ATTR_CPU_OUTPUT), low.value(Key::Ship, ATTR_POWER_OUTPUT)];
        gm::apply(client, f.owner, Change::Set(vec![(skill, 5)])).await.ok().expect("raise fitting skill");
        let high = Fit::load(&db, sde.clone(), f.owner, f.ship).await.ok().unwrap();
        let high_values = [high.value(Key::Ship, ATTR_CPU_OUTPUT), high.value(Key::Ship, ATTR_POWER_OUTPUT)];
        if let Some(index) = (0..2).find(|&i| high_values[i] > low_values[i] + 1e-4) {
            selected = Some((skill, [ATTR_CPU, ATTR_POWER][index], (high_values[index] + low_values[index]) / 2.0));
            break;
        }
        gm::apply(client, f.owner, Change::Remove(vec![skill])).await.ok().expect("remove nonmatching skill");
    }
    let (skill, resource, usage) = selected.expect("published SDE fitting skill that increases hull CPU or power output");
    for module in [f.modules[0], other_module] {
        assert!(dogma::set_override(&db, module, resource, usage).await.unwrap());
    }
    let modules = [f.modules[0], other_module];
    assert_eq!(db.execute("UPDATE game.items SET online=true WHERE item_id=ANY($1) AND owner_id=$2", &[&&modules[..], &f.owner]).await.unwrap(), 2);
    let (load_attr, output_attr) = if resource == ATTR_CPU { (ATTR_CPU_LOAD, ATTR_CPU_OUTPUT) } else { (ATTR_POWER_LOAD, ATTR_POWER_OUTPUT) };
    for ship in [f.ship, other_ship] {
        let fit = Fit::load(&db, sde.clone(), f.owner, ship).await.ok().unwrap();
        assert!(fit.value(Key::Ship, load_attr) <= fit.value(Key::Ship, output_attr), "initial fit must be legal at level five");
    }
    while events.try_recv().is_ok() {}
    let (owner_tx, mut owner_events) = tokio::sync::mpsc::unbounded_channel();
    let (gm_tx, mut gm_events) = tokio::sync::mpsc::unbounded_channel();
    let char_id = client.session.get("charid").clone();
    let sender = client.notify_tx.clone();
    let caller = ForeignCaller { client, owner: f.owner, char_id, sender };
    caller.client.world.register_pilot(f.owner, owner_tx);
    caller.client.session.apply(vec![("charid", Value::None)]);
    caller.client.notify_tx = gm_tx;
    gm::apply(caller.client, f.owner, Change::Set(vec![(skill, 1)])).await.ok().expect("lower fitting skill");
    for ship in [f.ship, other_ship] { assert!(dogma::online(&db, ship).await.unwrap().is_empty()); }
    let mut stopped = Vec::new();
    let mut attributes = Vec::new();
    while let Ok((name, payload)) = owner_events.try_recv() {
        if name == "OnGodmaShipEffect" && payload.get(3).and_then(Value::as_int) == Some(0) {
            if let Some(id) = payload.first().and_then(Value::as_int) { stopped.push(id); }
        }
        if name == "OnModuleAttributeChanges" {
            for id in modules { if super::offline_attribute(&payload, f.owner, id) { attributes.push(id); } }
        }
    }
    for id in modules {
        assert!(stopped.contains(&id), "missing owner skill-reduction offline notification for {id}");
        assert!(attributes.contains(&id), "missing owner skill-reduction isOnline notification for {id}");
    }
    assert!(gm_events.try_recv().is_err(), "owner notifications reached the foreign GM");

    gm::apply(caller.client, f.owner, Change::Set(vec![(skill, 5)])).await.ok().expect("restore fitting skill");
    assert_eq!(db.execute("UPDATE game.items SET online=true WHERE item_id=ANY($1) AND owner_id=$2", &[&&modules[..], &f.owner]).await.unwrap(), 2);
    while owner_events.try_recv().is_ok() {}
    assert!(gm_events.try_recv().is_err(), "owner skill notifications reached the foreign GM");
    gm::apply(caller.client, f.owner, Change::Remove(vec![skill])).await.ok().expect("remove fitting skill");
    for ship in [f.ship, other_ship] { assert!(dogma::online(&db, ship).await.unwrap().is_empty()); }
    let mut stopped = Vec::new();
    let mut attributes = Vec::new();
    while let Ok((name, payload)) = owner_events.try_recv() {
        if name == "OnGodmaShipEffect" && payload.get(3).and_then(Value::as_int) == Some(0) {
            if let Some(id) = payload.first().and_then(Value::as_int) { stopped.push(id); }
        }
        if name == "OnModuleAttributeChanges" {
            for id in modules { if super::offline_attribute(&payload, f.owner, id) { attributes.push(id); } }
        }
    }
    for id in modules {
        assert!(stopped.contains(&id), "missing owner skill-removal offline notification for {id}");
        assert!(attributes.contains(&id), "missing owner skill-removal isOnline notification for {id}");
    }
    assert!(gm_events.try_recv().is_err(), "owner notifications reached the foreign GM");
    drop(caller);

    // A bank is destroyed in the same transaction as a forced offline. Rollback
    // must retain both membership and online state and dispatch nothing.
    assert_eq!(db.execute("UPDATE game.items SET flag_id=$1, online=true WHERE item_id=$2 AND owner_id=$3 AND location_id=$4", &[&(FLAG_HI_SLOT_FIRST + 1), &f.modules[1], &f.owner, &f.ship]).await.unwrap(), 1);
    assert_eq!(db.execute("UPDATE game.items SET online=true WHERE item_id=$1 AND owner_id=$2 AND location_id=$3", &[&f.modules[0], &f.owner, &f.ship]).await.unwrap(), 1);
    for (id, attr, value) in [(f.ship, ATTR_CPU_OUTPUT, 1.0), (f.ship, ATTR_POWER_OUTPUT, 1000.0), (f.modules[0], ATTR_CPU, 20.0), (f.modules[1], ATTR_CPU, 10.0), (f.modules[0], ATTR_POWER, 0.0), (f.modules[1], ATTR_POWER, 0.0)] {
        assert!(dogma::set_override(&db, id, attr, value).await.unwrap());
    }
    item_combat::move_bank(&db, f.ship, f.modules[0], f.modules[1]).await.unwrap();
    while events.try_recv().is_ok() {}
    let scenario_sde = sde.clone();
    let rolled_back: Result<(), CallError> = db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox<OnlineChanged>| {
        let sde = scenario_sde.clone();
        Box::pin(async move {
            online::reconcile_core(tx, &sde, outbox, f.owner, f.ship).await?;
            assert!(crate::dao::items::charges::banks(tx, f.ship).await.map_err(services::db_err)?.is_empty());
            Err(CallError::Internal(anyhow::anyhow!("intentional fixture rollback")))
        })
    }, |(), _| async move { panic!("rolled-back bank change dispatched") }).await;
    assert!(rolled_back.is_err());
    assert_eq!(dogma::online(&db, f.ship).await.unwrap().len(), 2);
    assert_eq!(crate::dao::items::charges::banks(&db, f.ship).await.unwrap().len(), 1);
    assert!(events.try_recv().is_err());
    online::reconcile_ship(client, f.owner, f.ship).await.ok().expect("committed overload reconciliation");
    assert!(dogma::online(&db, f.ship).await.unwrap().is_empty());
    assert!(crate::dao::items::charges::banks(&db, f.ship).await.unwrap().is_empty());
    assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|(name, _)| name == "OnWeaponBanksChanged"));

    // Unfitting an unbanked running module must stop its captured generation
    // only after the move commits; rollback must leave the generation untouched.
    assert!(dogma::set_override(&db, f.ship, ATTR_CPU_OUTPUT, 1000.0).await.unwrap());
    assert!(dogma::set_override(&db, f.ship, ATTR_POWER_OUTPUT, 1000.0).await.unwrap());
    assert_eq!(db.execute("UPDATE game.items SET online=true WHERE item_id=$1 AND owner_id=$2 AND location_id=$3", &[&f.modules[0], &f.owner, &f.ship]).await.unwrap(), 1);
    let stop = Arc::new(AtomicBool::new(false));
    let _marker = ActiveMarker(f.modules[0]);
    {
        let mut state = services::combat::state();
        state.active.insert(f.modules[0], services::combat::ActiveModule { char_id: f.owner, stop: stop.clone(), forced_inactive: false });
        state.activation_generation.insert(f.modules[0], stop.clone());
    }
    let policy = online::OnlinePolicy::from_client(client).ok().unwrap();
    let scenario_sde = sde.clone();
    while events.try_recv().is_ok() {}
    let rollback: Result<(), CallError> = db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox<OnlineChanged>| {
        let policy = policy.clone();
        let sde = scenario_sde.clone();
        Box::pin(async move {
            assert!(online::set_online_core(tx, &sde, &policy, outbox, f.ship, f.modules[0], false).await?);
            Err(CallError::Internal(anyhow::anyhow!("intentional unfit rollback")))
        })
    }, |(), _| async move { panic!("rolled-back unfit dispatched") }).await;
    assert!(rollback.is_err());
    assert!(!stop.load(Ordering::Relaxed));
    assert!(dogma::online(&db, f.ship).await.unwrap().iter().any(|m| m.item_id == f.modules[0]));
    assert!(events.try_recv().is_err());

    let moved = services::inventory::move_item(client, &services::inventory::Container::Item(f.ship), f.modules[0], Some(f.ship), Some(FLAG_CARGO), None).await.ok().expect("unfit running module");
    assert_eq!(moved, Some(f.modules[0]));
    assert!(stop.load(Ordering::Relaxed));
    let row = db.query_one("SELECT location_id,flag_id,online FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&f.modules[0], &f.owner]).await.unwrap();
    assert_eq!(row.get::<_, i64>("location_id"), f.ship);
    assert_eq!(row.get::<_, i32>("flag_id"), FLAG_CARGO);
    assert!(!row.get::<_, bool>("online"));
    let mut effect = false;
    let mut attribute = false;
    while let Ok((name, payload)) = events.try_recv() {
        if name == "OnGodmaShipEffect" && payload.first() == Some(&Value::Int(f.modules[0]))
            && payload.get(1).and_then(Value::as_int) == Some(EFFECT_ONLINE)
            && payload.get(3).and_then(Value::as_int) == Some(0) { effect = true; }
        if name == "OnModuleAttributeChanges" && super::offline_attribute(&payload, f.owner, f.modules[0]) { attribute = true; }
    }
    assert!(effect && attribute, "unfit must publish online-effect and isOnline stops");
}
