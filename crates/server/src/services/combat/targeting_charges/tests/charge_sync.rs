//! Charge completion wire shape and reservation regressions against real SDE rows.
use super::*;
use super::super::completion::apply_loads;
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation};

pub(super) fn assert_prime(args: &[Value], sde: &Sde, ship: i64, flag: i32, kind: i32, quantity: i64) {
    assert_eq!(args.len(), 2);
    assert_eq!(args[0], Value::Int(ship));
    let Value::Instance(_, state) = &args[1] else { panic!("charge info must be a KeyVal") };
    assert_eq!(state.get("itemID"), Some(&charge_key(ship, flag, kind)));
    assert_eq!(state.get("invItem"), Some(&Value::None));
    assert_eq!(state.get("activeEffects"), Some(&Value::Dict(vec![])));
    let time = state.get("time").and_then(Value::as_int).expect("prime time");
    assert!(time > 0);
    assert_eq!(state.get("wallclockTime"), Some(&Value::Int(time)));
    let Some(Value::Dict(attrs)) = state.get("attributes") else { panic!("charge attributes") };
    assert!(attrs.contains(&(Value::Int(i64::from(infinity_core::consts::dogma::ATTR_QUANTITY)), Value::Float(quantity as f64))));
    let (id, value) = sde.types.attributes(kind).find(|(id, _)| *id != infinity_core::consts::dogma::ATTR_QUANTITY).expect("charge static attribute");
    assert!(attrs.contains(&(Value::Int(i64::from(id)), Value::Float(value))));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn mixed_source_completion_loads_only_surviving_rows_once() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let (module_type, charge_type) = compatible_ammunition(&sde);
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).expect("published ship").type_id;
    let flag = infinity_core::consts::FLAG_FITTING_FIRST;
    let fixture = create_ship(&db, ship_type, ShipLocation::Docked { station: 1 }, &[
        ShipItemSpec { type_id: module_type, flag_id: flag, quantity: 1, singleton: true, online: true, attributes: &[] },
    ]).await.unwrap();
    let module = fixture.module();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let (notify, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let insert = |amount, source| crate::dao::items::pending_reloads::insert(&test_db, fixture.character, fixture.ship, module, flag, charge_type, amount, source, fixture.ship);
        let cargo = insert(2, FLAG_CARGO).await.unwrap();
        let ammo = insert(3, FLAG_SPECIALIZED_AMMO_HOLD).await.unwrap();
        let pending = PendingLoad { owner: fixture.character, reload_ids: vec![cargo, ammo], external_sources: vec![], external_facts: Vec::new(), load: (module, flag, charge_type, 5, 0) };
        apply_loads(&test_db, &world, &notify, fixture.ship, vec![pending.clone()]).await.unwrap();
        assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), Some((charge_type, 5)));
        let (name, args) = rx.try_recv().unwrap();
        assert_eq!(name, "OnGodmaPrimeItem");
        assert_prime(&args, &sde, fixture.ship, flag, charge_type, 5);
        while rx.try_recv().is_ok() {}
        apply_loads(&test_db, &world, &notify, fixture.ship, vec![pending]).await.unwrap();
        assert!(rx.try_recv().is_err());
        let cargo = insert(2, FLAG_CARGO).await.unwrap();
        let ammo = insert(3, FLAG_SPECIALIZED_AMMO_HOLD).await.unwrap();
        crate::dao::items::pending_reloads::take(&test_db, cargo).await.unwrap();
        crate::dao::items::combat::apply_loads(&test_db, fixture.ship, &[(module, flag, charge_type, 4, 0)]).await.unwrap();
        assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), Some((charge_type, 4)));
        apply_loads(&test_db, &world, &notify, fixture.ship, vec![PendingLoad { owner: fixture.character, reload_ids: vec![cargo, ammo], external_sources: vec![], external_facts: Vec::new(), load: (module, flag, charge_type, 10, 5) }]).await.unwrap();
        assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), Some((charge_type, 7)));
        let (name, args) = rx.try_recv().unwrap();
        assert_eq!(name, "OnModuleAttributeChanges");
        let row = &args[0].items().unwrap()[0];
        let fields = row.items().unwrap();
        assert_eq!(fields[2], charge_key(fixture.ship, flag, charge_type));
        assert_eq!(fields[3].as_int(), Some(i64::from(infinity_core::consts::dogma::ATTR_QUANTITY)));
        assert_eq!(fields[5].as_int(), Some(7));
        assert_eq!(fields[6].as_int(), Some(4));
        assert!(rx.try_recv().is_err());
        crate::dao::items::combat::delete_charge(&test_db, fixture.ship, flag).await.unwrap();
        let cargo = insert(2, FLAG_CARGO).await.unwrap();
        let ammo = insert(3, FLAG_SPECIALIZED_AMMO_HOLD).await.unwrap();
        crate::dao::items::pending_reloads::take(&test_db, cargo).await.unwrap();
        apply_loads(&test_db, &world, &notify, fixture.ship, vec![PendingLoad { owner: fixture.character, reload_ids: vec![cargo, ammo], external_sources: vec![], external_facts: Vec::new(), load: (module, flag, charge_type, 5, 0) }]).await.unwrap();
        assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), Some((charge_type, 3)));
        let (name, args) = rx.try_recv().unwrap();
        assert_eq!(name, "OnGodmaPrimeItem");
        assert_prime(&args, &sde, fixture.ship, flag, charge_type, 3);
        crate::dao::items::combat::delete_charge(&test_db, fixture.ship, flag).await.unwrap();
        while rx.try_recv().is_ok() {}
        let station = sde.map.stations().next().unwrap().station_id;
        let native = insert(2, FLAG_CARGO).await.unwrap();
        let static_id = crate::dao::items::pending_reloads::insert(&test_db, fixture.character, fixture.ship, module, flag, charge_type, 3, infinity_core::consts::FLAG_HANGAR, station).await.unwrap();
        crate::dao::items::pending_reloads::take(&test_db, native).await.unwrap();
        apply_loads(&test_db, &world, &notify, fixture.ship, vec![PendingLoad { owner: fixture.character, reload_ids: vec![native, static_id], external_sources: vec![], external_facts: Vec::new(), load: (module, flag, charge_type, 5, 0) }]).await.unwrap();
        assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), Some((charge_type, 3)));
        let (name, args) = rx.try_recv().unwrap();
        assert_eq!(name, "OnGodmaPrimeItem");
        assert_prime(&args, &sde, fixture.ship, flag, charge_type, 3);
        let (name, _) = rx.try_recv().unwrap();
        assert_eq!(name, "OnModuleAttributeChanges");
        assert!(rx.try_recv().is_err());
    }).await;
    db.execute("DELETE FROM game.pending_reloads WHERE ship_id = $1", &[&fixture.ship]).await.unwrap();
    cleanup_ship(&db, None, fixture).await.unwrap();
    result.unwrap();
}
const RECOUNT_FIRST: i64 = 8;
const RECOUNT_SECOND: i64 = 2;
const RECOUNT_RELOAD_MS: f64 = 60_000.0;
const RECOUNT_CARGO: f64 = 1_000_000.0;

