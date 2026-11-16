//! Nested active-contents destruction through the real unpiloted loss path, its
//! post-capture snapshot conflicts and the concurrent completion race. These
//! regressions run against the shared World and a dedicated test database; they
//! create no killmail, capsule or reward.
use super::*;
use crate::db::outbox::Outbox;
use crate::dao::items::{creation, loading::{self, ItemQuery}, pending_reloads, transfers};
use crate::dao::items::parent_locks::tests::{backend_pid, wait_blocked};
use crate::lua::events::{combat::{CombatEntity, ConflictKind}, resources::{FactContext, FactOrigin}};
use crate::space::combat::FirstLethalSnapshot;
use crate::services::combat::groups_health::{Condition, ShipStats};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

struct Parts {
    foreign: i64,
    hull: i64,
    outer: i64,
    middle: i64,
    module: i64,
    cargo: i64,
    charge_type: i32,
    sentinel_source: i64,
    sentinel_module: i64,
    reservation: i64,
    survivor_reservation: i64,
    doomed_sourced: i64,
}
/// Rooted deep tree: hull -> outer -> middle -> nested hull -> module + cargo,
/// with a foreign-owned module inside the nested hull. A survivor sibling hull
/// carries its own weapon/source reservation; a doomed reservation sourced on
/// the survivor is consumed while one doomed-sourced reservation is consumed.
async fn nested_parts(f: &crate::services::drones::tests::fixture::Fixture) -> Parts {
    let (module_type, charge_type) = super::super::targeting_charges::tests::compatible_ammunition(&f.sde);
    let container = f.sde.types.iter().find(|t| t.published == Some(true) && t.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let account = f.account;
    let foreign: i64 = f.db.transaction(|tx, _: &mut ()| Box::pin(async move {
        Ok::<_, crate::db::Error>(tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'nested-loss-foreign-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id"))
    }), |owner, ()| async move { Ok(owner) }).await.unwrap();
    let flag = infinity_core::consts::FLAG_FITTING_FIRST;
    let outer = transfers::insert(&f.db, container, f.owner, f.ship, FLAG_CARGO, 1).await.unwrap();
    let middle = transfers::insert(&f.db, container, foreign, outer, FLAG_CARGO, 1).await.unwrap();
    let hull = creation::rookie_ship(&f.db, f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id, f.owner, middle, FLAG_CARGO).await.unwrap();
    let module = creation::rookie_ship(&f.db, module_type, foreign, hull, flag).await.unwrap();
    let cargo = transfers::insert(&f.db, charge_type, foreign, hull, FLAG_CARGO, 5).await.unwrap();
    f.db.execute("UPDATE game.items SET singleton=true WHERE item_id=ANY($1)", &[&&[outer, middle][..]]).await.unwrap();
    // The survivor sibling hull is positioned space content outside the doomed
    // tree's contents closure, so its rows fingerprint must survive unchanged.
    let sentinel_source = creation::positioned(&f.db, container, f.owner, f.system, [2.0, 0.0, 0.0]).await.unwrap();
    f.db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&sentinel_source]).await.unwrap();
    transfers::insert(&f.db, charge_type, f.owner, sentinel_source, FLAG_CARGO, 3).await.unwrap();
    // An unrelated reservation: weapon module fitted on the survivor hull,
    // source stack inside it. It must survive destruction untouched.
    let sentinel_module = creation::rookie_ship(&f.db, module_type, f.owner, sentinel_source, flag).await.unwrap();
    let survivor_reservation = pending_reloads::insert(&f.db, f.owner, sentinel_source, sentinel_module, flag, charge_type, 1, FLAG_CARGO, sentinel_source).await.unwrap();
    // Attachments on deep members: overrides, dirt, skin licence+application,
    // configuration, dynamic provenance, blueprint state, insurance, charges,
    // banks and doomed reservations.
    f.db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,$3)", &[&module, &infinity_core::consts::dogma::ATTR_CAPACITY, &12.5]).await.unwrap();
    f.db.execute("INSERT INTO game.ship_configuration (ship_item_id,owner_id) VALUES ($1,$2)", &[&hull, &f.owner]).await.unwrap();
    f.db.execute("INSERT INTO game.ship_dirt (ship_item_id,dirt_timestamp) VALUES ($1,$2)", &[&hull, &7_i64]).await.unwrap();
    f.db.execute("INSERT INTO game.ship_skin_licenses (character_id,skin_id) VALUES ($1,$2) ON CONFLICT DO NOTHING", &[&f.owner, &424242_i32]).await.unwrap();
    f.db.execute("INSERT INTO game.ship_skin_applications (ship_item_id,character_id,skin_id) VALUES ($1,$2,$3)", &[&hull, &f.owner, &424242_i32]).await.unwrap();
    f.db.execute("INSERT INTO game.dynamic_items (item_id,source_item_id,mutator_item_id,source_type_id,mutator_type_id,result_type_id,creator_id,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        &[&module, &module, &module, &module_type, &module_type, &module_type, &f.owner, &1_i64]).await.unwrap();
    f.db.execute("INSERT INTO game.dynamic_item_modifiers (item_id,attribute_id,factor) VALUES ($1,$2,$3)", &[&module, &infinity_core::consts::dogma::ATTR_CAPACITY, &1.5]).await.unwrap();
    f.db.execute("INSERT INTO game.blueprints (item_id,runs) VALUES ($1,$2)", &[&cargo, &3_i32]).await.unwrap();
    crate::dao::insurance::insure(&f.db, hull, f.owner, 0, 1.0, 1, 2).await.unwrap();
    crate::dao::insurance::insure(&f.db, f.ship, f.owner, 0, 1.0, 1, 2).await.unwrap();
    crate::dao::items::combat::apply_loads(&f.db, f.ship, &[(module, flag, charge_type, 4, 0)]).await.unwrap();
    f.db.execute("INSERT INTO game.weapon_banks (ship_id,master_id,slave_id) VALUES ($1,$2,$3)", &[&hull, &module, &module]).await.unwrap();
    let reservation = pending_reloads::insert(&f.db, f.owner, f.ship, module, flag, charge_type, 2, FLAG_CARGO, sentinel_source).await.unwrap();
    let doomed_sourced = pending_reloads::insert(&f.db, f.owner, f.ship, module, flag, charge_type, 1, FLAG_CARGO, cargo).await.unwrap();
    // Historical and licensed sentinels that must survive destruction.
    f.db.execute("INSERT INTO game.char_notes (char_id,item_id,note) VALUES ($1,$2,$3)", &[&f.owner, &module, &"deep note".to_owned()]).await.unwrap();
    Parts { foreign, hull, outer, middle, module, cargo, charge_type, sentinel_source, sentinel_module, reservation, survivor_reservation, doomed_sourced }
}

