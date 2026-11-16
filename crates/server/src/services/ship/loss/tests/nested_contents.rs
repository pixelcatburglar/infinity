//! Permanent regressions for whole-closure destruction through the piloted
//! loss transaction: deep foreign-owned contents, mutable attachments,
//! intersecting reservations, rollback atomicity, lock ordering and the
//! bounded whole-transaction retry on snapshot drift.
//! Loaded consumption precedes reservation inserts: a reserved flag
//! cold-loads pending and refuses the next hold.
use super::*;
use crate::services::tests::fixture::{assert_recovery_jobs_rolled_back, capture_recovery_jobs};
use infinity_core::consts::{CATEGORY_MODULE, GROUP_CARGO_CONTAINER,
    FLAG_CARGO, FLAG_DRONE_BAY, FLAG_FITTING_FIRST, FLAG_IMPLANT};
use crate::dao::items::{combat as item_combat, inventory, pending_reloads};
use std::{collections::BTreeSet, sync::atomic::{AtomicUsize, Ordering}, time::Duration};
mod escape;

const TEST_ATTR: i32 = 1;
const TEST_ATTR_B: i32 = 2;

fn loss_input(ship: i64, character: i64, ship_type: i32) -> LossInput {
    LossInput {
        position: [10.0, 20.0, 30.0],
        source: DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() },
        lethal_at: crate::client::filetime_now(),
        contributions: Vec::new(),
        victim: crate::lua::events::combat::CombatEntity {
            item_id: ship, type_id: ship_type, incarnation: [0; 16],
            character_id: Some(character), corporation_id: Some(1), alliance_id: None, faction_id: None,
        },
        context: crate::lua::events::resources::FactContext {
            corporation_id: None, alliance_id: None, faction_id: None,
            ship_id: Some(ship), ship_type_id: Some(ship_type), system_id: None, station_id: None,
            origin: crate::lua::events::resources::FactOrigin::Gm,
        },
        conflict: crate::lua::events::combat::ConflictKind::PlayerVersusPlayer,
        pending: Arc::new(facts::PlayerLossContext::new(None, [0.0; 3])),
    }
}

fn container_type(sde: &Sde) -> i32 {
    sde.types.iter().find(|t| t.published == Some(true) && t.group_id == Some(GROUP_CARGO_CONTAINER)
        && t.volume.is_some_and(|v| v.is_finite() && v > 0.0))
        .expect("published cargo container").type_id
}

fn hull_type(sde: &Sde) -> i32 {
    sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP) && t.type_id != TYPE_CAPSULE && t.group_id.is_some_and(|g| sde.repairable_ship.enters_recovery(t.type_id, g, CATEGORY_SHIP) && !sde.repairable_ship.unrepairable(CATEGORY_SHIP, g, t.type_id)))
        .expect("published non-capsule ship").type_id
}

fn module_type(sde: &Sde) -> i32 {
    sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE) && t.group_id.is_some_and(|g| sde.repairable_ship.enters_recovery(t.type_id, g, CATEGORY_MODULE) && !sde.repairable_ship.unrepairable(CATEGORY_MODULE, g, t.type_id)))
        .expect("published module").type_id
}

fn charge_type(sde: &Sde) -> i32 {
    crate::services::combat::targeting_charges::tests::compatible_ammunition(sde).1
}

/// Insert an item inside `location` without going through service paths.
async fn item_in(db: &Db, type_id: i32, owner: i64, location: i64, flag: i32, quantity: i64, singleton: bool) -> i64 {
    db.query_one(
        "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1,$2,$3,$4,$5,$6) RETURNING item_id",
        &[&type_id, &owner, &location, &flag, &quantity, &singleton],
    ).await.unwrap().get("item_id")
}

/// Count attachment rows per table keyed by any id in `ids`.
async fn attachment_counts(db: &Db, ids: &[i64]) -> i64 {
    let mut total = 0_i64;
    for (table, column) in [
        ("item_attributes", "item_id"), ("ship_configuration", "ship_item_id"),
        ("ship_dirt", "ship_item_id"), ("ship_skin_applications", "ship_item_id"),
        ("dynamic_items", "item_id"), ("dynamic_item_modifiers", "item_id"),
        ("module_charges", "ship_id"), ("weapon_banks", "ship_id"),
        ("pending_reloads", "ship_id"), ("insurance", "ship_id"), ("blueprints", "item_id"),
    ] {
        let n: i64 = db.query_one(&format!("SELECT count(*) AS n FROM game.{table} WHERE {column} = ANY($1)"), &[&ids]).await.unwrap().get("n");
        total += n;
    }
    total
}

