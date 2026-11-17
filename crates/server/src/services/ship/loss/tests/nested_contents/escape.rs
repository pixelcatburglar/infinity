//! Regressions for the frigate escape-bay survivor of the piloted loss
//! transaction: an eligible assembled frigate keeps its original item ID,
//! fitting, cargo and persisted state; a rolled-back loss restores custody
//! exactly; an unavailable bay falls back to a fresh capsule without taking
//! the bay candidate.
use super::*;
use infinity_core::consts::dogma::{ATTR_ARMOR_DAMAGE, ATTR_FRIGATE_ESCAPE_BAY_CAPACITY, ATTR_HULL_DAMAGE, ATTR_SHIELD_CHARGE};

/// First published ship type carrying positive frigate escape-bay capacity.
fn escape_host_type(sde: &Sde) -> i32 {
    sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.type_id != TYPE_CAPSULE && t.group_id.is_some_and(|g| sde.repairable_ship.enters_recovery(t.type_id, g, CATEGORY_SHIP) && !sde.repairable_ship.unrepairable(CATEGORY_SHIP, g, t.type_id)) && t.attribute(ATTR_FRIGATE_ESCAPE_BAY_CAPACITY).is_some_and(|v| v > 0.0))
        .expect("published escape-capable host").type_id
}