/// Rows under an item root, including reservation rows that reference it;
/// `exclude_reloads` drops consumed reservations from the survivor fingerprint.
async fn tree_rows(db: &Db, root: i64, exclude_reloads: &[i64]) -> Vec<(String, String)> {
    db.query("SELECT kind, hash FROM (SELECT 'items' AS kind, md5(row_to_json(i)::text) AS hash FROM game.items i WHERE i.item_id = $1 OR i.location_id = $1 UNION ALL SELECT 'attributes', md5(row_to_json(a)::text) FROM game.item_attributes a WHERE a.item_id = $1 UNION ALL SELECT 'reloads', md5(row_to_json(r)::text) FROM game.pending_reloads r WHERE (r.ship_id = $1 OR r.source_location_id = $1) AND r.reload_id != ALL($2) UNION ALL SELECT 'charges', md5(row_to_json(c)::text) FROM game.module_charges c WHERE c.ship_id = $1) rows ORDER BY kind, hash", &[&root, &exclude_reloads]).await.unwrap().iter().map(|r| (r.get("kind"), r.get("hash"))).collect()
}

fn snapshot(ship: i64, owner: i64, kind: i32) -> FirstLethalSnapshot {
    let victim = CombatEntity { item_id: ship, type_id: kind, incarnation: [ship as u8; 16], character_id: None,
        corporation_id: None, alliance_id: None, faction_id: None };
    FirstLethalSnapshot { token: 9, item_id: ship, condition: Condition {
        stats: ShipStats { max: [1.0; 3], resonance: [[1.0; 4]; 3], shield_tau_ms: 0.0 },
        now: [0.0; 3], shield_at: 1 }, contributions: vec![], undo_contributions: vec![],
        source: Default::default(), position: [0.0; 3], identity: None, owner_id: owner, corp_id: 1,
        is_drone: false, is_npc: false, expected_online_session: None, kind: Some(kind), lethal_at: 11,
        generation: 3, before_revision: 1, victim, pending_damage: None,
        victim_context: FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: Some(ship), ship_type_id: Some(kind), system_id: Some(1), station_id: None, origin: FactOrigin::Natural },
        conflict: ConflictKind::Environment }
}