/// Sorted item rows at the given ids.
async fn item_rows(db: &Db, ids: &[i64]) -> Vec<(i64, i32, i64, i64, i32, i64, bool)> {
    db.query(
        "SELECT item_id, type_id, owner_id, location_id, flag_id, quantity, singleton FROM game.items WHERE item_id = ANY($1) ORDER BY item_id",
        &[&ids],
    ).await.unwrap().iter()
        .map(|r| (r.get("item_id"), r.get("type_id"), r.get("owner_id"), r.get("location_id"), r.get("flag_id"), r.get("quantity"), r.get("singleton")))
        .collect()
}

async fn set_home(db: &Db, sde: &Sde, character: i64) {
    let station = sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).unwrap().station_id;
    db.execute("UPDATE game.characters SET home_station_id=$2 WHERE character_id=$1", &[&character, &station]).await.unwrap();
}

async fn assert_custody(db: &Db, character: i64, expected: &[(i64, i32, i32)]) {
    let group: i64 = db.query_one("SELECT group_id FROM game.repairable_recovery_groups WHERE beneficiary_id=$1 AND loss_id=$2", &[&character, &expected[0].0]).await.unwrap().get("group_id");
    let rows = item_rows(db, &expected.iter().map(|x| x.0).collect::<Vec<_>>()).await;
    let mut wanted: Vec<_> = expected.iter().map(|&(id, ty, flag)| (id, ty, infinity_core::consts::NPC_CORP_INTERBUS, group, flag, 1, true)).collect();
    wanted.sort_unstable_by_key(|row| row.0);
    assert_eq!(rows, wanted);
    assert_eq!(db.query("SELECT item_id FROM game.repairable_recovery_items WHERE group_id=$1 ORDER BY item_id", &[&group]).await.unwrap().iter().map(|r| r.get::<_, i64>("item_id")).collect::<Vec<_>>(), wanted.iter().map(|row| row.0).collect::<Vec<_>>());
}

async fn assert_no_recovery(db: &Db, character: i64, ids: &[i64]) {
    assert!(db.query("SELECT 1 FROM game.repairable_recovery_groups WHERE beneficiary_id=$1", &[&character]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.item_service_life WHERE item_id=ANY($1)", &[&ids]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.notifications WHERE receiver_id=$1", &[&character]).await.unwrap().is_empty());
}

async fn pending_ids(db: &Db, reload_ids: &[i64]) -> Vec<i64> {
    db.query("SELECT reload_id FROM game.pending_reloads WHERE reload_id = ANY($1) ORDER BY reload_id", &[&reload_ids])
        .await.unwrap().iter().map(|r| r.get("reload_id")).collect()
}

/// Delete fixture rows a scenario created, whichever state they ended in.
/// Metadata rows keyed to already-deleted items cannot be discovered through
/// item joins, so they are removed by exact key first.
async fn remove_tree(db: &Db, roots: &[i64]) {
    for (table, column) in [
        ("item_attributes", "item_id"), ("ship_configuration", "ship_item_id"), ("ship_dirt", "ship_item_id"),
        ("ship_skin_applications", "ship_item_id"), ("dynamic_item_modifiers", "item_id"), ("dynamic_items", "item_id"),
        ("module_charges", "ship_id"), ("weapon_banks", "ship_id"), ("weapon_banks", "master_id"), ("weapon_banks", "slave_id"),
        ("pending_reloads", "ship_id"), ("pending_reloads", "source_location_id"),
        ("insurance", "ship_id"), ("blueprints", "item_id"), ("char_notes", "item_id"),
    ] {
        db.execute(&format!("DELETE FROM game.{table} WHERE {column} = ANY($1)"), &[&roots]).await.unwrap();
    }
    for _ in 0..4 {
        db.execute("DELETE FROM game.items WHERE item_id = ANY($1) OR location_id = ANY($1)", &[&roots]).await.unwrap();
        db.execute("DELETE FROM game.items WHERE location_id IN (SELECT item_id FROM game.items WHERE item_id = ANY($1))", &[&roots]).await.unwrap();
    }
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&roots]).await.unwrap();
}

