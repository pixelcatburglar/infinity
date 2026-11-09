//! Charge completion wire shape and reservation regressions against real SDE rows.
use super::*;

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
    let fixture = crate::services::effects::tests::fixture::create(&db, ship_type, module_type, flag).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let (notify, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let insert = |amount, source| crate::dao::items::pending_reloads::insert(&test_db, fixture.character, fixture.ship, fixture.module, flag, charge_type, amount, source, fixture.ship);
        let cargo = insert(2, FLAG_CARGO).await.unwrap();
        let ammo = insert(3, FLAG_SPECIALIZED_AMMO_HOLD).await.unwrap();
        let pending = PendingLoad { owner: fixture.character, reload_ids: vec![cargo, ammo], external_sources: vec![], external_facts: Vec::new(), load: (fixture.module, flag, charge_type, 5, 0) };
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
        apply_loads(&test_db, &world, &notify, fixture.ship, vec![PendingLoad { owner: fixture.character, reload_ids: vec![cargo, ammo], external_sources: vec![], external_facts: Vec::new(), load: (fixture.module, flag, charge_type, 10, 5) }]).await.unwrap();
        assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), Some((charge_type, 8)));
        let (name, args) = rx.try_recv().unwrap();
        assert_eq!(name, "OnModuleAttributeChanges");
        let row = &args[0].items().unwrap()[0];
        let fields = row.items().unwrap();
        assert_eq!(fields[2], charge_key(fixture.ship, flag, charge_type));
        assert_eq!(fields[3].as_int(), Some(i64::from(infinity_core::consts::dogma::ATTR_QUANTITY)));
        assert_eq!(fields[5].as_int(), Some(8));
        assert_eq!(fields[6].as_int(), Some(5));
        assert!(rx.try_recv().is_err());
        crate::dao::items::combat::delete_charge(&test_db, fixture.ship, flag).await.unwrap();
        let cargo = insert(2, FLAG_CARGO).await.unwrap();
        let ammo = insert(3, FLAG_SPECIALIZED_AMMO_HOLD).await.unwrap();
        crate::dao::items::pending_reloads::take(&test_db, cargo).await.unwrap();
        apply_loads(&test_db, &world, &notify, fixture.ship, vec![PendingLoad { owner: fixture.character, reload_ids: vec![cargo, ammo], external_sources: vec![], external_facts: Vec::new(), load: (fixture.module, flag, charge_type, 5, 0) }]).await.unwrap();
        assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), Some((charge_type, 3)));
        let (name, args) = rx.try_recv().unwrap();
        assert_eq!(name, "OnGodmaPrimeItem");
        assert_prime(&args, &sde, fixture.ship, flag, charge_type, 3);
        crate::dao::items::combat::delete_charge(&test_db, fixture.ship, flag).await.unwrap();
        while rx.try_recv().is_ok() {}
        let station = sde.map.stations().next().unwrap().station_id;
        let native = insert(2, FLAG_CARGO).await.unwrap();
        let static_id = crate::dao::items::pending_reloads::insert(&test_db, fixture.character, fixture.ship, fixture.module, flag, charge_type, 3, infinity_core::consts::FLAG_HANGAR, station).await.unwrap();
        crate::dao::items::pending_reloads::take(&test_db, native).await.unwrap();
        apply_loads(&test_db, &world, &notify, fixture.ship, vec![PendingLoad { owner: fixture.character, reload_ids: vec![native, static_id], external_sources: vec![], external_facts: Vec::new(), load: (fixture.module, flag, charge_type, 5, 0) }]).await.unwrap();
        assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), Some((charge_type, 3)));
        let (name, args) = rx.try_recv().unwrap();
        assert_eq!(name, "OnGodmaPrimeItem");
        assert_prime(&args, &sde, fixture.ship, flag, charge_type, 3);
        let (name, _) = rx.try_recv().unwrap();
        assert_eq!(name, "OnModuleAttributeChanges");
        assert!(rx.try_recv().is_err());
    }).await;
    db.execute("DELETE FROM game.pending_reloads WHERE ship_id = $1", &[&fixture.ship]).await.unwrap();
    crate::services::effects::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
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
        let f = crate::services::effects::tests::fixture::create(&db, hull.type_id, module_type, FLAG_CARGO).await.unwrap();
        for (skill, level) in crate::services::skills::requirements::requirements(&sde, module_type) {
            let rank = sde.types.get(skill).and_then(|t| t.fit_attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT)).expect("module skill rank");
            let points = crate::services::skills::formulas::sp_for_level(rank, i64::from(level));
            crate::dao::skills::training::set_level(&db, f.character, skill, level, points).await.unwrap();
        }
        let slot = crate::services::effects::fitting::free_slot(&db, sde.clone(), f.character, f.ship, f.module, module_type).await.ok().flatten();
        if let Some(flag) = slot {
            let item = load_items(&db, &sde, ItemQuery::Id(f.module)).await.unwrap().pop().unwrap();
            assert!(crate::dao::items::inventory::move_item(&db, &item, f.ship, flag, f.character, true).await.unwrap());
            if crate::services::effects::fitting::validate_fit(&db, sde.clone(), f.character, f.ship, f.module).await.is_ok() {
                selected = Some((f, flag));
                break;
            }
        }
        crate::services::effects::tests::fixture::cleanup(&db, f).await.unwrap();
    }
    let (fixture, flag) = selected.expect("legal groupable module and published ship");
    let station_data = sde.map.stations().find(|station| station.solar_system_id.is_some()).expect("station with system");
    let station = station_data.station_id;
    let system = station_data.solar_system_id.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        test_db.execute("UPDATE game.items SET location_id=$2, flag_id=$3 WHERE item_id=$1", &[&fixture.ship, &station, &FLAG_HANGAR]).await.unwrap();
        crate::dao::characters::ship::dock(&test_db, fixture.character, station, system).await.unwrap();
        crate::dao::characters::selection::set_ship(&test_db, fixture.character, fixture.ship).await.unwrap();
        let source = crate::dao::items::transfers::insert(&test_db, charge_type, fixture.character, station, FLAG_HANGAR, 100_000).await.unwrap();
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), fixture.character).await;
        client.session.apply(vec![("shipid", Value::Int(fixture.ship)), ("stationid", Value::Int(station))]);
        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = sender;
        let args = [Value::Int(fixture.ship), Value::Int(fixture.module), Value::list([Value::Int(source)]), Value::Int(station)];
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
        let args = [Value::Int(fixture.ship), Value::Int(fixture.module), Value::list([Value::Int(replacement_source)]), Value::Int(station)];
        let call = Call { service: "dogmaIM", method: "LoadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
        let (_, replacement_quantity) = loaded(&test_db, fixture.ship, flag).await.unwrap().expect("replacement charge");
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let zero = events.iter().position(|(name, args)| name == "OnModuleAttributeChanges" && args[0].items().is_some_and(|rows|
            rows.iter().any(|row| row.items().is_some_and(|fields| fields.get(2) == Some(&charge_key(fixture.ship, flag, charge_type)) && fields.get(5).and_then(Value::as_int) == Some(0))))).expect("old key zero");
        let prime = events.iter().position(|(name, _)| name == "OnGodmaPrimeItem").expect("replacement prime");
        assert!(zero < prime);
        assert_prime(&events[prime].1, &sde, fixture.ship, flag, replacement, replacement_quantity);
        unload_ammo_core(&mut client, fixture.ship, vec![fixture.module], station, FLAG_HANGAR).await.unwrap();
        assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), None);
        let total: String = test_db.query_one("SELECT COALESCE(SUM(quantity),0)::text AS quantity FROM game.items WHERE owner_id=$1 AND type_id=$2 AND location_id=ANY($3)", &[&fixture.character, &replacement, &&[station, fixture.ship][..]]).await.unwrap().get("quantity");
        assert_eq!(total.parse::<i128>().unwrap(), i128::from(quantity));
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|(name, args)| name == "OnModuleAttributeChanges" && args[0].items().is_some_and(|rows|
            rows.iter().any(|row| row.items().is_some_and(|fields| fields.get(2) == Some(&charge_key(fixture.ship, flag, replacement)) && fields.get(5).and_then(Value::as_int) == Some(0))))));
        let args = [Value::Int(fixture.ship), Value::Int(fixture.module), Value::list([Value::Int(source)]), Value::Int(station)];
        let call = Call { service: "dogmaIM", method: "LoadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
        let (_, refilled) = loaded(&test_db, fixture.ship, flag).await.unwrap().expect("refilled charge");
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let prime = events.iter().find(|(name, _)| name == "OnGodmaPrimeItem").expect("refill prime");
        assert_prime(&prime.1, &sde, fixture.ship, flag, charge_type, refilled);
        unload_ammo_core(&mut client, fixture.ship, vec![fixture.module], station, FLAG_HANGAR).await.unwrap();
        while rx.try_recv().is_ok() {}
        assert_eq!(gm_load_ammo(&mut client, fixture.ship, vec![fixture.module], station, charge_type, quantity).await.unwrap(), 1);
        let (_, actual) = loaded(&test_db, fixture.ship, flag).await.unwrap().expect("generated charge");
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let primes: Vec<_> = events.iter().filter(|(name, _)| name == "OnGodmaPrimeItem").collect();
        assert_eq!(primes.len(), 1);
        assert_prime(&primes[0].1, &sde, fixture.ship, flag, charge_type, actual);
        unload_ammo_core(&mut client, fixture.ship, vec![fixture.module], station, FLAG_HANGAR).await.unwrap();
        while rx.try_recv().is_ok() {}
        let second = crate::dao::items::creation::rookie_ship(&test_db, module_type, fixture.character, fixture.ship, FLAG_CARGO).await.unwrap();
        let second_flag = crate::services::effects::fitting::free_slot(&test_db, sde.clone(), fixture.character, fixture.ship, second, module_type).await.unwrap().expect("second legal slot");
        let item = load_items(&test_db, &sde, ItemQuery::Id(second)).await.unwrap().pop().unwrap();
        assert!(crate::dao::items::inventory::move_item(&test_db, &item, fixture.ship, second_flag, fixture.character, true).await.unwrap());
        crate::services::effects::fitting::validate_fit(&test_db, sde.clone(), fixture.character, fixture.ship, second).await.unwrap();
        test_db.execute("UPDATE game.items SET online=true WHERE item_id=$1", &[&second]).await.unwrap();
        let link_args = [Value::Int(fixture.ship), Value::Int(fixture.module), Value::Int(second)];
        let link = Call { service: "dogmaIM", method: "LinkWeapons", args: &link_args, kwargs: &kwargs, bound: true, bind_params: None };
        crate::services::dispatch(&mut client, &link).await.unwrap();
        assert_eq!(bank(&test_db, fixture.ship, fixture.module).await.unwrap(), vec![fixture.module, second]);
        let bank_source = crate::dao::items::transfers::insert(&test_db, charge_type, fixture.character, station, FLAG_HANGAR, quantity * 2).await.unwrap();
        let args = [Value::Int(fixture.ship), Value::Int(fixture.module), Value::list([Value::Int(bank_source)]), Value::Int(station)];
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
    crate::services::effects::tests::fixture::cleanup(&db, fixture).await.unwrap();
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
            crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnChargeBeingLoadedToModule"))));
        let timer = f.client.outbox.iter().find(|packet| matches!(&packet.destination,
            crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnModuleAttributeChanges"))).expect("reload timer");
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
            let f = crate::services::effects::tests::fixture::create(&db, hull.type_id, module.type_id, FLAG_CARGO).await.unwrap();
            for (skill, level) in crate::services::skills::requirements::requirements(&sde, module.type_id) {
                let rank = sde.types.get(skill).and_then(|t| t.fit_attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT)).expect("module skill rank");
                let points = crate::services::skills::formulas::sp_for_level(rank, i64::from(level));
                crate::dao::skills::training::set_level(&db, f.character, skill, level, points).await.unwrap();
            }
            let slot = crate::services::effects::fitting::free_slot(&db, sde.clone(), f.character, f.ship, f.module, module.type_id).await.ok().flatten();
            let Some(flag) = slot else { crate::services::effects::tests::fixture::cleanup(&db, f).await.unwrap(); continue };
            let item = load_items(&db, &sde, ItemQuery::Id(f.module)).await.unwrap().pop().unwrap();
            assert!(crate::dao::items::inventory::move_item(&db, &item, f.ship, flag, f.character, true).await.unwrap());
            if crate::services::effects::fitting::validate_fit(&db, sde.clone(), f.character, f.ship, f.module).await.is_err() {
                crate::services::effects::tests::fixture::cleanup(&db, f).await.unwrap(); continue;
            }
            let before = Fit::load(&db, sde.clone(), f.character, f.ship).await.unwrap();
            let mut variants = Vec::new();
            for &charge in &charges {
                crate::dao::items::combat::apply_loads(&db, f.ship, &[(f.module, flag, charge, 1, 0)]).await.unwrap();
                let after = Fit::load(&db, sde.clone(), f.character, f.ship).await.unwrap();
                let changes = after.fitting_changes(&before.fitting_values(&before.fitting_candidates(&after)));
                crate::dao::items::combat::delete_charge(&db, f.ship, flag).await.unwrap();
                variants.extend(changes.into_iter().filter_map(|(key, attr, old, new)|
                    (key == Key::Module(f.module)).then_some((charge, attr, old, new))));
            }
            for &(charge, attr, old, new) in &variants {
                if let Some(&(replacement, _, _, replacement_value)) = variants.iter().find(|&&(other, a, _, value)| other != charge && a == attr && value != new) {
                    selected = Some((f, flag, charge, replacement, attr, old, new, replacement_value));
                    break 'search;
                }
            }
            crate::services::effects::tests::fixture::cleanup(&db, f).await.unwrap();
        }
    }
    let (fixture, flag, charge, replacement, attr, old, new, replacement_value) = selected.expect("published compatible charge pair with effective otherID module modifier");
    let station_data = sde.map.stations().find(|station| station.solar_system_id.is_some()).expect("station with system");
    let station = station_data.station_id;
    let system = station_data.solar_system_id.unwrap();
    let test_db = db.clone();
    let test_sde = sde.clone();
    let (mut client, peer) = crate::services::tests::client(test_db.clone(), fixture.character).await;
    let world = client.world.clone();
    let result = tokio::spawn(async move {
        test_db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&fixture.ship, &station, &FLAG_HANGAR]).await.unwrap();
        crate::dao::characters::ship::dock(&test_db, fixture.character, station, system).await.unwrap();
        crate::dao::characters::selection::set_ship(&test_db, fixture.character, fixture.ship).await.unwrap();
        let _peer = peer;
        client.session.apply(vec![("shipid", Value::Int(fixture.ship)), ("stationid", Value::Int(station))]);
        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = sender.clone();
        let source = crate::dao::items::transfers::insert(&test_db, charge, fixture.character, station, FLAG_HANGAR, 1).await.unwrap();
        assert_eq!(load_ammo_core(&mut client, fixture.ship, vec![fixture.module], vec![source], Some(station)).await.unwrap(), Value::None);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let prime = events.iter().find(|(name, _)| name == "OnGodmaPrimeItem").expect("script prime");
        assert_prime(&prime.1, &test_sde, fixture.ship, flag, charge, 1);
        assert!(events.iter().any(|(name, args)| name == "OnModuleAttributeChanges" && args[0].items().is_some_and(|rows|
            rows.iter().any(|row| row.items().is_some_and(|fields| fields.get(2) == Some(&Value::Int(fixture.module))
                && fields.get(3).and_then(Value::as_int) == Some(i64::from(attr))
                && fields.get(5).and_then(Value::as_float) == Some(new) && fields.get(6).and_then(Value::as_float) == Some(old))))));
        let source = crate::dao::items::transfers::insert(&test_db, replacement, fixture.character, station, FLAG_HANGAR, 1).await.unwrap();
        assert_eq!(load_ammo_core(&mut client, fixture.ship, vec![fixture.module], vec![source], Some(station)).await.unwrap(), Value::None);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let prime = events.iter().find(|(name, _)| name == "OnGodmaPrimeItem").expect("replacement script prime");
        assert_prime(&prime.1, &test_sde, fixture.ship, flag, replacement, 1);
        let final_fit = Fit::load(&test_db, test_sde.clone(), fixture.character, fixture.ship).await.unwrap();
        assert_eq!(final_fit.value(Key::Module(fixture.module), attr), replacement_value);
        assert!(events.iter().any(|(name, args)| name == "OnModuleAttributeChanges" && args[0].items().is_some_and(|rows|
            rows.iter().any(|row| row.items().is_some_and(|fields| fields.get(2) == Some(&Value::Int(fixture.module))
                && fields.get(3).and_then(Value::as_int) == Some(i64::from(attr))
                && fields.get(5).and_then(Value::as_float) == Some(replacement_value) && fields.get(6).and_then(Value::as_float) == Some(old))))));
        unload_ammo_core(&mut client, fixture.ship, vec![fixture.module], station, FLAG_HANGAR).await.unwrap();
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|(name, args)| name == "OnModuleAttributeChanges" && args[0].items().is_some_and(|rows|
            rows.iter().any(|row| row.items().is_some_and(|fields| fields.get(2) == Some(&Value::Int(fixture.module))
                && fields.get(3).and_then(Value::as_int) == Some(i64::from(attr))
                && fields.get(5).and_then(Value::as_float) == Some(old) && fields.get(6).and_then(Value::as_float) == Some(replacement_value))))));
        test_db.execute("UPDATE game.items SET location_id=$2,flag_id=0,x=0,y=0,z=0 WHERE item_id=$1", &[&fixture.ship, &system]).await.unwrap();
        crate::dao::characters::ship::undock(&test_db, fixture.character, system).await.unwrap();
        crate::dao::characters::selection::set_ship(&test_db, fixture.character, fixture.ship).await.unwrap();
        crate::dao::items::dogma::set_override(&test_db, fixture.module, ATTR_RELOAD_TIME, 2_000.0).await.unwrap();
        client.session.apply(vec![("stationid", Value::None), ("solarsystemid", Value::Int(system))]);
        client.world.register_pilot(fixture.character, sender);
        let hull = load_items(&test_db, &test_sde, ItemQuery::Id(fixture.ship)).await.unwrap().pop().unwrap();
        let ball = crate::services::effects::tests::fixture::ball(fixture.ship, hull.type_id, fixture.character, Some(fixture.character), 0.0);
        crate::services::effects::tests::fixture::admit(&test_db, &client.world, system, ball).await;
        let ship = fixture.ship;
        let revision = client.world.with(system, move |s| s.combat_fit_revision(ship).unwrap()).await.unwrap();
        let source = crate::dao::items::transfers::insert(&test_db, charge, fixture.character, ship, FLAG_CARGO, 1).await.unwrap();
        assert_eq!(load_ammo_core(&mut client, ship, vec![fixture.module], vec![source], Some(ship)).await.unwrap(), Value::None);
        let marker = *state().reloading.get(&fixture.module).expect("script reload marker");
        let pending_rows = crate::dao::items::pending_reloads::for_ship(&test_db, ship).await.unwrap();
        assert_eq!(pending_rows.len(), 1);
        let pending = PendingLoad { owner: fixture.character, reload_ids: vec![pending_rows[0].reload_id],
            external_sources: vec![], external_facts: Vec::new(), load: (fixture.module, flag, charge, pending_rows[0].quantity, 0) };
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
                        fields.get(2) == Some(&Value::Int(fixture.module)) && fields.get(3).and_then(Value::as_int) == Some(i64::from(attr))
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
        assert_eq!(final_fit.value(Key::Module(fixture.module), attr), new);
        assert!(!final_fit.overrides(Key::Charge(fixture.module)).iter().any(|(id, _)| *id == infinity_core::consts::dogma::ATTR_QUANTITY));
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
    state().reloading.remove(&fixture.module);
    super::super::super::forget_ship(fixture.ship);
    db.execute("DELETE FROM game.pending_reloads WHERE ship_id=$1", &[&fixture.ship]).await.unwrap();
    crate::services::effects::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