async fn destroy(db: &Db, sde: &Arc<Sde>, system: i64, snapshot: Arc<FirstLethalSnapshot>) -> Result<bool, CallError> {
    let pending = Arc::new(crate::services::ship::loss::facts::PlayerLossContext::new(None, [0.0; 3]));
    db.transaction(|tx, outbox: &mut Outbox| {
        let sde = sde.clone();
        let snapshot = Arc::clone(&snapshot);
        let pending = pending.clone();
        Box::pin(async move { destroy_in(tx, outbox, &sde, system, &snapshot, &pending, &[]).await })
    }, |value, _outbox| async move { Ok(value) }).await
}

async fn cleanup_parts(f: &crate::services::drones::tests::fixture::Fixture, parts: &Parts) {
    crate::services::tests::fixture::cleanup_recovery(&f.db, f.owner).await.unwrap();
    f.db.execute("DELETE FROM game.char_notes WHERE char_id=$1", &[&f.owner]).await.unwrap();
    f.db.execute("DELETE FROM game.ship_skin_applications WHERE ship_item_id=ANY($1)", &[&&[f.ship, parts.hull][..]]).await.unwrap();
    f.db.execute("DELETE FROM game.ship_skin_licenses WHERE character_id=$1 AND skin_id=$2", &[&f.owner, &424242_i32]).await.unwrap();
    f.db.execute("DELETE FROM game.insurance WHERE owner_id=$1", &[&f.owner]).await.unwrap();
    f.db.execute("DELETE FROM game.pending_reloads WHERE owner_id=$1", &[&f.owner]).await.unwrap();
    f.db.execute("DELETE FROM game.weapon_banks WHERE slave_id=ANY($1) OR master_id=ANY($1)", &[&&[f.ship, parts.hull, parts.module, parts.outer, parts.middle, parts.sentinel_source, parts.sentinel_module][..]]).await.unwrap();
    f.db.execute("DELETE FROM game.module_charges WHERE ship_id=ANY($1)", &[&&[f.ship, parts.hull, parts.sentinel_source][..]]).await.unwrap();
    f.db.execute("DELETE FROM game.ship_configuration WHERE ship_item_id=$1", &[&parts.hull]).await.unwrap();
    f.db.execute("DELETE FROM game.ship_dirt WHERE ship_item_id=$1", &[&parts.hull]).await.unwrap();
    f.db.execute("DELETE FROM game.dynamic_item_modifiers WHERE item_id=ANY($1)", &[&&[parts.module, parts.cargo][..]]).await.unwrap();
    f.db.execute("DELETE FROM game.dynamic_items WHERE item_id=ANY($1)", &[&&[parts.module, parts.cargo][..]]).await.unwrap();
    f.db.execute("DELETE FROM game.blueprints WHERE item_id=ANY($1)", &[&&[parts.cargo, parts.module][..]]).await.unwrap();
    f.db.execute("DELETE FROM game.item_attributes WHERE item_id=ANY($1)", &[&&[f.ship, parts.hull, parts.outer, parts.middle, parts.module, parts.cargo, parts.sentinel_source, parts.sentinel_module][..]]).await.unwrap();
    f.db.execute("DELETE FROM game.map_events WHERE solar_system_id=$1", &[&f.system]).await.unwrap();
    f.db.execute("DELETE FROM game.items WHERE owner_id=$1 OR location_id=ANY($2) OR item_id=ANY($2)", &[&f.owner, &&[f.ship, parts.hull, parts.outer, parts.middle, parts.sentinel_source, parts.sentinel_module][..]]).await.unwrap();
    f.db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&parts.foreign]).await.unwrap();
    f.db.transaction(|tx, _: &mut ()| {
        let owner = f.owner;
        Box::pin(async move { crate::services::tests::fixture::cleanup_progress(tx, &[owner], &[owner]).await })
    }, |(), ()| async move { Ok(()) }).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn deep_foreign_owned_tree_is_destroyed_without_capsule_or_resurrection() {
    let f = crate::services::drones::tests::fixture::Fixture::new().await;
    let parts = nested_parts(&f).await;
    let result = crate::tests::catch_scenario(async {
        f.db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&f.owner]).await.unwrap();
        let unrelated = tree_rows(&f.db, parts.sentinel_source, &[parts.reservation]).await;
        let killmails = f.db.query_one("SELECT count(*) AS n FROM game.killmails", &[]).await.unwrap().get::<_, i64>("n");
        let capsules = f.db.query("SELECT item_id FROM game.items WHERE type_id=$1 AND owner_id=ANY($2)", &[&infinity_core::consts::TYPE_CAPSULE, &&[f.owner, parts.foreign][..]]).await.unwrap().len();
        let lost = snapshot(f.ship, f.owner, f.world.ball(f.system, f.ship).await.unwrap().type_id);
        assert!(destroy(&f.db, &f.sde, f.system, Arc::new(lost.clone())).await.unwrap());
        let group: i64 = f.db.query_one("SELECT group_id FROM game.repairable_recovery_groups WHERE beneficiary_id=$1 AND source_item_id=$2", &[&f.owner, &f.ship]).await.unwrap().get("group_id");
        let root = loading::load_items(&f.db, ItemQuery::Id(f.ship)).await.unwrap();
        assert_eq!(root.len(), 1);
        assert_eq!((root[0].type_id, root[0].owner_id, root[0].location_id, root[0].flag_id, root[0].quantity, root[0].singleton), (lost.kind.unwrap(), infinity_core::consts::NPC_CORP_INTERBUS, group, 0, 1, true));
        assert_eq!(f.db.query("SELECT item_id FROM game.repairable_recovery_items WHERE group_id=$1", &[&group]).await.unwrap().iter().map(|r| r.get::<_, i64>("item_id")).collect::<Vec<_>>(), [f.ship]);
        for id in [parts.hull, parts.outer, parts.middle, parts.module, parts.cargo] {
            assert!(loading::load_items(&f.db, ItemQuery::Id(id)).await.unwrap().is_empty(), "closure member {id} survived");
        }
        for (table, sql) in [
            ("item_attributes", "SELECT count(*) AS n FROM game.item_attributes WHERE item_id=ANY($1)"),
            ("ship_configuration", "SELECT count(*) AS n FROM game.ship_configuration WHERE ship_item_id=ANY($1)"),
            ("ship_dirt", "SELECT count(*) AS n FROM game.ship_dirt WHERE ship_item_id=ANY($1)"),
            ("ship_skin_applications", "SELECT count(*) AS n FROM game.ship_skin_applications WHERE ship_item_id=ANY($1)"),
            ("dynamic_items", "SELECT count(*) AS n FROM game.dynamic_items WHERE item_id=ANY($1)"),
            ("dynamic_item_modifiers", "SELECT count(*) AS n FROM game.dynamic_item_modifiers WHERE item_id=ANY($1)"),
            ("blueprints", "SELECT count(*) AS n FROM game.blueprints WHERE item_id=ANY($1)"),
            ("insurance", "SELECT count(*) AS n FROM game.insurance WHERE ship_id=ANY($1)"),
            ("module_charges", "SELECT count(*) AS n FROM game.module_charges WHERE ship_id=ANY($1)"),
            ("weapon_banks", "SELECT count(*) AS n FROM game.weapon_banks WHERE ship_id=ANY($1) OR master_id=ANY($1) OR slave_id=ANY($1)"),
        ] {
            let ids = if matches!(table, "insurance" | "module_charges" | "weapon_banks") {
                vec![f.ship, parts.hull, parts.outer, parts.middle, parts.module, parts.cargo]
            } else { vec![parts.hull, parts.outer, parts.middle, parts.module, parts.cargo] };
            let count: i64 = f.db.query_one(sql, &[&ids]).await.unwrap().get("n");
            assert_eq!(count, 0, "{table} rows survived loss");
        }
        assert!(f.db.query("SELECT reload_id FROM game.pending_reloads WHERE reload_id=ANY($1)", &[&&[parts.reservation, parts.doomed_sourced][..]]).await.unwrap().is_empty());
        assert_eq!(f.db.query_one("SELECT count(*) AS n FROM game.pending_reloads WHERE reload_id=$1", &[&parts.survivor_reservation]).await.unwrap().get::<_, i64>("n"), 1, "the unrelated survivor reservation must not be touched");
        // Historical and licence sentinels survive; so does the unrelated sibling
        // tree — its reservation stays, the consumed doomed reservation is excluded.
        assert_eq!(f.db.query_one("SELECT count(*) AS n FROM game.ship_skin_licenses WHERE character_id=$1 AND skin_id=$2", &[&f.owner, &424242_i32]).await.unwrap().get::<_, i64>("n"), 1);
        assert_eq!(f.db.query_one("SELECT count(*) AS n FROM game.char_notes WHERE char_id=$1 AND item_id=$2", &[&f.owner, &parts.module]).await.unwrap().get::<_, i64>("n"), 1);
        assert_eq!(tree_rows(&f.db, parts.sentinel_source, &[parts.reservation]).await, unrelated);
        assert_eq!(f.db.query_one("SELECT count(*) AS n FROM game.killmails", &[]).await.unwrap().get::<_, i64>("n"), killmails);
        assert_eq!(f.db.query("SELECT item_id FROM game.items WHERE type_id=$1 AND owner_id=ANY($2)", &[&infinity_core::consts::TYPE_CAPSULE, &&[f.owner, parts.foreign][..]]).await.unwrap().len(), capsules);
        assert_eq!(f.db.query_one("SELECT count(*) AS n FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &f.ship]).await.unwrap().get::<_, i64>("n"), 1);
        // Repeated settlement is idempotent: no second map event, no resurrection.
        assert!(!destroy(&f.db, &f.sde, f.system, Arc::new(lost.clone())).await.unwrap());
        assert_eq!(f.db.query_one("SELECT count(*) AS n FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &f.ship]).await.unwrap().get::<_, i64>("n"), 1);
        // Delayed completion of a consumed reservation is exercised in the
        // targeting_charges nested-loss suite, which owns the private load type.
    }).await;
    cleanup_parts(&f, &parts).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn reservation_drift_retries_whole_loss_once_and_publishes_once() {
    let f = crate::services::drones::tests::fixture::Fixture::new().await;
    let parts = nested_parts(&f).await;
    let result = crate::tests::catch_scenario(async {
        f.db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&f.owner]).await.unwrap();
        let unrelated = tree_rows(&f.db, parts.sentinel_source, &[parts.reservation]).await;
        // A held item row makes the first attempt's initial item lock wait while
        // its unlocked capture has already run; inserting a reservation then is
        // genuine snapshot drift, never a sleep-guessed race.
        let config = crate::config::tests::test_database().to_pg_config().unwrap();
        let (mut holder_client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let holder = holder_client.transaction().await.unwrap();
        holder.execute("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&parts.cargo]).await.unwrap();
        let holder_pid = backend_pid(&holder).await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_body = attempts.clone();
        let db = f.db.clone();
        let sde = f.sde.clone();
        let lost = Arc::new(snapshot(f.ship, f.owner, f.world.ball(f.system, f.ship).await.unwrap().type_id));
        let system = f.system;
        let pending = Arc::new(crate::services::ship::loss::facts::PlayerLossContext::new(None, [0.0; 3]));
        let op = db.transaction(|tx, outbox: &mut Outbox| {
            attempts_body.fetch_add(1, Ordering::SeqCst);
            let sde = sde.clone();
            let snapshot = Arc::clone(&lost);
            let pending = pending.clone();
            Box::pin(async move { destroy_in(tx, outbox, &sde, system, &snapshot, &pending, &[]).await })
        }, |value, _outbox| async move { Ok(value) });
        let mut op = std::pin::pin!(op);
        // Drive the first attempt until it is provably blocked on the held cargo
        // row: its unlocked closure/reservation capture has completed by then,
        // so the insert below lands inside the capture-to-fence window.
        tokio::select! {
            outcome = tokio::time::timeout(Duration::from_secs(60), op.as_mut()) => {
                panic!("the held cargo row must block the loss attempt, got {outcome:?}");
            }
            blocked = tokio::time::timeout(Duration::from_secs(30), wait_blocked(&f.db, holder_pid)) => {
                blocked.expect("first attempt did not reach the held item fence");
            }
        }
        let inserted_id = pending_reloads::insert(&f.db, f.owner, f.ship, parts.module, infinity_core::consts::FLAG_FITTING_FIRST, parts.charge_type, 1, FLAG_CARGO, parts.cargo).await.unwrap();
        holder.rollback().await.unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(60), op).await.unwrap();
        assert!(outcome.unwrap());
        assert_eq!(attempts.load(Ordering::SeqCst), 2, "the drift must force exactly one whole-transaction retry");
        assert!(f.db.query("SELECT reload_id FROM game.pending_reloads WHERE reload_id=$1", &[&inserted_id]).await.unwrap().is_empty(), "the drift reservation must be consumed by the retry");
        assert_eq!(f.db.query_one("SELECT count(*) AS n FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &f.ship]).await.unwrap().get::<_, i64>("n"), 1);
        assert_eq!(tree_rows(&f.db, parts.sentinel_source, &[parts.reservation]).await, unrelated);
    }).await;
    cleanup_parts(&f, &parts).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn repeated_snapshot_conflict_exhausts_retry_and_publishes_nothing() {
    let f = crate::services::drones::tests::fixture::Fixture::new().await;
    let parts = nested_parts(&f).await;
    let result = crate::tests::catch_scenario(async {
        f.db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&f.owner]).await.unwrap();
        let before = tree_rows(&f.db, f.ship, &[]).await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_body = attempts.clone();
        let db = f.db.clone();
        let sde = f.sde.clone();
        let lost = Arc::new(snapshot(f.ship, f.owner, f.world.ball(f.system, f.ship).await.unwrap().type_id));
        let system = f.system;
        let pending = Arc::new(crate::services::ship::loss::facts::PlayerLossContext::new(None, [0.0; 3]));
        let outcome = db.transaction(|tx, outbox: &mut Outbox| {
            attempts_body.fetch_add(1, Ordering::SeqCst);
            let sde = sde.clone();
            let snapshot = Arc::clone(&lost);
            let pending = pending.clone();
            Box::pin(async move {
                assert!(destroy_in(tx, outbox, &sde, system, &snapshot, &pending, &[]).await?);
                Err::<bool, _>(db_err(crate::db::Error::StaleSnapshot("loss snapshot changed")))
            })
        }, |value, _outbox| async move { Ok(value) }).await;
        assert!(matches!(outcome, Err(CallError::Internal(_))), "exhausted retries surface Internal, never User");
        assert_eq!(attempts.load(Ordering::SeqCst), 2, "the bounded runner must attempt exactly twice");
        assert_eq!(tree_rows(&f.db, f.ship, &[]).await, before, "both attempts roll back completely");
        assert!(f.db.query("SELECT reload_id FROM game.pending_reloads WHERE reload_id=ANY($1)", &[&&[parts.reservation, parts.doomed_sourced][..]]).await.unwrap().len() == 2);
        assert!(f.db.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &f.ship]).await.unwrap().is_empty());
    }).await;
    cleanup_parts(&f, &parts).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