struct Recount {
    fixture: crate::services::drones::tests::fixture::Fixture,
    module: i64,
    charge_type: i32,
    flag: i32,
    other_type: i32,
    other_module_type: i32,
    rx: tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>,
}

fn magazine_room(module: &crate::sde::types::TypeData, charge: &crate::sde::types::TypeData) -> i64 {
    let capacity = module.capacity.unwrap_or(0.0).max(module.attribute(ATTR_CAPACITY).unwrap_or(0.0));
    let volume = charge.volume.unwrap_or(0.0);
    if volume <= 0.0 { return 0; }
    (capacity / volume.max(1e-6) + 1e-6).floor() as i64
}

async fn drop_probe(db: &Db, id: i64) {
    db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&id]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
}

async fn recount_stage() -> Recount {
    let mut fixture = crate::services::drones::tests::fixture::Fixture::new().await;
    fixture.set(fixture.ship, ATTR_CAPACITY, RECOUNT_CARGO).await;
    let size = fixture.sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
    let candidates: Vec<(i32, i32, i32)> = fixture.sde.types.iter().filter(|module| module.published == Some(true)
        && module.category_id == Some(infinity_core::consts::CATEGORY_MODULE) && fixture.sde.is_tradable(module.type_id))
        .filter_map(|module| {
            let groups: Vec<_> = CHARGE_GROUP_ATTRIBUTES.iter().flat_map(|name| fixture.sde.dogma.attributes_by_name(name))
                .filter_map(|attr| module.attribute(attr.attribute_id)).collect();
            let charges: Vec<_> = fixture.sde.types.iter().filter(|charge| charge.published == Some(true)
                && charge.category_id == Some(CATEGORY_CHARGE) && fixture.sde.is_tradable(charge.type_id)
                && charge_matches(groups.iter().copied(), charge.group_id, size.and_then(|id| module.attribute(id)), size.and_then(|id| charge.attribute(id)))
                && magazine_room(module, charge) >= RECOUNT_FIRST + RECOUNT_SECOND).map(|charge| charge.type_id).collect();
            (charges.len() >= 2).then(|| (module.type_id, charges[0], charges[1]))
        }).collect();
    let mut chosen = None;
    for (module_type, charge_type, other_type) in candidates {
        for (skill, level) in crate::services::skills::requirements::requirements(&fixture.sde, module_type) {
            let rank = fixture.sde.types.get(skill).and_then(|ty| ty.fit_attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT)).expect("module skill rank");
            crate::dao::skills::training::set_level(&fixture.db, fixture.owner, skill, level, crate::services::skills::formulas::sp_for_level(rank, i64::from(level))).await.unwrap();
        }
        let module = crate::dao::items::creation::rookie_ship(&fixture.db, module_type, fixture.owner, fixture.ship, FLAG_CARGO).await.unwrap();
        fixture.set(module, ATTR_RELOAD_TIME, RECOUNT_RELOAD_MS).await;
        let Some(flag) = crate::services::effects::fitting::free_slot(&fixture.db, fixture.sde.clone(), fixture.owner, fixture.ship, module, module_type).await.ok().flatten() else {
            drop_probe(&fixture.db, module).await;
            continue;
        };
        let item = load_items(&fixture.db, &fixture.sde, ItemQuery::Id(module)).await.unwrap().pop().unwrap();
        assert!(crate::dao::items::inventory::move_item(&fixture.db, &item, fixture.ship, flag, fixture.owner, true).await.unwrap());
        if crate::services::effects::fitting::validate_fit(&fixture.db, fixture.sde.clone(), fixture.owner, fixture.ship, module).await.is_err() {
            drop_probe(&fixture.db, module).await;
            continue;
        }
        chosen = Some((module, module_type, charge_type, other_type, flag));
        break;
    }
    let (module, module_type, charge_type, other_type, flag) = chosen.expect("fixture hull has no legal ten-round magazine slot");
    let other_module_type = fixture.sde.types.iter().find(|module| module.type_id != module_type && module.published == Some(true)
        && module.category_id == Some(infinity_core::consts::CATEGORY_MODULE) && fixture.sde.is_tradable(module.type_id))
        .expect("second published module").type_id;
    let fit = Fit::load(&fixture.db, fixture.sde.clone(), fixture.owner, fixture.ship).await.unwrap();
    let condition = crate::services::combat::prepare_condition_with_fit(&fixture.db, fixture.ship, &fit).await.unwrap();
    let ship = fixture.ship;
    fixture.world.with(fixture.system, move |s| s.install_condition(ship, condition.into_condition(), fit)).await.unwrap();
    let (sender, rx) = tokio::sync::mpsc::unbounded_channel();
    fixture.client.notify_tx = sender.clone();
    fixture.world.register_pilot(fixture.owner, sender);
    Recount { fixture, module, charge_type, flag, other_type, other_module_type, rx }
}

fn load_call<'a>(args: &'a [Value]) -> Call<'a> {
    Call { service: "dogmaIM", method: "LoadAmmo", args, kwargs: &Value::None, bound: true, bind_params: None }
}

async fn load_stack(stage: &mut Recount, quantity: i64) -> i64 {
    let source = crate::dao::items::transfers::insert(&stage.fixture.db, stage.charge_type, stage.fixture.owner, stage.fixture.ship, FLAG_CARGO, quantity).await.unwrap();
    let args = [Value::Int(stage.fixture.ship), Value::Int(stage.module), Value::list([Value::Int(source)]), Value::Int(stage.fixture.ship)];
    let call = load_call(&args);
    assert_eq!(crate::services::dispatch(&mut stage.fixture.client, &call).await.unwrap(), Value::None);
    source
}

async fn pending_row(stage: &Recount) -> crate::dao::items::pending_reloads::PendingReload {
    let rows = crate::dao::items::pending_reloads::for_ship(&stage.fixture.db, stage.fixture.ship).await.unwrap();
    assert_eq!(rows.len(), 1);
    rows.into_iter().next().unwrap()
}

fn charge_quantity(args: &[Value], ship: i64, flag: i32, kind: i32) -> Option<(i64, i64)> {
    args.first()?.items()?.iter().find_map(|row| {
        let fields = row.items()?;
        (fields.get(2) == Some(&charge_key(ship, flag, kind)) && fields.get(3).and_then(Value::as_int) == Some(i64::from(infinity_core::consts::dogma::ATTR_QUANTITY)))
            .then(|| (fields.get(5).and_then(Value::as_int).unwrap(), fields.get(6).and_then(Value::as_int).unwrap()))
    })
}

async fn wait_marker(module: i64, marker: i64) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if !state().reloading.contains_key(&module) { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap_or_else(|_| panic!("reload marker {marker} was not cleared"));
}

async fn drive_timer(stage: &mut Recount, marker: i64) {
    let system = stage.fixture.system;
    let due = crate::client::unix_ms_from_filetime_ceil(marker);
    stage.fixture.world.with(system, move |s| s.tick(due)).await.unwrap();
}