/// Remove the extra fixture rows this suite's scenarios may leave behind:
/// character-owned notes and skin licenses, plus any secondary characters.
async fn scrub_fixture_extras(db: &Db, fixture: crate::services::tests::fixture::Fixture) {
    db.execute("DELETE FROM game.ship_skin_applications WHERE character_id = $1", &[&fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.char_notes WHERE char_id IN (SELECT character_id FROM game.characters WHERE user_id = $1)", &[&fixture.account]).await.unwrap();
    db.execute("DELETE FROM game.ship_skin_licenses WHERE character_id IN (SELECT character_id FROM game.characters WHERE user_id = $1)", &[&fixture.account]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id IN (SELECT character_id FROM game.characters WHERE user_id = $1 AND character_id <> $2)", &[&fixture.account, &fixture.character]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE user_id = $1 AND character_id <> $2", &[&fixture.account, &fixture.character]).await.unwrap();
}

/// Run the real piloted loss once through `Db::transaction`, mirroring the
/// settlement call shape in services/combat/damage.rs.
async fn piloted_loss(db: &Db, sde: &Arc<Sde>, character: i64, ship: i64, system: i64, ship_kind: i32,
    loaded: &[crate::dao::items::charges::LoadedWrite]) -> Result<Option<LossOutcome>, CallError> {
    let identity = Identity { char_id: character, corp_id: 1, ship_id: Some(ship), station_id: None, system_id: Some(system) };
    let input = loss_input(ship, character, ship_kind);
    let sde = sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            let input = input.clone();
            let loaded = loaded.to_vec();
            Box::pin(async move { destroy_ship_in_transaction(tx, outbox, &identity, &sde, &input, None, &loaded).await })
        },
        |outcome, _outbox| async move { Ok(outcome) },
    ).await
}

