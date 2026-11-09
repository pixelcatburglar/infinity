//! Requires a dedicated quiet test database: whole-database fingerprints intentionally prove no side effects anywhere.
use super::*;
use infinity_core::{consts::{CATEGORY_SHIP, dogma::{ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE}}};
use crate::{dao::items::{creation, pending_reloads}, space::World};
use std::sync::Arc;

mod shared_bays;

async fn fingerprint(db: &Db, ship: i64) -> Vec<(String, String)> {
    db.query("SELECT kind, hash FROM (SELECT 'items' AS kind, md5(row_to_json(i)::text) AS hash FROM game.items i WHERE item_id = $1 OR location_id = $1 UNION ALL SELECT 'attributes', md5(row_to_json(a)::text) FROM game.item_attributes a WHERE item_id = $1 UNION ALL SELECT 'insurance', md5(row_to_json(i)::text) FROM game.insurance i WHERE ship_id = $1 UNION ALL SELECT 'reloads', md5(row_to_json(r)::text) FROM game.pending_reloads r WHERE ship_id = $1) rows ORDER BY kind, hash", &[&ship]).await.unwrap().iter().map(|r| (r.get("kind"), r.get("hash"))).collect()
}

async fn database_fingerprint(db: &Db) -> Vec<(String, i64, Vec<(String, String)>)> {
    let mut result = Vec::new();
    for (table, key) in [
        ("accounts", "user_id"), ("characters", "character_id"), ("items", "item_id"),
        ("item_attributes", "item_id, attribute_id"), ("insurance", "ship_id"),
        ("pending_reloads", "reload_id"),
    ] {
        let count: i64 = db.query_one(&format!("SELECT count(*) AS n FROM game.{table}"), &[]).await.unwrap().get("n");
        let rows = db.query(&format!("SELECT json_build_array({key})::text AS key, md5(row_to_json(t)::text) AS hash FROM game.{table} t ORDER BY {key}"), &[]).await.unwrap()
            .iter().map(|row| (row.get("key"), row.get("hash"))).collect();
        result.push((table.to_owned(), count, rows));
    }
    result
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn persistent_damage_repair_guard_and_death() {
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let baseline = database_fingerprint(&db).await;
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE].iter().all(|id| t.attribute(*id).is_some_and(|v| v.is_finite() && v > 0.0))).unwrap().type_id;
    let system = 1_i64;
    let (account, owner, ship) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('hull-health-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'hull-health-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
        let ship = creation::positioned(tx, kind, owner, system, [0.0; 3]).await?;
        pending_reloads::insert(tx, owner, ship, ship, 1, kind, 1, infinity_core::consts::FLAG_CARGO, ship).await?;
        crate::dao::insurance::insure(tx, ship, owner, kind, 1.0, 1, 2).await?;
        Ok::<_, crate::db::Error>((account, owner, ship))
    }), |v, ()| async move { Ok(v) }).await.unwrap();
    let world = World::new(db.clone(), sde, 1);
    let w = world.clone();
    let d = db.clone();
    let result = tokio::spawn(async move {
        w.system(system).await.unwrap();
        assert!(w.ball(system, ship).await.is_some());
        let sampled = w.ball(system, ship).await.unwrap();
        assert!(sampled.char_id.is_none());
        let seed = w.with(system, move |s| s.combat_condition_snapshot(ship).unwrap().condition).await.unwrap();
        let fit = Fit::for_space_type(w.sde().clone(), kind, Vec::new());
        w.with(system, move |s| {
            let mut ball = s.take(ship).unwrap();
            ball.char_id = Some(owner);
            s.add_ship(ball);
            s.install_condition(ship, seed, fit);
        }).await.unwrap();
        let before_npc_repair = w.with(system, move |s| s.combat_health_scalars(ship).unwrap()).await.unwrap();
        damage::repair_target(&w, system, ship, 1, 1.0, crate::space::combat::RepairSource::Local, move |s| s.is_npc(ship)).await.unwrap();
        let after_npc_repair = w.with(system, move |s| s.combat_health_scalars(ship).unwrap()).await.unwrap();
        assert_eq!(after_npc_repair.0, before_npc_repair.0);
        assert_eq!(after_npc_repair.1[1..], before_npc_repair.1[1..]);
        assert!(damage::apply_damage_guarded(&d, &w, system, ship, [1e30, 0.0, 0.0, 0.0], Default::default(),
            false, move |s| s.is_npc(ship)).await.unwrap().is_none());
        assert!(w.ball(system, ship).await.is_some());
        let pilot_generation = crate::space::targeting::next_generation();
        let pilot_facts = w.with(system, move |s| s.prepare_combat_source(ship, None, None, None, crate::lua::events::resources::FactOrigin::Natural, pilot_generation, 1, 1, |_| true)).await.unwrap().unwrap();
        assert_eq!(damage::apply_damage_guarded(&d, &w, system, ship, [0.0; 4], damage::DamageSource { facts: Some(pilot_facts), ..Default::default() }, false, |_| true).await.unwrap().unwrap().dealt, 0.0);
        w.with(system, move |s| s.retire_combat_source(pilot_generation).unwrap()).await.unwrap();
        assert!(w.ball(system, ship).await.unwrap().char_id.is_some());
        let seed = w.with(system, move |s| s.combat_condition_snapshot(ship).unwrap().condition).await.unwrap();
        let fit = Fit::for_space_type(w.sde().clone(), kind, Vec::new());
        w.with(system, move |s| {
            let mut ball = s.take(ship).unwrap();
            ball.char_id = None;
            s.add_persistent_ship(ball);
            s.install_condition(ship, seed, fit);
        }).await.unwrap();
        let generation = crate::space::targeting::next_generation();
        let facts = w.with(system, move |s| s.prepare_combat_source(ship, None, None, None, crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true)).await.unwrap().unwrap();
        assert!(damage::apply_damage_guarded(&d, &w, system, ship, [1.0, 0.0, 0.0, 0.0], damage::DamageSource { facts: Some(facts), ..Default::default() }, false, |_| true).await.unwrap().unwrap().dealt > 0.0);
        let before_guard = fingerprint(&d, ship).await;
        let health_guard = w.ball(system, ship).await.unwrap().health;
        damage::repair_target(&w, system, ship, 0, 1.0, crate::space::combat::RepairSource::Local, |_| false).await.unwrap();
        assert_eq!(fingerprint(&d, ship).await, before_guard);
        assert_eq!(w.ball(system, ship).await.unwrap().health, health_guard);
        let initial = fingerprint(&d, ship).await;
        let before_damage = w.ball(system, ship).await.unwrap().health;
        let facts = w.with(system, move |s| s.prepare_combat_source(ship, None, None, None, crate::lua::events::resources::FactOrigin::Natural, generation, 2, 1, |_| true)).await.unwrap().unwrap();
        assert!(damage::apply_damage_guarded(&d, &w, system, ship, [1.0, 0.0, 0.0, 0.0], damage::DamageSource { facts: Some(facts), ..Default::default() }, false, |_| true).await.unwrap().unwrap().dealt > 0.0);
        assert_eq!(fingerprint(&d, ship).await, initial);
        assert!(w.ball(system, ship).await.unwrap().health[0] < before_damage[0]);
        w.flush_positions().await.unwrap();
        let damaged = fingerprint(&d, ship).await;
        assert_ne!(initial, damaged);
        assert!(d.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.unwrap().is_empty());
        damage::repair_target(&w, system, ship, 0, 1.0, crate::space::combat::RepairSource::Local, |_| true).await.unwrap();
        assert_eq!(fingerprint(&d, ship).await, damaged);
        w.flush_positions().await.unwrap();
        let repaired = fingerprint(&d, ship).await;
        assert_ne!(damaged, repaired);
        d.execute("UPDATE game.items SET owner_id = $2 WHERE item_id = $1", &[&ship, &(owner + 1)]).await.unwrap();
        let before_refusal = fingerprint(&d, ship).await;
        let before_health = w.ball(system, ship).await.unwrap().health;
        let before_condition = w.with(system, move |s| s.combat_condition_snapshot(ship).unwrap().condition.now).await.unwrap();
        let facts = w.with(system, move |s| s.prepare_combat_source(ship, None, None, None, crate::lua::events::resources::FactOrigin::Natural, generation, 3, 1, |_| true)).await.unwrap().unwrap();
        assert!(damage::apply_damage_guarded(&d, &w, system, ship, [1e30, 0.0, 0.0, 0.0], damage::DamageSource { facts: Some(facts), ..Default::default() }, false, |_| true).await.is_err());
        assert_eq!(fingerprint(&d, ship).await, before_refusal);
        assert!(matches!(w.with(system, move |s| s.combat_phase(ship)).await.flatten(), Some(crate::space::combat::CombatPhase::Dying { .. })));
        assert!(d.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.unwrap().is_empty());
        d.execute("UPDATE game.items SET owner_id = $2 WHERE item_id = $1", &[&ship, &owner]).await.unwrap();
        let wait = std::time::Duration::from_secs(crate::config::game().space.position_snapshot_secs as u64 * 2 + 5);
        tokio::time::timeout(wait, async {
            loop {
                if w.with(system, move |s| s.combat_phase(ship)).await.flatten() == Some(crate::space::combat::CombatPhase::Live) { break; }
                assert!(w.after(system, 100).await);
            }
        }).await.expect("pending loss reconciliation did not restore the hull");
        let restored = w.with(system, move |s| s.combat_condition_snapshot(ship).unwrap().condition.now).await.unwrap();
        assert_eq!(restored[1..], before_condition[1..]);
        assert!(restored[0] >= before_condition[0], "shield may recharge while reconciliation is pending");
        let restored_health = w.ball(system, ship).await.unwrap().health;
        assert_eq!(restored_health[1..], before_health[1..]);
        assert!(restored_health[0] >= before_health[0]);
        assert_eq!(fingerprint(&d, ship).await, repaired);
        let before_rollback = fingerprint(&d, ship).await;
        let snapshot = w.with(system, move |s| {
            let facts = s.prepare_combat_source(ship, None, None, None, crate::lua::events::resources::FactOrigin::Natural, generation, 4, 1, |_| true).unwrap();
            s.apply_damage(ship, [1e30, 0.0, 0.0, 0.0], damage::DamageSource { facts: Some(facts), ..Default::default() }, false).unwrap().unwrap().first_lethal.unwrap()
        }).await.unwrap();
        let failed: Result<(), CallError> = d.transaction(|tx, _: &mut Outbox| {
            let snapshot = Arc::clone(&snapshot);
            let sde = w.sde().clone();
            Box::pin(async move {
            assert!(destroy_in(tx, &sde, system, &snapshot).await?);
            assert!(tx.query_opt("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.map_err(crate::db::Error::from).map_err(db_err)?.is_some());
            tx.query_one("SELECT 1 / 0 AS failure", &[]).await.map_err(crate::db::Error::from).map_err(db_err)?;
            Ok(())
        }) }, |value, _| async move { Ok(value) }).await;
        assert!(failed.is_err());
        assert_eq!(fingerprint(&d, ship).await, before_rollback);
        assert!(d.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.unwrap().is_empty());
        let (token, membership_generation) = (snapshot.token, snapshot.generation);
        assert!(w.with(system, move |s| s.restore_known_rollback(ship, token, membership_generation)).await.unwrap());
        assert_eq!(w.ball(system, ship).await.unwrap().health[1..], before_health[1..]);
        let facts = w.with(system, move |s| s.prepare_combat_source(ship, None, None, None, crate::lua::events::resources::FactOrigin::Natural, generation, 5, 1, |_| true)).await.unwrap().unwrap();
        assert!(damage::apply_damage_guarded(&d, &w, system, ship, [1e30, 0.0, 0.0, 0.0], damage::DamageSource { facts: Some(facts), ..Default::default() }, false, |_| true).await.unwrap().unwrap().lethal);
        w.with(system, move |s| s.retire_combat_source(generation).unwrap()).await.unwrap();
        assert!(w.ball(system, ship).await.is_none());
        let events = d.query("SELECT solar_system_id, kind FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].get::<_, i64>("solar_system_id"), events[0].get::<_, i16>("kind")), (system, crate::dao::map_telemetry::EventKind::Ship as i16));
        assert!(damage::apply_damage_guarded(&d, &w, system, ship, [1e30, 0.0, 0.0, 0.0], Default::default(), false, |_| true).await.unwrap().is_none());
        assert_eq!(d.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.unwrap().len(), 1);
        assert!(damage::repair_target(&w, system, ship, 2, 1e30, crate::space::combat::RepairSource::Local, |_| true).await.is_err());
        assert!(w.ball(system, ship).await.is_none());
        assert!(fingerprint(&d, ship).await.is_empty());
        assert!(!crate::dao::items::space_ships::unpiloted(&d, system).await.unwrap().iter().any(|row| row.item_id == ship));
    }).await;
    world.remove_ball(system, ship).await;
    forget_ship(ship);
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await?;
        tx.execute("DELETE FROM game.pending_reloads WHERE ship_id = $1", &[&ship]).await?;
        tx.execute("DELETE FROM game.insurance WHERE ship_id = $1", &[&ship]).await?;
        tx.execute("DELETE FROM game.items WHERE item_id = $1 OR location_id = $1", &[&ship]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    assert_eq!(database_fingerprint(&db).await, baseline);
    result.unwrap();
}