async fn charge_total(stage: &Recount) -> i128 {
    let ship = stage.fixture.ship;
    let owner = stage.fixture.owner;
    let kind = stage.charge_type;
    let loaded = loaded(&stage.fixture.db, ship, stage.flag).await.unwrap().filter(|(ty, _)| *ty == kind).map(|(_, quantity)| i128::from(quantity)).unwrap_or(0);
    let cargo: i64 = stage.fixture.db.query_one("SELECT COALESCE(SUM(quantity),0)::bigint AS quantity FROM game.items WHERE owner_id=$1 AND type_id=$2 AND location_id=$3 AND flag_id=$4", &[&owner, &kind, &ship, &FLAG_CARGO]).await.unwrap().get("quantity");
    let pending: i64 = stage.fixture.db.query_one("SELECT COALESCE(SUM(quantity),0)::bigint AS quantity FROM game.pending_reloads WHERE ship_id=$1 AND type_id=$2", &[&ship, &kind]).await.unwrap().get("quantity");
    loaded + i128::from(cargo) + i128::from(pending)
}

async fn cleanup_recount(stage: Recount) {
    let ship = stage.fixture.ship;
    let system = stage.fixture.system;
    let world = stage.fixture.world.clone();
    let module_id = stage.module;
    let marker = { state().reloading.get(&module_id).copied() };
    if let Some(marker) = marker {
        world.with(system, move |s| s.tick(crate::client::unix_ms_from_filetime_ceil(marker))).await.unwrap();
        wait_marker(module_id, marker).await;
    }
    stage.fixture.db.execute("DELETE FROM game.pending_reloads WHERE ship_id=$1", &[&ship]).await.unwrap();
    stage.fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn reload_completion_after_unload_preserves_charge_total() {
    let mut stage = recount_stage().await;
    let result = crate::tests::catch_scenario(async {
        let source = load_stack(&mut stage, RECOUNT_FIRST).await;
        let marker = *state().reloading.get(&stage.module).expect("first reload marker");
        stage.fixture.world.with(stage.fixture.system, |_| ()).await.unwrap();
        drive_timer(&mut stage, marker).await;
        wait_marker(stage.module, marker).await;
        assert_eq!(loaded(&stage.fixture.db, stage.fixture.ship, stage.flag).await.unwrap(), Some((stage.charge_type, RECOUNT_FIRST)));
        assert!(load_items(&stage.fixture.db, &stage.fixture.sde, ItemQuery::Id(source)).await.unwrap().is_empty());
        while stage.rx.try_recv().is_ok() {}
        stage.fixture.client.outbox.clear();
        let second = load_stack(&mut stage, RECOUNT_SECOND).await;
        let marker = *state().reloading.get(&stage.module).expect("second reload marker");
        stage.fixture.world.with(stage.fixture.system, |_| ()).await.unwrap();
        let pending = pending_row(&stage).await;
        assert_eq!((pending.module_id, pending.flag_id, pending.type_id, pending.owner_id, pending.source_location_id, pending.quantity), (stage.module, stage.flag, stage.charge_type, stage.fixture.owner, stage.fixture.ship, RECOUNT_SECOND));
        assert_eq!(loaded(&stage.fixture.db, stage.fixture.ship, stage.flag).await.unwrap(), Some((stage.charge_type, RECOUNT_FIRST)));
        assert!(load_items(&stage.fixture.db, &stage.fixture.sde, ItemQuery::Id(second)).await.unwrap().is_empty());
        while stage.rx.try_recv().is_ok() {}
        stage.fixture.client.outbox.clear();
        unload_ammo_core(&mut stage.fixture.client, stage.fixture.ship, vec![stage.module], stage.fixture.ship, FLAG_CARGO).await.unwrap();
        assert_eq!(loaded(&stage.fixture.db, stage.fixture.ship, stage.flag).await.unwrap(), None);
        let cargo = load_items(&stage.fixture.db, &stage.fixture.sde, ItemQuery::LocationFlagTypeOwnerStack(stage.fixture.ship, FLAG_CARGO, stage.charge_type, stage.fixture.owner)).await.unwrap().pop().unwrap();
        assert_eq!((cargo.owner_id, cargo.type_id, cargo.location_id, cargo.flag_id, cargo.quantity), (stage.fixture.owner, stage.charge_type, stage.fixture.ship, FLAG_CARGO, RECOUNT_FIRST));
        assert!(pending_row(&stage).await == pending, "pending row changed during unload");
        assert!(state().reloading.get(&stage.module) == Some(&marker));
        while stage.rx.try_recv().is_ok() {}
        drive_timer(&mut stage, marker).await;
        wait_marker(stage.module, marker).await;
        let loaded_row = loaded(&stage.fixture.db, stage.fixture.ship, stage.flag).await.unwrap();
        let cargo = load_items(&stage.fixture.db, &stage.fixture.sde, ItemQuery::Id(cargo.item_id)).await.unwrap().pop().unwrap();
        let pending_left = crate::dao::items::pending_reloads::for_ship(&stage.fixture.db, stage.fixture.ship).await.unwrap();
        let total = charge_total(&stage).await;
        assert_eq!((loaded_row, cargo.quantity, pending_left.len(), total), (Some((stage.charge_type, RECOUNT_SECOND)), RECOUNT_FIRST, 0, i128::from(RECOUNT_FIRST + RECOUNT_SECOND)));
        assert!(load_items(&stage.fixture.db, &stage.fixture.sde, ItemQuery::Id(second)).await.unwrap().is_empty());
        let notices: Vec<_> = std::iter::from_fn(|| stage.rx.try_recv().ok()).collect();
        let primes: Vec<_> = notices.iter().filter(|(name, args)| name == "OnGodmaPrimeItem" && args.get(1).and_then(|info| match info { Value::Instance(_, state) => state.get("itemID"), _ => None }) == Some(&charge_key(stage.fixture.ship, stage.flag, stage.charge_type))).collect();
        assert_eq!(primes.len(), 1);
        assert_prime(&primes[0].1, &stage.fixture.sde, stage.fixture.ship, stage.flag, stage.charge_type, RECOUNT_SECOND);
        assert!(!notices.iter().any(|(name, args)| name == "OnModuleAttributeChanges" && charge_quantity(args, stage.fixture.ship, stage.flag, stage.charge_type).is_some_and(|(_, old)| old == RECOUNT_FIRST)));
        let module = stage.module;
        let ship = stage.fixture.ship;
        let charge_type = stage.charge_type;
        let fitted = stage.fixture.world.with(stage.fixture.system, move |s| s.with_combat_fit(ship, move |fit, _, _| fit.source_type(Key::Charge(module)))).await.unwrap();
        assert_eq!(fitted, Some(Some(charge_type)));
    }).await;
    cleanup_recount(stage).await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn reload_completion_retains_reservation_on_identity_conflict() {
    for conflict in [true, false] {
        let mut stage = recount_stage().await;
        let result = crate::tests::catch_scenario(async {
            load_stack(&mut stage, RECOUNT_FIRST).await;
            let first = *state().reloading.get(&stage.module).expect("initial marker");
            stage.fixture.world.with(stage.fixture.system, |_| ()).await.unwrap();
            drive_timer(&mut stage, first).await;
            wait_marker(stage.module, first).await;
            while stage.rx.try_recv().is_ok() {}
            load_stack(&mut stage, RECOUNT_SECOND).await;
            let marker = *state().reloading.get(&stage.module).expect("pending marker");
            stage.fixture.world.with(stage.fixture.system, |_| ()).await.unwrap();
            let pending = pending_row(&stage).await;
            assert_eq!(pending.quantity, RECOUNT_SECOND);
            let ship = stage.fixture.ship;
            let owner = stage.fixture.owner;
            let module = stage.module;
            let flag = stage.flag;
            let other_type = stage.other_type;
            if conflict {
                stage.fixture.db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
                    assert!(crate::dao::items::ship::lock_reload_ship(tx, ship).await?);
                    tx.execute("UPDATE game.module_charges SET type_id=$3 WHERE ship_id=$1 AND flag_id=$2", &[&ship, &flag, &other_type]).await?;
                    Ok::<_, crate::db::Error>(())
                }), |(), _: Outbox| async { Ok::<_, crate::db::Error>(()) }).await.unwrap();
            } else {
                let other_module_type = stage.other_module_type;
                let replacement = crate::dao::items::creation::rookie_ship(&stage.fixture.db, other_module_type, owner, ship, FLAG_CARGO).await.unwrap();
                stage.fixture.db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
                    assert!(crate::dao::items::ship::lock_reload_ship(tx, ship).await?);
                    tx.execute("UPDATE game.items SET flag_id=$3 WHERE item_id=$1 AND location_id=$2", &[&module, &ship, &FLAG_CARGO]).await?;
                    tx.execute("UPDATE game.items SET flag_id=$3 WHERE item_id=$1 AND location_id=$2", &[&replacement, &ship, &flag]).await?;
                    Ok::<_, crate::db::Error>(())
                }), |(), _: Outbox| async { Ok::<_, crate::db::Error>(()) }).await.unwrap();
            }
            let before_loaded = loaded(&stage.fixture.db, ship, flag).await.unwrap();
            let before_module = load_items(&stage.fixture.db, &stage.fixture.sde, ItemQuery::Id(module)).await.unwrap().pop().unwrap();
            let before_slot = load_items(&stage.fixture.db, &stage.fixture.sde, ItemQuery::LocationFlags(ship, &[flag])).await.unwrap();
            let before_cargo = load_items(&stage.fixture.db, &stage.fixture.sde, ItemQuery::OwnerLocationFlag(owner, ship, FLAG_CARGO)).await.unwrap();
            while stage.rx.try_recv().is_ok() {}
            drive_timer(&mut stage, marker).await;
            wait_marker(module, marker).await;
            assert!(pending_row(&stage).await == pending, "rollback keeps the pending row");
            assert_eq!(loaded(&stage.fixture.db, ship, flag).await.unwrap(), before_loaded);
            let after_module = load_items(&stage.fixture.db, &stage.fixture.sde, ItemQuery::Id(module)).await.unwrap().pop().unwrap();
            assert_eq!((after_module.location_id, after_module.flag_id, after_module.type_id, after_module.owner_id), (before_module.location_id, before_module.flag_id, before_module.type_id, before_module.owner_id));
            let after_slot = load_items(&stage.fixture.db, &stage.fixture.sde, ItemQuery::LocationFlags(ship, &[flag])).await.unwrap();
            assert_eq!(after_slot.iter().map(|item| (item.item_id, item.type_id, item.flag_id)).collect::<Vec<_>>(), before_slot.iter().map(|item| (item.item_id, item.type_id, item.flag_id)).collect::<Vec<_>>());
            let after_cargo = load_items(&stage.fixture.db, &stage.fixture.sde, ItemQuery::OwnerLocationFlag(owner, ship, FLAG_CARGO)).await.unwrap();
            assert_eq!(after_cargo.iter().map(|item| (item.item_id, item.quantity)).collect::<Vec<_>>(), before_cargo.iter().map(|item| (item.item_id, item.quantity)).collect::<Vec<_>>());
            let notices: Vec<_> = std::iter::from_fn(|| stage.rx.try_recv().ok()).collect();
            assert!(!notices.iter().any(|(name, _)| name == "OnGodmaPrimeItem"));
            assert!(!notices.iter().any(|(name, args)| name == "OnModuleAttributeChanges" && charge_quantity(args, ship, flag, stage.charge_type).is_some()));
        }).await;
        cleanup_recount(stage).await;
        if let Err(panic) = result { std::panic::resume_unwind(panic); }
    }
}