/// Add a second character to the fixture account so closure ownership can differ.
async fn foreign_character(db: &Db, fixture: crate::services::tests::fixture::Fixture) -> i64 {
    db.query_one(
        "INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'nested-loss-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id",
        &[&fixture.account],
    ).await.unwrap().get("character_id")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn deep_foreign_owned_closure_and_attachments_destroyed() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let mut created = Vec::new();
    let result = catch_scenario(async {
        set_home(&db, &sde, character).await;
        let system = sde.map.systems().next().unwrap().solar_system_id;
        let hull = hull_type(&sde);
        let box_kind = container_type(&sde);
        let module_kind = module_type(&sde);
        let charge_kind = charge_type(&sde);
        let drone_kind = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_DRONE) && t.group_id.is_some_and(|g| sde.repairable_ship.enters_recovery(t.type_id, g, infinity_core::consts::CATEGORY_DRONE) && !sde.repairable_ship.unrepairable(infinity_core::consts::CATEGORY_DRONE, g, t.type_id))).expect("recoverable drone").type_id;
        let foreign = foreign_character(&db, fixture).await;
        // root -> outer box -> inner box -> foreign-owned hull -> module/cargo.
        let root = creation::positioned(&db, hull, character, system, [0.0; 3]).await.unwrap();
        created.push(root);
        let outer = item_in(&db, box_kind, character, root, FLAG_CARGO, 1, true).await;
        let inner = item_in(&db, box_kind, character, outer, FLAG_CARGO, 1, true).await;
        let nested = item_in(&db, hull, foreign, inner, FLAG_CARGO, 1, true).await;
        let nested_module = item_in(&db, module_kind, foreign, nested, FLAG_FITTING_FIRST, 1, true).await;
        let nested_slave = item_in(&db, module_kind, foreign, nested, FLAG_FITTING_FIRST + 1, 1, true).await;
        let nested_cargo = item_in(&db, charge_kind, foreign, nested, FLAG_CARGO, 9, false).await;
        let root_module = item_in(&db, module_kind, character, root, FLAG_FITTING_FIRST, 1, true).await;
        let root_slave = item_in(&db, module_kind, character, root, FLAG_FITTING_FIRST + 1, 1, true).await;
        let blueprint = item_in(&db, charge_kind, character, root, FLAG_CARGO, 1, false).await;
        let drone = item_in(&db, drone_kind, character, root, FLAG_DRONE_BAY, 1, true).await;
        created.extend([outer, inner, nested, nested_module, nested_slave, nested_cargo, root_module, root_slave, blueprint, drone]);
        // An external surviving ship holding one stack and one module.
        let outside = creation::positioned(&db, hull, foreign, system, [1.0; 3]).await.unwrap();
        created.push(outside);
        let outside_stack = item_in(&db, charge_kind, foreign, outside, FLAG_CARGO, 7, false).await;
        let outside_module = item_in(&db, module_kind, foreign, outside, FLAG_FITTING_FIRST, 1, true).await;
        created.extend([outside_stack, outside_module]);
        // Mutable attachments at root and at the deepest hull.
        for id in [root, nested] {
            db.execute("INSERT INTO game.item_attributes (item_id, attribute_id, value) VALUES ($1,$2,1.5),($1,$3,2.5)", &[&id, &TEST_ATTR, &TEST_ATTR_B]).await.unwrap();
            db.execute("INSERT INTO game.ship_configuration (ship_item_id, owner_id) VALUES ($1,$2)", &[&id, &character]).await.unwrap();
            db.execute("INSERT INTO game.ship_dirt (ship_item_id, dirt_timestamp) VALUES ($1, 42)", &[&id]).await.unwrap();
            db.execute("INSERT INTO game.ship_skin_licenses (character_id, skin_id, expires_at) VALUES ($1,$2,NULL) ON CONFLICT DO NOTHING", &[&character, &TEST_ATTR]).await.unwrap();
            db.execute("INSERT INTO game.ship_skin_applications (ship_item_id, character_id, skin_id) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING", &[&id, &character, &TEST_ATTR]).await.unwrap();
            db.execute("INSERT INTO game.dynamic_items (item_id, source_item_id, mutator_item_id, source_type_id, mutator_type_id, result_type_id, creator_id, created_at) VALUES ($1,$1,$1,1,1,1,$2,1) ON CONFLICT DO NOTHING", &[&id, &character]).await.unwrap();
            db.execute("INSERT INTO game.dynamic_item_modifiers (item_id, attribute_id, factor) VALUES ($1,$2,1.0) ON CONFLICT DO NOTHING", &[&id, &TEST_ATTR]).await.unwrap();
            db.execute("INSERT INTO game.insurance (ship_id, owner_id, type_id, fraction, start_date, end_date) VALUES ($1,$2,$3,1.0,1,$4) ON CONFLICT DO NOTHING", &[&id, &character, &hull, &i64::MAX]).await.unwrap();
        }
        item_combat::apply_loads(&db, root, &[(root_module, FLAG_FITTING_FIRST, charge_kind, 3, 0)]).await.unwrap();
        item_combat::apply_loads(&db, nested, &[(nested_module, FLAG_FITTING_FIRST, charge_kind, 2, 0)]).await.unwrap();
        item_combat::move_bank(&db, root, root_module, root_slave).await.unwrap();
        item_combat::move_bank(&db, nested, nested_module, nested_slave).await.unwrap();
        db.execute("INSERT INTO game.blueprints (item_id, material_efficiency, time_efficiency, runs) VALUES ($1,7,6,3) ON CONFLICT DO NOTHING", &[&blueprint]).await.unwrap();
        // Licenses, private notes and historical rows stay.
        db.execute("INSERT INTO game.char_notes (char_id, item_id, note) VALUES ($1,$2,'kept')", &[&character, &nested]).await.unwrap();
        let before_items = item_rows(&db, &[outside, outside_stack, outside_module]).await;
        selection::set_ship(&db, character, root).await.unwrap();
        db.execute("UPDATE game.characters SET solar_system_id=$2, station_id=NULL WHERE character_id=$1", &[&character, &system]).await.unwrap();
        // A live dirty absolute must reach the loss reads: consume two rounds
        // under the real actor, then hand the same capture to the settlement.
        let world = crate::space::World::new(db.clone(), sde.clone(), 1);
        world.system(system).await.unwrap();
        let row = crate::dao::items::space_ships::FreeShip { item_id: root, type_id: hull, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &row, character, 0.0).await.unwrap();
        world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let loaded = world.with(system, move |s| {
            for _ in 0..2 {
                let permit = s.hold_loaded_use(root, root_module).unwrap();
                assert!(s.consume_loaded(permit).is_some());
            }
            s.capture_dirty_loaded(&[root])
        }).await.unwrap();
        // Reservations are created only after the two consumes: a pending
        // reservation on this flag cold-loads the slot pending, refusing use.
        let unrelated_reload = pending_reloads::insert(&db, foreign, outside, outside_module, FLAG_FITTING_FIRST, charge_kind, 1, FLAG_CARGO, outside).await.unwrap();
        // Reservation matrix: destroyed weapon + destroyed source, destroyed
        // weapon + external source, external weapon + destroyed source.
        let doomed_both = pending_reloads::insert(&db, character, root, root_module, FLAG_FITTING_FIRST, charge_kind, 1, FLAG_CARGO, outer).await.unwrap();
        let doomed_external_source = pending_reloads::insert(&db, character, root, root_module, FLAG_FITTING_FIRST, charge_kind, 1, FLAG_CARGO, outside_stack).await.unwrap();
        let doomed_external_weapon = pending_reloads::insert(&db, foreign, outside, outside_module, FLAG_FITTING_FIRST, charge_kind, 1, FLAG_CARGO, nested_cargo).await.unwrap();
        // Physical source item deleted before capture: the reservation names a
        // source id that is already absent at snapshot time, exercising the
        // initially-absent source policy rather than the classifier's decision.
        let absent_source = item_in(&db, charge_kind, character, 1, FLAG_CARGO, 1, false).await;
        db.execute("DELETE FROM game.items WHERE item_id=$1", &[&absent_source]).await.unwrap();
        let doomed_absent_source = pending_reloads::insert(&db, character, root, root_module, FLAG_FITTING_FIRST, charge_kind, 1, FLAG_CARGO, absent_source).await.unwrap();
        let outcome = piloted_loss(&db, &sde, character, root, system, hull, &loaded).await.unwrap();
        assert!(outcome.is_some(), "piloted loss must produce a capsule outcome");
        let destroyed = [outer, inner, nested, nested_module, nested_slave, nested_cargo, blueprint];
        assert!(item_rows(&db, &destroyed).await.is_empty(), "destroyed closure members must be deleted");
        assert_eq!(attachment_counts(&db, &destroyed).await, 0);
        assert_custody(&db, character, &[(root, hull, 0), (root_module, module_kind, FLAG_FITTING_FIRST), (root_slave, module_kind, FLAG_FITTING_FIRST + 1), (drone, drone_kind, FLAG_DRONE_BAY)]).await;
        for table in ["ship_configuration", "ship_dirt", "ship_skin_applications", "dynamic_items", "dynamic_item_modifiers"] {
            assert_eq!(db.query(&format!("SELECT 1 FROM game.{table} WHERE {}=$1", if table.starts_with("ship_") { "ship_item_id" } else { "item_id" }), &[&root]).await.unwrap().len(), 1);
        }
        assert!(db.query_opt("SELECT 1 FROM game.item_service_life WHERE item_id=$1", &[&root]).await.unwrap().is_some());
        assert_eq!(db.query("SELECT 1 FROM game.item_attributes WHERE item_id=$1 AND attribute_id=ANY($2)", &[&root, &&[TEST_ATTR, TEST_ATTR_B][..]]).await.unwrap().len(), 2);
        assert!(db.query("SELECT 1 FROM game.module_charges WHERE ship_id=$1 UNION ALL SELECT 1 FROM game.weapon_banks WHERE ship_id=$1", &[&root]).await.unwrap().is_empty());
        assert!(pending_ids(&db, &[doomed_both, doomed_external_source, doomed_external_weapon, doomed_absent_source]).await.is_empty());
        assert_eq!(item_rows(&db, &[outside, outside_stack, outside_module]).await, before_items);
        assert_eq!(pending_ids(&db, &[unrelated_reload]).await.len(), 1);
        assert!(db.query_opt("SELECT 1 FROM game.char_notes WHERE char_id=$1 AND item_id=$2", &[&character, &nested]).await.unwrap().is_some());
        assert!(db.query_opt("SELECT 1 FROM game.ship_skin_licenses WHERE character_id=$1 AND skin_id=$2", &[&character, &TEST_ATTR]).await.unwrap().is_some());
        let blob: String = db.query_one("SELECT blob FROM game.killmails WHERE victim_item_id=$1", &[&root]).await.unwrap().get("blob");
        assert!(blob.contains(&format!("t={charge_kind} f={FLAG_FITTING_FIRST} s=0 d=0 x=1")),
            "the killmail sees the settled absolute, not the durable residue: {blob}");
        world.remove_ball(system, root).await;
        assert!(db.query_opt("SELECT 1 FROM game.killmails WHERE victim_id=$1 AND victim_item_id=$2", &[&character, &root]).await.unwrap().is_some());
    }).await;
    remove_tree(&db, &created).await;
    scrub_fixture_extras(&db, fixture).await;
    cleanup(&db, fixture, &created).await;
    resume_scenario(result);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn forced_error_rolls_back_closure_and_publishes_nothing() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let mut created = Vec::new();
    let result = catch_scenario(async {
        set_home(&db, &sde, character).await;
        let system = sde.map.systems().next().unwrap().solar_system_id;
        let hull = hull_type(&sde);
        let box_kind = container_type(&sde);
        let module_kind = module_type(&sde);
        let charge_kind = charge_type(&sde);
        let root = creation::positioned(&db, hull, character, system, [0.0; 3]).await.unwrap();
        created.push(root);
        let outer = item_in(&db, box_kind, character, root, FLAG_CARGO, 1, true).await;
        let module = item_in(&db, module_kind, character, outer, FLAG_CARGO, 1, true).await;
        created.extend([outer, module]);
        db.execute("INSERT INTO game.ship_dirt (ship_item_id, dirt_timestamp) VALUES ($1, 7)", &[&root]).await.unwrap();
        db.execute("INSERT INTO game.item_attributes (item_id, attribute_id, value) VALUES ($1,$2,3.0)", &[&outer, &TEST_ATTR]).await.unwrap();
        item_combat::apply_loads(&db, root, &[(module, FLAG_FITTING_FIRST, charge_kind, 5, 0)]).await.unwrap();
        let doomed = pending_reloads::insert(&db, character, root, module, FLAG_FITTING_FIRST, charge_kind, 2, FLAG_CARGO, outer).await.unwrap();
        crate::dao::insurance::insure(&db, root, character, hull, 1.0, 1, i64::MAX).await.unwrap();
        selection::set_ship(&db, character, root).await.unwrap();
        db.execute("UPDATE game.characters SET solar_system_id=$2, station_id=NULL WHERE character_id=$1", &[&character, &system]).await.unwrap();
        let before = item_rows(&db, &[root, outer, module]).await;
        let identity = Identity { char_id: character, corp_id: 1, ship_id: Some(root), station_id: None, system_id: Some(system) };
        let input = loss_input(root, character, hull);
        let sde2 = sde.clone();
        let captured = Arc::new(std::sync::Mutex::new((Vec::new(), Vec::new()))); let captured_tx = captured.clone();
        let result: Result<Option<LossOutcome>, CallError> = db.transaction(
            move |tx, outbox: &mut Outbox<OnlineChanged>| {
                let sde2 = sde2.clone();
                let input = input.clone();
                let captured_tx = captured_tx.clone();
                Box::pin(async move {
                    assert!(destroy_ship_in_transaction(tx, outbox, &identity, &sde2, &input, None, &[]).await?.is_some());
                    let jobs = capture_recovery_jobs(tx, character).await; *captured_tx.lock().unwrap() = jobs;
                    Err(db_err(crate::db::Error::InvalidItemLockRequest("forced post-loss rollback")))
                })
            },
            |_v: Option<LossOutcome>, _o| async move { panic!("dispatch must not run for a rolled-back loss") },
        ).await;
        match result {
            Err(CallError::Internal(e)) => assert!(e.chain().any(|c| matches!(c.downcast_ref::<crate::db::Error>(), Some(crate::db::Error::InvalidItemLockRequest("forced post-loss rollback")))), "expected forced rollback error, got {e:?}"),
            other => panic!("expected Internal, got {}", other.err().map_or("success".to_string(), |e| format!("{e:?}"))),
        }
        let captured = captured.lock().unwrap().clone();
        assert_recovery_jobs_rolled_back(&db, &captured).await;
        assert_eq!(item_rows(&db, &[root, outer, module]).await, before);
        assert_eq!(pending_ids(&db, &[doomed]).await.len(), 1);
        assert!(db.query_opt("SELECT 1 FROM game.ship_dirt WHERE ship_item_id=$1", &[&root]).await.unwrap().is_some());
        assert!(db.query_opt("SELECT 1 FROM game.item_attributes WHERE item_id=$1", &[&outer]).await.unwrap().is_some());
        assert!(db.query_opt("SELECT 1 FROM game.module_charges WHERE ship_id=$1", &[&root]).await.unwrap().is_some());
        assert!(db.query_opt("SELECT 1 FROM game.insurance WHERE ship_id=$1", &[&root]).await.unwrap().is_some());
        assert!(db.query_opt("SELECT 1 FROM game.killmails WHERE victim_id=$1 AND victim_item_id=$2", &[&character, &root]).await.unwrap().is_none());
        assert!(db.query_opt("SELECT 1 FROM game.progress_events WHERE kind='ShipLost' AND subject_character_id=$1", &[&character]).await.unwrap().is_none());
        assert_no_recovery(&db, character, &[root, outer, module]).await;
        assert_eq!(selection::ship_id(&db, character).await.unwrap(), Some(root));
    }).await;
    remove_tree(&db, &created).await;
    scrub_fixture_extras(&db, fixture).await;
    cleanup(&db, fixture, &created).await;
    resume_scenario(result);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn repeated_loss_is_idempotent_and_missing_root_is_noop() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let mut created = Vec::new();
    let result = catch_scenario(async {
        set_home(&db, &sde, character).await;
        let system = sde.map.systems().next().unwrap().solar_system_id;
        let hull = hull_type(&sde);
        let root = creation::positioned(&db, hull, character, system, [0.0; 3]).await.unwrap();
        created.push(root);
        selection::set_ship(&db, character, root).await.unwrap();
        db.execute("UPDATE game.characters SET solar_system_id=$2, station_id=NULL WHERE character_id=$1", &[&character, &system]).await.unwrap();
        let first = piloted_loss(&db, &sde, character, root, system, hull, &[]).await.unwrap();
        assert!(first.is_some());
        let capsule = selection::ship_id(&db, character).await.unwrap().unwrap();
        assert_ne!(capsule, root);
        // The second settlement cannot produce another capsule, killmail or payout.
        let second = piloted_loss(&db, &sde, character, root, system, hull, &[]).await.unwrap();
        assert!(second.is_none(), "repeat settlement is a no-op");
        assert_eq!(selection::ship_id(&db, character).await.unwrap(), Some(capsule));
        assert_eq!(db.query("SELECT kill_id FROM game.killmails WHERE victim_id=$1 AND victim_item_id=$2", &[&character, &root]).await.unwrap().len(), 1);
        // A root that never existed has no closure and no cleanup work.
        let missing: i64 = db.query_one("SELECT (SELECT max(item_id) FROM game.items) + 1000000 AS id", &[]).await.unwrap().get("id");
        assert!(lock_loss_items(&db, &sde, missing, None).await.unwrap().is_none());
        crate::dao::items::ship::delete_lost(&db, missing, &[]).await.unwrap();
    }).await;
    remove_tree(&db, &created).await;
    scrub_fixture_extras(&db, fixture).await;
    cleanup(&db, fixture, &created).await;
    resume_scenario(result);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn descendants_terminate_on_containment_cycle() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let mut created = Vec::new();
    let mut clone_id = None;
    let result = catch_scenario(async {
        let owner: i64 = db.query_one("SELECT nextval('game.items_item_id_seq') AS id", &[]).await.unwrap().get("id");
        let a = item_in(&db, 1, owner, 1, 0, 1, true).await;
        created.push(a);
        let b = item_in(&db, 1, owner, a, 0, 1, true).await;
        created.push(b);
        let c = item_in(&db, 1, owner, b, 0, 1, true).await;
        created.push(c);
        db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&a, &c]).await.unwrap();
        let closure = tokio::time::timeout(Duration::from_secs(30), inventory::descendants(&db, &[a])).await.expect("cycle must not hang").unwrap();
        assert_eq!(closure.iter().copied().collect::<BTreeSet<_>>(), [a, b, c].into_iter().collect());
        // A dormant clone subtree stays outside the active closure.
        let custody = crate::dao::characters::clones::create(&db, owner, a).await.unwrap();
        clone_id = Some((custody.clone_id, owner));
        let clone: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,brain_slot,jump_clone_id) VALUES (1,$1,$1,$2,1,true,1,$3) RETURNING item_id", &[&owner, &FLAG_IMPLANT, &custody.clone_id]).await.unwrap().get("item_id");
        created.push(clone);
        let dormant = item_in(&db, 1, owner, clone, 0, 1, false).await;
        created.push(dormant);
        let closure = inventory::descendants(&db, &[a]).await.unwrap();
        assert!(!closure.contains(&clone) && !closure.contains(&dormant));
    }).await;
    remove_tree(&db, &created).await;
    if let Some((id, owner)) = clone_id {
        crate::dao::characters::clones::destroy(&db, id, owner).await.unwrap();
    }
    resume_scenario(result);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn hull_locks_precede_older_nested_nonhull() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let mut created = Vec::new();
    let result = catch_scenario(async {
        let system = sde.map.systems().next().unwrap().solar_system_id;
        let hull = hull_type(&sde);
        let module_kind = module_type(&sde);
        // Oldest row first: the non-hull cargo precedes both hull ids so a
        // naive ascending-id acquisition would take N before H.
        let cargo = item_in(&db, module_kind, character, 1, FLAG_CARGO, 3, false).await;
        let nested = item_in(&db, hull, character, 1, FLAG_CARGO, 1, true).await;
        let root = creation::positioned(&db, hull, character, system, [0.0; 3]).await.unwrap();
        db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&nested, &root]).await.unwrap();
        db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&cargo, &nested]).await.unwrap();
        created.extend([root, nested, cargo]);
        assert!(cargo < nested && nested < root);
        let mut a = connection().await;
        let holder = a.transaction().await.unwrap();
        // Transaction A holds the nested hull: loss preparation must acquire
        // it before the older non-hull cargo row, so B waits on H, not N.
        holder.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&nested]).await.unwrap();
        let holder_pid = crate::dao::items::parent_locks::tests::backend_pid(&holder).await;
        let db2 = db.clone();
        let sde2 = sde.clone();
        let contender = tokio::spawn(async move {
            let mut b = connection().await;
            let contender_tx = b.transaction().await.unwrap();
            let locks = lock_loss_items(&contender_tx, &sde2, root, None).await.unwrap().unwrap();
            assert!(locks.is_write_locked(root) && locks.is_write_locked(nested) && locks.is_write_locked(cargo));
            contender_tx.rollback().await.unwrap();
        });
        wait_blocked(&db2, holder_pid).await;
        // While the loss waits on the nested hull, the older non-hull cargo
        // row remains lockable: acquisition took H before N.
        let probe = connection().await;
        assert_eq!(probe.query("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE NOWAIT", &[&cargo]).await.unwrap().len(), 1);
        holder.rollback().await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), contender).await.unwrap().unwrap();
    }).await;
    remove_tree(&db, &created).await;
    scrub_fixture_extras(&db, fixture).await;
    cleanup(&db, fixture, &created).await;
    resume_scenario(result);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn pending_insert_between_capture_and_lock_retries_whole_loss_once() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let mut created = Vec::new();
    let result = catch_scenario(async {
        set_home(&db, &sde, character).await;
        let system = sde.map.systems().next().unwrap().solar_system_id;
        let hull = hull_type(&sde);
        let module_kind = module_type(&sde);
        let charge_kind = charge_type(&sde);
        let root = creation::positioned(&db, hull, character, system, [0.0; 3]).await.unwrap();
        created.push(root);
        let module = item_in(&db, module_kind, character, root, FLAG_FITTING_FIRST, 1, true).await;
        let source = item_in(&db, charge_kind, character, root, FLAG_CARGO, 20, false).await;
        created.extend([module, source]);
        selection::set_ship(&db, character, root).await.unwrap();
        db.execute("UPDATE game.characters SET solar_system_id=$2, station_id=NULL WHERE character_id=$1", &[&character, &system]).await.unwrap();
        // Holder keeps the root hull row UPDATE-locked. The loss transaction
        // finishes its unlocked closure/reservation capture first, then blocks
        // on the very first item lock, which is the only hull in this fixture.
        let mut holder_client = connection().await;
        let holder = holder_client.transaction().await.unwrap();
        holder.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&root]).await.unwrap();
        let holder_pid = crate::dao::items::parent_locks::tests::backend_pid(&holder).await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let identity = Identity { char_id: character, corp_id: 1, ship_id: Some(root), station_id: None, system_id: Some(system) };
        let input = loss_input(root, character, hull);
        let sde2 = sde.clone();
        let db2 = db.clone();
        let loss = tokio::spawn(async move {
            db2.transaction(
                move |tx, outbox: &mut Outbox<OnlineChanged>| {
                    count.fetch_add(1, Ordering::SeqCst);
                    let sde3 = sde2.clone();
                    let input = input.clone();
                    Box::pin(async move { destroy_ship_in_transaction(tx, outbox, &identity, &sde3, &input, None, &[]).await })
                },
                |outcome, _outbox| async move { Ok::<_, CallError>(outcome) },
            ).await
        });
        // wait_blocked proves the loss backend sits inside its first item lock,
        // so its unlocked capture already ran without the drift reservation.
        wait_blocked(&db, holder_pid).await;
        let drift = pending_reloads::insert(&db, character, root, module, FLAG_FITTING_FIRST, charge_kind, 4, FLAG_CARGO, source).await.unwrap();
        holder.rollback().await.unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(60), loss).await.unwrap().unwrap();
        assert!(outcome.unwrap().is_some());
        assert_eq!(attempts.load(Ordering::SeqCst), 2, "the whole loss transaction ran exactly twice");
        assert!(pending_ids(&db, &[drift]).await.is_empty(), "the committed attempt consumed the drift reservation");
        assert_custody(&db, character, &[(root, hull, 0), (module, module_kind, FLAG_FITTING_FIRST)]).await;
        assert!(item_rows(&db, &[source]).await.is_empty());
        assert_eq!(db.query("SELECT kill_id FROM game.killmails WHERE victim_id=$1 AND victim_item_id=$2", &[&character, &root]).await.unwrap().len(), 1);
        let capsules = db.query("SELECT item_id FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&character, &TYPE_CAPSULE]).await.unwrap();
        assert_eq!(capsules.len(), 1, "exactly one capsule was created");
        assert_eq!(db.query("SELECT event_id FROM game.progress_events WHERE kind='ShipLost' AND subject_character_id=$1", &[&character]).await.unwrap().len(), 1);
    }).await;
    remove_tree(&db, &created).await;
    scrub_fixture_extras(&db, fixture).await;
    cleanup(&db, fixture, &created).await;
    resume_scenario(result);
}