/// First published type the escape bay's shared hold model accepts.
fn escape_frigate_type(sde: &Sde) -> i32 {
    sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && holds::accepts(sde, FLAG_FRIGATE_ESCAPE_BAY, t.type_id))
        .expect("published escape-accepted frigate").type_id
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn loaded_escape_publishes_original_ship_and_preserves_loss_boundary() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let (mut client, _peer) = crate::services::tests::client(db.clone(), character).await;
    client.login.user_id = fixture.account;
    let world = client.world.clone();
    let sde = client.sde.clone();
    let mut created = Vec::new();
    let mut space_cleanup = None;
    let result = catch_scenario(async {
        set_home(&db, &sde, character).await;
        let system = sde.map.systems().next().unwrap().solar_system_id;
        let host_type = escape_host_type(&sde);
        let frigate_type = escape_frigate_type(&sde);
        world.system(system).await.unwrap();
        let host_radius = crate::services::ship::balls::radius(&sde, host_type).unwrap();
        let survivor_radius = crate::services::ship::balls::radius(&sde, frigate_type).unwrap();
        let edge = world.with(system, |s| s.balls().map(|b| b.pos[0] + f64::from(b.radius)).fold(0.0, f64::max)).await.unwrap();
        const PLACEMENT_CLEARANCE_M: f64 = 1_000.0; // fixture margin beyond combined collision radii, not a game rule
        let position = [edge + f64::from(host_radius.max(survivor_radius)) + PLACEMENT_CLEARANCE_M, 0.0, 0.0];
        let module_kind = module_type(&sde);
        let charge_kind = charge_type(&sde);
        let box_kind = container_type(&sde);
        // The survivor is created before its host: the escape publishes this
        // original item ID, never a freshly minted row.
        let survivor = creation::positioned(&db, frigate_type, character, system, [9.0; 3]).await.unwrap();
        let host = creation::positioned(&db, host_type, character, system, position).await.unwrap();
        db.execute("UPDATE game.items SET location_id=$2, flag_id=$3, x=NULL, y=NULL, z=NULL WHERE item_id=$1", &[&survivor, &host, &FLAG_FRIGATE_ESCAPE_BAY]).await.unwrap();
        // Survivor fitting, loaded charge and container cargo.
        let survivor_module = item_in(&db, module_kind, character, survivor, FLAG_FITTING_FIRST, 1, true).await;
        let survivor_box = item_in(&db, box_kind, character, survivor, FLAG_CARGO, 1, true).await;
        let survivor_cargo = item_in(&db, charge_kind, character, survivor_box, FLAG_CARGO, 7, false).await;
        item_combat::apply_loads(&db, survivor, &[(survivor_module, FLAG_FITTING_FIRST, charge_kind, 3, 0)]).await.unwrap();
        // Host-side doomed fitting and cargo stack.
        let host_module = item_in(&db, module_kind, character, host, FLAG_FITTING_FIRST, 1, true).await;
        let host_stack = item_in(&db, charge_kind, character, host, FLAG_CARGO, 5, false).await;
        item_combat::apply_loads(&db, host, &[(host_module, FLAG_FITTING_FIRST, charge_kind, 2, 0)]).await.unwrap();
        created.extend([survivor, host, survivor_module, survivor_box, survivor_cargo, host_module, host_stack]);
        let survivor_ids = [survivor, survivor_module, survivor_box, survivor_cargo];
        let life_before: Vec<(i64, f64)> = db.query("SELECT item_id, damage FROM game.item_service_life WHERE item_id=ANY($1) ORDER BY item_id", &[&survivor_ids.as_slice()]).await.unwrap().iter().map(|r| (r.get("item_id"), r.get("damage"))).collect();
        // Representative persisted damage and insurance on both hulls.
        for (attribute, value) in [(ATTR_SHIELD_CHARGE, 12.5), (ATTR_ARMOR_DAMAGE, 7.5), (ATTR_HULL_DAMAGE, 3.25)] {
            db.execute("INSERT INTO game.item_attributes (item_id, attribute_id, value) VALUES ($1,$2,$3)", &[&survivor, &attribute, &value]).await.unwrap();
        }
        crate::dao::insurance::insure(&db, survivor, character, frigate_type, 1.0, 1, i64::MAX).await.unwrap();
        crate::dao::insurance::insure(&db, host, character, host_type, 1.0, 1, i64::MAX).await.unwrap();
        // Reservation directions: local survivor retained, both cross-boundary
        // directions consumed with the doomed endpoint. source_location_id is
        // the container holding the source stack, not the stack itself.
        let retained = pending_reloads::insert(&db, character, survivor, survivor_module, FLAG_FITTING_FIRST, charge_kind, 1, FLAG_CARGO, survivor_box).await.unwrap();
        let survivor_to_host = pending_reloads::insert(&db, character, survivor, survivor_module, FLAG_FITTING_FIRST, charge_kind, 1, FLAG_CARGO, host).await.unwrap();
        let host_to_survivor = pending_reloads::insert(&db, character, host, host_module, FLAG_FITTING_FIRST, charge_kind, 1, FLAG_CARGO, survivor_box).await.unwrap();
        let retained_row = pending_reloads::for_ship(&db, survivor).await.unwrap()
            .into_iter().find(|r| r.reload_id == retained).expect("local survivor reload");
        db.execute("UPDATE game.characters SET station_id=NULL, solar_system_id=$2, ship_id=$3 WHERE character_id=$1", &[&character, &system, &host]).await.unwrap();
        let price = sde.prices.get(host_type);
        let insured = price.map(|p| p.adjusted).filter(|v| *v != 0.0)
            .or_else(|| price.map(|p| p.average).filter(|v| *v != 0.0))
            .or(sde.types.get(host_type).unwrap().base_price).unwrap_or(0.0);
        assert!(insured > 0.0, "escape host must carry an insured value for payout accounting");
        let payout = crate::isk::IskAmount::from_isk(crate::services::market::round2(insured)).unwrap();
        let balance = crate::services::wallets::balance(&db, character).await.unwrap();
        // Real client wiring: the fixture pilot hears every committed notify.
        client.session.apply(vec![("solarsystemid", Value::Int(system)), ("shipid", Value::Int(host))]);
        let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = sender.clone();
        world.register_pilot(character, sender);
        world.set_online_session(character, Some(OnlineSession { character_id: character, user_id: None, session_id: 1, ship_id: Some(host), station_id: None, system_id: Some(system), corporation_id: Some(1), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
        let row = FreeShip { item_id: host, type_id: host_type, owner_id: character, corporation_id: 1, alliance_id: -1, pos: position };
        let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
        world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        space_cleanup = Some((system, survivor));
        // Real lethal settle: apply_damage_guarded takes the market lock itself.
        let outcome = apply_damage_guarded(&db, &world, system, host, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true, |_| true).await.ok().unwrap().unwrap();
        assert!(outcome.lethal);
        // The real __ship_destroyed -> ship_destroyed finisher lands the
        // original ID on the connected session.
        let mut destroyed = None;
        let mut survivor_change = None;
        while let Ok((event, args)) = notices.try_recv() {
            if event == "__ship_destroyed" { destroyed = Some(args); }
            else if event == "OnItemChange" && crate::services::item_descriptor().get(&args[0], "itemID") == Some(Value::Int(survivor)) { survivor_change = Some(args); }
        }
        let destroyed = destroyed.expect("committed loss must notify __ship_destroyed");
        assert_eq!(destroyed[0].as_int(), Some(host));
        assert_eq!(destroyed[4].as_int(), Some(survivor));
        crate::services::ship_destroyed(&mut client, &destroyed).await.unwrap();
        assert_eq!(client.session.int("shipid"), Some(survivor));
        // The targeted relocation notice names the original ID at its new
        // location, with the old host and escape flag in the change dict.
        let change = survivor_change.expect("survivor OnItemChange");
        let descriptor = crate::services::item_descriptor();
        assert_eq!(descriptor.get(&change[0], "itemID"), Some(Value::Int(survivor)));
        assert_eq!(descriptor.get(&change[0], "locationID"), Some(Value::Int(system)));
        assert_eq!(descriptor.get(&change[0], "flagID"), Some(Value::Int(0)));
        assert_eq!(change[1], Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(host)), (Value::Int(IX_FLAG), Value::Int(i64::from(FLAG_FRIGATE_ESCAPE_BAY)))]));
        assert_eq!(change[2], Value::None);
        // Original ID selected in the database, live in space and in the
        // committed online session; the host ball is gone.
        assert_eq!(selection::ship_id(&db, character).await.unwrap(), Some(survivor));
        assert_eq!(world.with(system, move |s| s.pilot_ball(character).map(|b| (b.id, b.pos))).await.flatten(), Some((survivor, position)));
        assert_eq!(world.online_session(character).unwrap().ship_id, Some(survivor));
        assert!(world.ball(system, host).await.is_none());
        // The relocated row sits at the sampled loss position; no capsule exists.
        let row = db.query_one("SELECT location_id, flag_id, x, y, z FROM game.items WHERE item_id=$1", &[&survivor]).await.unwrap();
        assert_eq!((row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"), row.get::<_, Option<f64>>("x"), row.get::<_, Option<f64>>("y"), row.get::<_, Option<f64>>("z")), (system, 0, Some(position[0]), Some(position[1]), Some(position[2])));
        assert!(db.query("SELECT 1 FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&character, &TYPE_CAPSULE]).await.unwrap().is_empty());
        // Survivor parents, ownership, quantities, charges, damage and
        // insurance intact; host closure and its contract are consumed.
        assert_eq!(item_rows(&db, &[survivor_module, survivor_box, survivor_cargo]).await,
            vec![(survivor_module, module_kind, character, survivor, FLAG_FITTING_FIRST, 1, true),
                 (survivor_box, box_kind, character, survivor, FLAG_CARGO, 1, true),
                 (survivor_cargo, charge_kind, character, survivor_box, FLAG_CARGO, 7, false)]);
        let charge = db.query_one("SELECT type_id, quantity FROM game.module_charges WHERE ship_id=$1 AND flag_id=$2", &[&survivor, &FLAG_FITTING_FIRST]).await.unwrap();
        assert_eq!((charge.get::<_, i32>("type_id"), charge.get::<_, i64>("quantity")), (charge_kind, 3));
        for (attribute, value) in [(ATTR_SHIELD_CHARGE, 12.5), (ATTR_ARMOR_DAMAGE, 7.5), (ATTR_HULL_DAMAGE, 3.25)] {
            let kept: f64 = db.query_one("SELECT value FROM game.item_attributes WHERE item_id=$1 AND attribute_id=$2", &[&survivor, &attribute]).await.unwrap().get("value");
            assert_eq!(kept, value, "persisted damage attribute {attribute}");
        }
        assert!(crate::dao::insurance::contract(&db, survivor).await.unwrap().is_some());
        assert_custody(&db, character, &[(host, host_type, 0), (host_module, module_kind, FLAG_FITTING_FIRST)]).await;
        assert!(item_rows(&db, &[host_stack]).await.is_empty());
        let life_after: Vec<(i64, f64)> = db.query("SELECT item_id, damage FROM game.item_service_life WHERE item_id=ANY($1) ORDER BY item_id", &[&survivor_ids.as_slice()]).await.unwrap().iter().map(|r| (r.get("item_id"), r.get("damage"))).collect();
        assert_eq!(life_before, life_after);
        assert!(db.query("SELECT 1 FROM game.repairable_recovery_items WHERE item_id=ANY($1)", &[&survivor_ids.as_slice()]).await.unwrap().is_empty());
        assert!(db.query_opt("SELECT 1 FROM game.module_charges WHERE ship_id=$1", &[&host]).await.unwrap().is_none());
        assert!(crate::dao::insurance::contract(&db, host).await.unwrap().is_none());
        assert!(pending_reloads::for_ship(&db, survivor).await.unwrap() == vec![retained_row], "local survivor reload changed");
        assert!(pending_ids(&db, &[survivor_to_host, host_to_survivor]).await.is_empty());
        // Host-only accounting: its killmail and ShipLost name only the host;
        // the escaped hull never enters the killmail's <i t= f=> item list.
        let blob: String = db.query_one("SELECT blob FROM game.killmails WHERE victim_id=$1 AND victim_item_id=$2", &[&character, &host]).await.unwrap().get("blob");
        assert!(!blob.contains(&format!("t={frigate_type} ")) && !blob.contains(&format!("f={FLAG_FRIGATE_ESCAPE_BAY} ")));
        assert!(db.query_opt("SELECT 1 FROM game.killmails WHERE victim_item_id=$1", &[&survivor]).await.unwrap().is_none());
        assert_eq!(db.query("SELECT 1 FROM game.progress_events WHERE kind='ShipLost' AND subject_character_id=$1 AND source_key=$2", &[&character, &format!("ShipLost:{host}")]).await.unwrap().len(), 1);
        assert_eq!(crate::services::wallets::balance(&db, character).await.unwrap(), balance.checked_add(payout).unwrap());
    }).await;
    if let Some((system, ship)) = space_cleanup { world.remove_ball(system, ship).await; }
    world.unregister_pilot_generation(character, None);
    remove_tree(&db, &created).await;
    scrub_fixture_extras(&db, fixture).await;
    cleanup(&db, fixture, &created).await;
    resume_scenario(result);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn escape_transaction_error_restores_original_custody() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let mut created = Vec::new();
    let result = catch_scenario(async {
        set_home(&db, &sde, character).await;
        let system = sde.map.systems().next().unwrap().solar_system_id;
        let host_type = escape_host_type(&sde);
        let frigate_type = escape_frigate_type(&sde);
        let module_kind = module_type(&sde);
        let charge_kind = charge_type(&sde);
        let survivor = creation::positioned(&db, frigate_type, character, system, [9.0; 3]).await.unwrap();
        let host = creation::positioned(&db, host_type, character, system, [0.0; 3]).await.unwrap();
        db.execute("UPDATE game.items SET location_id=$2, flag_id=$3, x=NULL, y=NULL, z=NULL WHERE item_id=$1", &[&survivor, &host, &FLAG_FRIGATE_ESCAPE_BAY]).await.unwrap();
        let survivor_module = item_in(&db, module_kind, character, survivor, FLAG_FITTING_FIRST, 1, true).await;
        let host_stack = item_in(&db, charge_kind, character, host, FLAG_CARGO, 5, false).await;
        created.extend([survivor, host, survivor_module, host_stack]);
        let cross = pending_reloads::insert(&db, character, survivor, survivor_module, FLAG_FITTING_FIRST, charge_kind, 1, FLAG_CARGO, host).await.unwrap();
        crate::dao::insurance::insure(&db, survivor, character, frigate_type, 1.0, 1, i64::MAX).await.unwrap();
        crate::dao::insurance::insure(&db, host, character, host_type, 1.0, 1, i64::MAX).await.unwrap();
        selection::set_ship(&db, character, host).await.unwrap();
        db.execute("UPDATE game.characters SET solar_system_id=$2, station_id=NULL WHERE character_id=$1", &[&character, &system]).await.unwrap();
        let balance = crate::services::wallets::balance(&db, character).await.unwrap();
        let before = item_rows(&db, &[survivor, host, survivor_module, host_stack]).await;
        let reloads_before = pending_reloads::for_ship(&db, survivor).await.unwrap();
        let identity = Identity { char_id: character, corp_id: 1, ship_id: Some(host), station_id: None, system_id: Some(system) };
        let input = loss_input(host, character, host_type);
        let sde2 = sde.clone();
        let captured = Arc::new(std::sync::Mutex::new((Vec::new(), Vec::new()))); let captured_tx = captured.clone();
        let _market = crate::services::market::lock().await;
        let result: Result<Option<LossOutcome>, CallError> = db.transaction(
            move |tx, outbox: &mut Outbox<OnlineChanged>| {
                let sde2 = sde2.clone();
                let input = input.clone();
                let captured_tx = captured_tx.clone();
                Box::pin(async move {
                    let outcome = destroy_ship_in_transaction(tx, outbox, &identity, &sde2, &input, None, &[]).await?.expect("loaded escape must produce an outcome");
                    let jobs = capture_recovery_jobs(tx, character).await; *captured_tx.lock().unwrap() = jobs;
                    // The committed transaction already relocated and selected
                    // the original frigate before the forced failure lands.
                    assert_eq!(outcome.replacement_ship, survivor);
                    let moved = tx.query_one("SELECT location_id, flag_id FROM game.items WHERE item_id=$1", &[&survivor]).await.map_err(db_err)?;
                    assert_eq!((moved.get::<_, i64>("location_id"), moved.get::<_, i32>("flag_id")), (system, 0));
                    let selected: Option<i64> = tx.query_one("SELECT ship_id FROM game.characters WHERE character_id=$1", &[&character]).await.map_err(db_err)?.get("ship_id");
                    assert_eq!(selected, Some(survivor));
                    Err(db_err(crate::db::Error::InvalidItemLockRequest("forced post-extraction rollback")))
                })
            },
            |_v: Option<LossOutcome>, _o| async move { panic!("dispatch must not run for a rolled-back loss") },
        ).await;
        match result {
            Err(CallError::Internal(e)) => assert!(e.chain().any(|c| matches!(c.downcast_ref::<crate::db::Error>(), Some(crate::db::Error::InvalidItemLockRequest("forced post-extraction rollback")))), "expected forced rollback error, got {e:?}"),
            other => panic!("expected Internal, got {}", other.err().map_or("success".to_string(), |e| format!("{e:?}"))),
        }
        let captured = captured.lock().unwrap().clone();
        assert_recovery_jobs_rolled_back(&db, &captured).await;
        assert_eq!(item_rows(&db, &[survivor, host, survivor_module, host_stack]).await, before);
        assert!(pending_reloads::for_ship(&db, survivor).await.unwrap() == reloads_before, "survivor reloads changed after rollback");
        assert_eq!(pending_ids(&db, &[cross]).await.len(), 1);
        assert!(crate::dao::insurance::contract(&db, survivor).await.unwrap().is_some());
        assert!(crate::dao::insurance::contract(&db, host).await.unwrap().is_some());
        assert_eq!(crate::services::wallets::balance(&db, character).await.unwrap(), balance);
        assert_eq!(selection::ship_id(&db, character).await.unwrap(), Some(host));
        assert!(db.query_opt("SELECT 1 FROM game.killmails WHERE victim_id=$1 AND victim_item_id=$2", &[&character, &host]).await.unwrap().is_none());
        assert!(db.query("SELECT 1 FROM game.progress_events WHERE kind IN ('ShipLost','LossSettled') AND subject_character_id=$1", &[&character]).await.unwrap().is_empty());
        assert!(db.query("SELECT 1 FROM game.notifications WHERE receiver_id=$1", &[&character]).await.unwrap().is_empty());
        assert_no_recovery(&db, character, &[survivor, host, survivor_module, host_stack]).await;
    }).await;
    remove_tree(&db, &created).await;
    scrub_fixture_extras(&db, fixture).await;
    cleanup(&db, fixture, &created).await;
    resume_scenario(result);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn unavailable_escape_uses_capsule_without_taking_candidate() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let mut created = Vec::new();
    let result = catch_scenario(async {
        set_home(&db, &sde, character).await;
        let system = sde.map.systems().next().unwrap().solar_system_id;
        let host_type = escape_host_type(&sde);
        let frigate_type = escape_frigate_type(&sde);
        let foreign = foreign_character(&db, fixture).await;
        for (case, owner, zeroed) in [("zero-capacity host", character, true), ("foreign-owned candidate", foreign, false)] {
            let candidate = creation::positioned(&db, frigate_type, owner, system, [4.0; 3]).await.unwrap();
            let host = creation::positioned(&db, host_type, character, system, [0.0; 3]).await.unwrap();
            created.extend([candidate, host]);
            db.execute("UPDATE game.items SET location_id=$2, flag_id=$3, x=NULL, y=NULL, z=NULL WHERE item_id=$1", &[&candidate, &host, &FLAG_FRIGATE_ESCAPE_BAY]).await.unwrap();
            if zeroed {
                assert!(crate::dao::items::dogma::set_override_if(&db, host, ATTR_FRIGATE_ESCAPE_BAY_CAPACITY, None, 0.0).await.unwrap());
            }
            selection::set_ship(&db, character, host).await.unwrap();
            db.execute("UPDATE game.characters SET solar_system_id=$2, station_id=NULL WHERE character_id=$1", &[&character, &system]).await.unwrap();
            // Direct transaction calls take the existing market guard once.
            let _market = crate::services::market::lock().await;
            let outcome = piloted_loss(&db, &sde, character, host, system, host_type, &[]).await.unwrap().expect("unavailable escape still produces an outcome");
            assert_ne!(outcome.replacement_ship, candidate, "{case}");
            let capsule = item_rows(&db, &[outcome.replacement_ship]).await;
            assert_eq!(capsule.len(), 1, "{case}");
            assert_eq!(capsule[0].1, TYPE_CAPSULE, "{case}: the replacement is a fresh capsule");
            assert_eq!(selection::ship_id(&db, character).await.unwrap(), Some(outcome.replacement_ship), "{case}");
            assert_custody(&db, character, &[(host, host_type, 0)]).await;
            assert!(item_rows(&db, &[candidate]).await.is_empty(), "{case}: candidate consumed");
            assert!(db.query_opt("SELECT 1 FROM game.killmails WHERE victim_id=$1 AND victim_item_id=$2", &[&character, &host]).await.unwrap().is_some(), "{case}");
        }
    }).await;
    remove_tree(&db, &created).await;
    scrub_fixture_extras(&db, fixture).await;
    cleanup(&db, fixture, &created).await;
    resume_scenario(result);
}