#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn docked_native_load_unload_and_generated_reload_prime() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let size = sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
    let (module_type, charge_type) = sde.types.iter().filter(|m| m.published == Some(true)
        && m.category_id == Some(infinity_core::consts::CATEGORY_MODULE)
        && m.group_id.is_some_and(|group| infinity_core::consts::GROUPABLE_GROUP_IDS.contains(&i64::from(group))))
        .find_map(|m| {
            let groups: Vec<_> = CHARGE_GROUP_ATTRIBUTES.iter().flat_map(|name| sde.dogma.attributes_by_name(name))
                .filter_map(|a| m.attribute(a.attribute_id)).collect();
            let capacity = m.capacity.unwrap_or(0.0).max(m.attribute(ATTR_CAPACITY).unwrap_or(0.0));
            sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE)
                && charge_matches(groups.iter().copied(), c.group_id, size.and_then(|id| m.attribute(id)), size.and_then(|id| c.attribute(id)))
                && c.volume.is_some_and(|v| v > 0.0 && capacity >= v && capacity / v < 100_000.0))
                .map(|c| (m.type_id, c.type_id))
        }).expect("groupable module with compatible charge");
    let required = sde.types.get(module_type).unwrap().capacity.unwrap_or(0.0).max(sde.types.get(module_type).unwrap().attribute(ATTR_CAPACITY).unwrap_or(0.0));
    let mut selected = None;
    for hull in sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && t.capacity.unwrap_or(0.0).max(t.attribute(ATTR_CAPACITY).unwrap_or(0.0)) >= required
        && t.attribute(infinity_core::consts::dogma::ATTR_HI_SLOTS).is_some_and(|v| v >= 2.0)) {
        let f = create_ship(&db, hull.type_id, ShipLocation::Docked { station: 1 }, &[
            ShipItemSpec { type_id: module_type, flag_id: FLAG_CARGO, quantity: 1, singleton: true, online: true, attributes: &[] },
        ]).await.unwrap();
        for (skill, level) in crate::services::skills::requirements::requirements(&sde, module_type) {
            let rank = sde.types.get(skill).and_then(|t| t.fit_attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT)).expect("module skill rank");
            let points = crate::services::skills::formulas::sp_for_level(rank, i64::from(level));
            crate::dao::skills::training::set_level(&db, f.character, skill, level, points).await.unwrap();
        }
        let slot = crate::services::effects::fitting::free_slot(&db, sde.clone(), f.character, f.ship, f.module(), module_type).await.ok().flatten();
        if let Some(flag) = slot {
            let item = load_items(&db, &sde, ItemQuery::Id(f.module())).await.unwrap().pop().unwrap();
            assert!(crate::dao::items::inventory::move_item(&db, &item, f.ship, flag, f.character, true).await.unwrap());
            if crate::services::effects::fitting::validate_fit(&db, sde.clone(), f.character, f.ship, f.module()).await.is_ok() {
                selected = Some((f, flag));
                break;
            }
        }
        cleanup_ship(&db, None, f).await.unwrap();
    }
    let (mut fixture, flag) = selected.expect("legal groupable module and published ship");
    let module = fixture.module();
    let station_data = sde.map.stations().find(|station| station.solar_system_id.is_some()).expect("station with system");
    let station = station_data.station_id;
    let system = station_data.solar_system_id.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        test_db.execute("UPDATE game.items SET location_id=$2, flag_id=$3 WHERE item_id=$1", &[&fixture.ship, &station, &FLAG_HANGAR]).await.unwrap();
        test_db.execute("UPDATE game.characters SET station_id=$2, solar_system_id=$3 WHERE character_id=$1", &[&fixture.character, &station, &system]).await.unwrap();
        crate::dao::characters::selection::set_ship(&test_db, fixture.character, fixture.ship).await.unwrap();
        let source = crate::dao::items::transfers::insert(&test_db, charge_type, fixture.character, station, FLAG_HANGAR, 100_000).await.unwrap();
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), fixture.character).await;
        client.session.apply(vec![("shipid", Value::Int(fixture.ship)), ("stationid", Value::Int(station))]);
        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = sender;
        let args = [Value::Int(fixture.ship), Value::Int(module), Value::list([Value::Int(source)]), Value::Int(station)];
        let kwargs = Value::None;
        let call = Call { service: "dogmaIM", method: "LoadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
        let (_, quantity) = loaded(&test_db, fixture.ship, flag).await.unwrap().expect("loaded charge");
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let primes: Vec<_> = events.iter().filter(|(name, _)| name == "OnGodmaPrimeItem").collect();
        assert_eq!(primes.len(), 1);
        assert_prime(&primes[0].1, &sde, fixture.ship, flag, charge_type, quantity);
        assert!(!events.iter().any(|(name, _)| name == "OnGodmaPrimeLocation"));
        assert!(!events.iter().any(|(name, args)| name == "OnModuleAttributeChanges" && args[0].items().is_some_and(|rows|
            rows.iter().any(|row| row.items().is_some_and(|fields| fields.get(2) == Some(&charge_key(fixture.ship, flag, charge_type)) && fields.get(5).and_then(Value::as_int).is_some_and(|value| value > 0))))));
        let module_data = sde.types.get(module_type).unwrap();
        let groups: Vec<_> = CHARGE_GROUP_ATTRIBUTES.iter().flat_map(|name| sde.dogma.attributes_by_name(name))
            .filter_map(|attr| module_data.attribute(attr.attribute_id)).collect();
        let size = sde.dogma.attributes_by_name("chargeSize").next().map(|attr| attr.attribute_id);
        let replacement = sde.types.iter().find(|candidate| candidate.type_id != charge_type && candidate.published == Some(true)
            && candidate.category_id == Some(CATEGORY_CHARGE) && candidate.volume.is_some_and(|v| v > 0.0 && v <= module_data.capacity.unwrap_or(0.0).max(module_data.attribute(ATTR_CAPACITY).unwrap_or(0.0)))
            && charge_matches(groups.iter().copied(), candidate.group_id, size.and_then(|id| module_data.attribute(id)), size.and_then(|id| candidate.attribute(id))))
            .expect("second compatible published charge").type_id;
        let replacement_source = crate::dao::items::transfers::insert(&test_db, replacement, fixture.character, station, FLAG_HANGAR, quantity).await.unwrap();
        let args = [Value::Int(fixture.ship), Value::Int(module), Value::list([Value::Int(replacement_source)]), Value::Int(station)];
        let call = Call { service: "dogmaIM", method: "LoadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
        let (_, replacement_quantity) = loaded(&test_db, fixture.ship, flag).await.unwrap().expect("replacement charge");
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let zero = events.iter().position(|(name, args)| name == "OnModuleAttributeChanges" && args[0].items().is_some_and(|rows|
            rows.iter().any(|row| row.items().is_some_and(|fields| fields.get(2) == Some(&charge_key(fixture.ship, flag, charge_type)) && fields.get(5).and_then(Value::as_int) == Some(0))))).expect("old key zero");
        let prime = events.iter().position(|(name, _)| name == "OnGodmaPrimeItem").expect("replacement prime");
        assert!(zero < prime);
        assert_prime(&events[prime].1, &sde, fixture.ship, flag, replacement, replacement_quantity);
        unload_ammo_core(&mut client, fixture.ship, vec![module], station, FLAG_HANGAR).await.unwrap();
        assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), None);
        let total: String = test_db.query_one("SELECT COALESCE(SUM(quantity),0)::text AS quantity FROM game.items WHERE owner_id=$1 AND type_id=$2 AND location_id=ANY($3)", &[&fixture.character, &replacement, &&[station, fixture.ship][..]]).await.unwrap().get("quantity");
        assert_eq!(total.parse::<i128>().unwrap(), i128::from(quantity));
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|(name, args)| name == "OnModuleAttributeChanges" && args[0].items().is_some_and(|rows|
            rows.iter().any(|row| row.items().is_some_and(|fields| fields.get(2) == Some(&charge_key(fixture.ship, flag, replacement)) && fields.get(5).and_then(Value::as_int) == Some(0))))));
        let args = [Value::Int(fixture.ship), Value::Int(module), Value::list([Value::Int(source)]), Value::Int(station)];
        let call = Call { service: "dogmaIM", method: "LoadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
        let (_, refilled) = loaded(&test_db, fixture.ship, flag).await.unwrap().expect("refilled charge");
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let prime = events.iter().find(|(name, _)| name == "OnGodmaPrimeItem").expect("refill prime");
        assert_prime(&prime.1, &sde, fixture.ship, flag, charge_type, refilled);
        unload_ammo_core(&mut client, fixture.ship, vec![module], station, FLAG_HANGAR).await.unwrap();
        while rx.try_recv().is_ok() {}
        assert_eq!(gm_load_ammo(&mut client, fixture.ship, vec![module], station, charge_type, quantity).await.unwrap(), 1);
        let (_, actual) = loaded(&test_db, fixture.ship, flag).await.unwrap().expect("generated charge");
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let primes: Vec<_> = events.iter().filter(|(name, _)| name == "OnGodmaPrimeItem").collect();
        assert_eq!(primes.len(), 1);
        assert_prime(&primes[0].1, &sde, fixture.ship, flag, charge_type, actual);
        unload_ammo_core(&mut client, fixture.ship, vec![module], station, FLAG_HANGAR).await.unwrap();
        while rx.try_recv().is_ok() {}
        let second = crate::dao::items::creation::rookie_ship(&test_db, module_type, fixture.character, fixture.ship, FLAG_CARGO).await.unwrap();
        let second_flag = crate::services::effects::fitting::free_slot(&test_db, sde.clone(), fixture.character, fixture.ship, second, module_type).await.unwrap().expect("second legal slot");
        let item = load_items(&test_db, &sde, ItemQuery::Id(second)).await.unwrap().pop().unwrap();
        assert!(crate::dao::items::inventory::move_item(&test_db, &item, fixture.ship, second_flag, fixture.character, true).await.unwrap());
        crate::services::effects::fitting::validate_fit(&test_db, sde.clone(), fixture.character, fixture.ship, second).await.unwrap();
        test_db.execute("UPDATE game.items SET online=true WHERE item_id=$1", &[&second]).await.unwrap();
        let link_args = [Value::Int(fixture.ship), Value::Int(module), Value::Int(second)];
        let link = Call { service: "dogmaIM", method: "LinkWeapons", args: &link_args, kwargs: &kwargs, bound: true, bind_params: None };
        crate::services::dispatch(&mut client, &link).await.unwrap();
        assert_eq!(bank(&test_db, fixture.ship, module).await.unwrap(), vec![module, second]);
        let bank_source = crate::dao::items::transfers::insert(&test_db, charge_type, fixture.character, station, FLAG_HANGAR, quantity * 2).await.unwrap();
        let args = [Value::Int(fixture.ship), Value::Int(module), Value::list([Value::Int(bank_source)]), Value::Int(station)];
        let call = Call { service: "dogmaIM", method: "LoadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let primes: Vec<_> = events.iter().filter(|(name, _)| name == "OnGodmaPrimeItem").collect();
        assert_eq!(primes.len(), 2);
        for member_flag in [flag, second_flag] {
            let (_, actual) = loaded(&test_db, fixture.ship, member_flag).await.unwrap().expect("bank charge");
            let prime = primes.iter().find(|(_, args)| {
                let Value::Instance(_, info) = &args[1] else { return false };
                info.get("itemID") == Some(&charge_key(fixture.ship, member_flag, charge_type))
            }).expect("bank member prime");
            assert_prime(&prime.1, &sde, fixture.ship, member_flag, charge_type, actual);
        }
    }).await;
    db.execute("DELETE FROM game.pending_reloads WHERE ship_id=$1", &[&fixture.ship]).await.unwrap();
    crate::dao::items::combat::clear_banks(&db, fixture.ship).await.unwrap();
    fixture.items.extend(db.query("SELECT item_id FROM game.items WHERE owner_id = $1", &[&fixture.character]).await.unwrap().iter().map(|row| row.get::<_, i64>("item_id")));
    cleanup_ship(&db, None, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn native_space_reload_primes_only_after_deadline() {
    let (mut fixture, _, module, charge_type, source) = shared_reload_fixture().await;
    fixture.set(module, ATTR_RELOAD_TIME, 2_000.0).await;
    let fit = Fit::load(&fixture.db, fixture.sde.clone(), fixture.owner, fixture.ship).await.unwrap();
    let condition = crate::services::combat::prepare_condition_with_fit(&fixture.db, fixture.ship, &fit).await.unwrap();
    let ship = fixture.ship;
    fixture.world.with(fixture.system, move |s| s.install_condition(ship, condition.into_condition(), fit)).await.unwrap();
    let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
    fixture.client.notify_tx = sender.clone();
    fixture.world.register_pilot(fixture.owner, sender);
    let fixture = Arc::new(tokio::sync::Mutex::new(fixture));
    let test_fixture = fixture.clone();
    let result = tokio::spawn(async move {
        let mut f = test_fixture.lock().await;
        let args = [Value::Int(f.ship), Value::Int(module), Value::list([Value::Int(source)]), Value::Int(f.ship)];
        let kwargs = Value::None;
        let call = Call { service: "dogmaIM", method: "LoadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut f.client, &call).await.unwrap(), Value::None);
        let marker = *state().reloading.get(&module).expect("reload marker");
        let rows = crate::dao::items::pending_reloads::for_ship(&f.db, f.ship).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(f.client.outbox.iter().any(|packet| matches!(&packet.destination,
            infinity_wire::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnChargeBeingLoadedToModule"))));
        let timer = f.client.outbox.iter().find(|packet| matches!(&packet.destination,
            infinity_wire::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnModuleAttributeChanges"))).expect("reload timer");
        let envelope = timer.body[0].items().unwrap();
        let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
        let args = payload.items().unwrap()[1].items().unwrap();
        let row = &args[0].items().unwrap()[0];
        let fields = row.items().unwrap();
        assert_eq!(fields[2].as_int(), Some(module));
        assert_eq!(fields[3].as_int(), Some(i64::from(ATTR_NEXT_ACTIVATION_TIME)));
        assert_eq!(fields[5].as_int(), Some(marker));
        while let Ok((name, _)) = rx.try_recv() { assert_ne!(name, "OnGodmaPrimeItem"); }
        let prime = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let (name, args) = rx.recv().await.expect("reload notification");
                if name == "OnGodmaPrimeItem" { break args; }
            }
        }).await.expect("actor deadline");
        assert_prime(&prime, &f.sde, f.ship, rows[0].flag_id, charge_type, rows[0].quantity);
        let Value::Instance(_, info) = &prime[1] else { panic!("prime info") };
        assert!(info.get("time").and_then(Value::as_int).unwrap() >= marker);
    }).await;
    Arc::try_unwrap(fixture).ok().expect("fixture ownership").into_inner().cleanup().await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn real_script_changes_stored_fitting_on_insert_and_remove() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let size = sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
    let mut selected = None;
    'search: for module in sde.types.iter().filter(|m| m.published == Some(true)
        && m.category_id == Some(infinity_core::consts::CATEGORY_MODULE) && sde.is_tradable(m.type_id)) {
        let groups: Vec<_> = CHARGE_GROUP_ATTRIBUTES.iter().flat_map(|name| sde.dogma.attributes_by_name(name))
            .filter_map(|a| module.attribute(a.attribute_id)).collect();
        let capacity = module.capacity.unwrap_or(0.0).max(module.attribute(ATTR_CAPACITY).unwrap_or(0.0));
        let charges: Vec<_> = sde.types.iter().filter(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE)
            && sde.is_tradable(c.type_id) && c.volume.is_some_and(|v| v > 0.0 && capacity >= v)
            && charge_matches(groups.iter().copied(), c.group_id, size.and_then(|id| module.attribute(id)), size.and_then(|id| c.attribute(id)))
            && c.effects().iter().filter_map(|entry| sde.dogma.effect(entry.effect_id)).flat_map(|effect| effect.modifiers())
                .any(|modifier| modifier.func == "ItemModifier" && modifier.domain == "otherID"))
            .map(|c| c.type_id).collect();
        if charges.is_empty() { continue; }
        for hull in sde.types.iter().filter(|h| h.published == Some(true) && h.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
            && h.capacity.unwrap_or(0.0).max(h.attribute(ATTR_CAPACITY).unwrap_or(0.0)) >= capacity) {
            let f = create_ship(&db, hull.type_id, ShipLocation::Docked { station: 1 }, &[
                ShipItemSpec { type_id: module.type_id, flag_id: FLAG_CARGO, quantity: 1, singleton: true, online: true, attributes: &[] },
            ]).await.unwrap();
            for (skill, level) in crate::services::skills::requirements::requirements(&sde, module.type_id) {
                let rank = sde.types.get(skill).and_then(|t| t.fit_attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT)).expect("module skill rank");
                let points = crate::services::skills::formulas::sp_for_level(rank, i64::from(level));
                crate::dao::skills::training::set_level(&db, f.character, skill, level, points).await.unwrap();
            }
            let slot = crate::services::effects::fitting::free_slot(&db, sde.clone(), f.character, f.ship, f.module(), module.type_id).await.ok().flatten();
            let Some(flag) = slot else { cleanup_ship(&db, None, f).await.unwrap(); continue };
            let item = load_items(&db, &sde, ItemQuery::Id(f.module())).await.unwrap().pop().unwrap();
            assert!(crate::dao::items::inventory::move_item(&db, &item, f.ship, flag, f.character, true).await.unwrap());
            if crate::services::effects::fitting::validate_fit(&db, sde.clone(), f.character, f.ship, f.module()).await.is_err() {
                cleanup_ship(&db, None, f).await.unwrap(); continue;
            }
            let before = Fit::load(&db, sde.clone(), f.character, f.ship).await.unwrap();
            let mut variants = Vec::new();
            for &charge in &charges {
                crate::dao::items::combat::apply_loads(&db, f.ship, &[(f.module(), flag, charge, 1, 0)]).await.unwrap();
                let after = Fit::load(&db, sde.clone(), f.character, f.ship).await.unwrap();
                let changes = after.fitting_changes(&before.fitting_values(&before.fitting_candidates(&after)));
                crate::dao::items::combat::delete_charge(&db, f.ship, flag).await.unwrap();
                variants.extend(changes.into_iter().filter_map(|(key, attr, old, new)|
                    (key == Key::Module(f.module())).then_some((charge, attr, old, new))));
            }
            for &(charge, attr, old, new) in &variants {
                if let Some(&(replacement, _, _, replacement_value)) = variants.iter().find(|&&(other, a, _, value)| other != charge && a == attr && value != new) {
                    selected = Some((f, flag, charge, replacement, attr, old, new, replacement_value));
                    break 'search;
                }
            }
            cleanup_ship(&db, None, f).await.unwrap();
        }
    }
    let (mut fixture, flag, charge, replacement, attr, old, new, replacement_value) = selected.expect("published compatible charge pair with effective otherID module modifier");
    let module = fixture.module();
    let station_data = sde.map.stations().find(|station| station.solar_system_id.is_some()).expect("station with system");
    let station = station_data.station_id;
    let system = station_data.solar_system_id.unwrap();
    let test_db = db.clone();
    let test_sde = sde.clone();
    let (mut client, peer) = crate::services::tests::client(test_db.clone(), fixture.character).await;
    let world = client.world.clone();
    let result = tokio::spawn(async move {
        test_db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&fixture.ship, &station, &FLAG_HANGAR]).await.unwrap();
        test_db.execute("UPDATE game.characters SET station_id=$2, solar_system_id=$3 WHERE character_id=$1", &[&fixture.character, &station, &system]).await.unwrap();
        crate::dao::characters::selection::set_ship(&test_db, fixture.character, fixture.ship).await.unwrap();
        let _peer = peer;
        client.session.apply(vec![("shipid", Value::Int(fixture.ship)), ("stationid", Value::Int(station))]);
        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = sender.clone();
        let source = crate::dao::items::transfers::insert(&test_db, charge, fixture.character, station, FLAG_HANGAR, 1).await.unwrap();
        assert_eq!(load_ammo_core(&mut client, fixture.ship, vec![module], vec![source], Some(station)).await.unwrap(), Value::None);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let prime = events.iter().find(|(name, _)| name == "OnGodmaPrimeItem").expect("script prime");
        assert_prime(&prime.1, &test_sde, fixture.ship, flag, charge, 1);
        assert!(events.iter().any(|(name, args)| name == "OnModuleAttributeChanges" && args[0].items().is_some_and(|rows|
            rows.iter().any(|row| row.items().is_some_and(|fields| fields.get(2) == Some(&Value::Int(module))
                && fields.get(3).and_then(Value::as_int) == Some(i64::from(attr))
                && fields.get(5).and_then(Value::as_float) == Some(new) && fields.get(6).and_then(Value::as_float) == Some(old))))));
        let source = crate::dao::items::transfers::insert(&test_db, replacement, fixture.character, station, FLAG_HANGAR, 1).await.unwrap();
        assert_eq!(load_ammo_core(&mut client, fixture.ship, vec![module], vec![source], Some(station)).await.unwrap(), Value::None);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let prime = events.iter().find(|(name, _)| name == "OnGodmaPrimeItem").expect("replacement script prime");
        assert_prime(&prime.1, &test_sde, fixture.ship, flag, replacement, 1);
        let final_fit = Fit::load(&test_db, test_sde.clone(), fixture.character, fixture.ship).await.unwrap();
        assert_eq!(final_fit.value(Key::Module(module), attr), replacement_value);
        assert!(events.iter().any(|(name, args)| name == "OnModuleAttributeChanges" && args[0].items().is_some_and(|rows|
            rows.iter().any(|row| row.items().is_some_and(|fields| fields.get(2) == Some(&Value::Int(module))
                && fields.get(3).and_then(Value::as_int) == Some(i64::from(attr))
                && fields.get(5).and_then(Value::as_float) == Some(replacement_value) && fields.get(6).and_then(Value::as_float) == Some(old))))));
        unload_ammo_core(&mut client, fixture.ship, vec![module], station, FLAG_HANGAR).await.unwrap();
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|(name, args)| name == "OnModuleAttributeChanges" && args[0].items().is_some_and(|rows|
            rows.iter().any(|row| row.items().is_some_and(|fields| fields.get(2) == Some(&Value::Int(module))
                && fields.get(3).and_then(Value::as_int) == Some(i64::from(attr))
                && fields.get(5).and_then(Value::as_float) == Some(old) && fields.get(6).and_then(Value::as_float) == Some(replacement_value))))));
        test_db.execute("UPDATE game.items SET location_id=$2,flag_id=0,x=0,y=0,z=0 WHERE item_id=$1", &[&fixture.ship, &system]).await.unwrap();
        crate::dao::characters::ship::undock(&test_db, fixture.character, system).await.unwrap();
        crate::dao::characters::selection::set_ship(&test_db, fixture.character, fixture.ship).await.unwrap();
        crate::dao::items::dogma::set_override(&test_db, module, ATTR_RELOAD_TIME, 2_000.0).await.unwrap();
        client.session.apply(vec![("stationid", Value::None), ("solarsystemid", Value::Int(system))]);
        client.world.register_pilot(fixture.character, sender);
        let hull = load_items(&test_db, &test_sde, ItemQuery::Id(fixture.ship)).await.unwrap().pop().unwrap();
        let ball = crate::services::effects::tests::fixture::ball(fixture.ship, hull.type_id, fixture.character, Some(fixture.character), 0.0);
        crate::services::effects::tests::fixture::admit(&test_db, &client.world, system, ball).await;
        let ship = fixture.ship;
        let revision = client.world.with(system, move |s| s.combat_fit_revision(ship).unwrap()).await.unwrap();
        let source = crate::dao::items::transfers::insert(&test_db, charge, fixture.character, ship, FLAG_CARGO, 1).await.unwrap();
        assert_eq!(load_ammo_core(&mut client, ship, vec![module], vec![source], Some(ship)).await.unwrap(), Value::None);
        let marker = *state().reloading.get(&module).expect("script reload marker");
        let pending_rows = crate::dao::items::pending_reloads::for_ship(&test_db, ship).await.unwrap();
        assert_eq!(pending_rows.len(), 1);
        let pending = PendingLoad { owner: fixture.character, reload_ids: vec![pending_rows[0].reload_id],
            external_sources: vec![], external_facts: Vec::new(), load: (module, flag, charge, pending_rows[0].quantity, 0) };
        while let Ok((name, _)) = rx.try_recv() { assert_ne!(name, "OnGodmaPrimeItem"); }
        assert_eq!(client.world.with(system, move |s| s.combat_fit_revision(ship).unwrap()).await.unwrap(), revision);
        let (prime, changed) = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let mut prime = None;
            let mut changed = false;
            while prime.is_none() || !changed {
                let (name, args) = rx.recv().await.expect("script actor notification");
                if name == "OnGodmaPrimeItem" { prime = Some(args); }
                else if name == "OnModuleAttributeChanges" {
                    changed |= args[0].items().is_some_and(|rows| rows.iter().any(|row| row.items().is_some_and(|fields|
                        fields.get(2) == Some(&Value::Int(module)) && fields.get(3).and_then(Value::as_int) == Some(i64::from(attr))
                        && fields.get(5).and_then(Value::as_float) == Some(new) && fields.get(6).and_then(Value::as_float) == Some(old))));
                }
            }
            (prime.unwrap(), changed)
        }).await.expect("actor script publication");
        assert!(changed);
        assert_prime(&prime, &test_sde, ship, flag, charge, 1);
        let Value::Instance(_, info) = &prime[1] else { panic!("prime info") };
        assert!(info.get("time").and_then(Value::as_int).unwrap() >= marker);
        let final_fit = Fit::load(&test_db, test_sde.clone(), fixture.character, ship).await.unwrap();
        assert_eq!(final_fit.value(Key::Module(module), attr), new);
        assert!(!final_fit.overrides(Key::Charge(module)).iter().any(|(id, _)| *id == infinity_core::consts::dogma::ATTR_QUANTITY));
        let completed_revision = client.world.with(system, move |s| s.combat_fit_revision(ship)).await.unwrap().expect("completed actor fit");
        assert!(completed_revision > revision);
        while let Ok((name, args)) = rx.try_recv() {
            assert_eq!(name, "OnModuleAttributeChanges", "unexpected first-completion notification");
            assert!(!args[0].items().unwrap().iter().any(|row| row.items().is_some_and(|fields|
                fields.get(2) == Some(&charge_key(ship, flag, charge))
                    && fields.get(3).and_then(Value::as_int) == Some(i64::from(infinity_core::consts::dogma::ATTR_QUANTITY)))));
        }
        let source_before = load_items(&test_db, &test_sde, ItemQuery::Id(source)).await.unwrap().pop().map(|item| item.quantity);
        let loaded_before = loaded(&test_db, ship, flag).await.unwrap();
        assert_eq!(loaded_before, Some((charge, 1)));
        assert!(crate::dao::items::pending_reloads::for_ship(&test_db, ship).await.unwrap().is_empty());
        apply_loads(&test_db, &client.world, &client.notify_tx, ship, vec![pending]).await.unwrap();
        assert_eq!(client.world.with(system, move |s| s.combat_fit_revision(ship)).await.unwrap(), Some(completed_revision));
        assert_eq!(loaded(&test_db, ship, flag).await.unwrap(), loaded_before);
        assert_eq!(load_items(&test_db, &test_sde, ItemQuery::Id(source)).await.unwrap().pop().map(|item| item.quantity), source_before);
        assert!(crate::dao::items::pending_reloads::for_ship(&test_db, ship).await.unwrap().is_empty());
        assert!(rx.try_recv().is_err(), "duplicate callback emitted a notification");
    }).await;
    world.remove_ball(system, fixture.ship).await;
    world.unregister_pilot_generation(fixture.character, None);
    state().reloading.remove(&module);
    super::super::super::forget_ship(fixture.ship);
    db.execute("DELETE FROM game.pending_reloads WHERE ship_id=$1", &[&fixture.ship]).await.unwrap();
    fixture.items.extend(db.query("SELECT item_id FROM game.items WHERE owner_id = $1", &[&fixture.character]).await.unwrap().iter().map(|row| row.get::<_, i64>("item_id")));
    cleanup_ship(&db, None, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn resident_completion_settles_actor_dirty_then_publishes_absolute() {
    let mut stage = recount_stage().await;
    let result = crate::tests::catch_scenario(async {
        load_stack(&mut stage, 8).await;
        let marker = *state().reloading.get(&stage.module).expect("first reload marker");
        drive_timer(&mut stage, marker).await;
        wait_marker(stage.module, marker).await;
        let (system, ship, module, flag, kind) = (stage.fixture.system, stage.fixture.ship, stage.module, stage.flag, stage.charge_type);
        assert_eq!(loaded(&stage.fixture.db, ship, flag).await.unwrap(), Some((kind, 8)));
        stage.fixture.world.with(system, move |s| {
            let permit = s.hold_loaded_use(ship, module).expect("charged use admitted");
            assert_eq!(s.consume_loaded(permit), Some((kind, 7)));
        }).await.unwrap();
        assert_eq!(loaded(&stage.fixture.db, ship, flag).await.unwrap(), Some((kind, 8)),
            "durable still holds the value before the actor's dirty consumption");
        load_stack(&mut stage, 3).await;
        let marker = *state().reloading.get(&stage.module).expect("second reload marker");
        drive_timer(&mut stage, marker).await;
        wait_marker(stage.module, marker).await;
        assert_eq!(loaded(&stage.fixture.db, ship, flag).await.unwrap(), Some((kind, 10)),
            "completion settled the actor absolute before adding the reservation");
        assert!(crate::dao::items::pending_reloads::for_ship(&stage.fixture.db, ship).await.unwrap().is_empty());
        let effective = stage.fixture.world.with(system, move |s| s.loaded_effective(ship, module)).await.flatten();
        assert_eq!(effective.map(|view| (view.1, view.2)), Some((kind, 10)));
    }).await;
    cleanup_recount(stage).await;
    result.unwrap();
}
