//! Requires a dedicated quiet test database: ship-scoped fingerprints and owned-row absence checks prove no side effects.
use super::*;
use infinity_core::{consts::{CATEGORY_SHIP, dogma::{ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE}}};
use crate::{dao::items::{creation, pending_reloads}, space::World};
use std::sync::Arc;

mod nested_contents;
mod shared_bays;

async fn fingerprint(db: &Db, ship: i64) -> Vec<(String, String)> {
    db.query("SELECT kind, hash FROM (SELECT 'items' AS kind, md5(row_to_json(i)::text) AS hash FROM game.items i WHERE item_id = $1 OR location_id = $1 UNION ALL SELECT 'attributes', md5(row_to_json(a)::text) FROM game.item_attributes a WHERE item_id = $1 UNION ALL SELECT 'insurance', md5(row_to_json(i)::text) FROM game.insurance i WHERE ship_id = $1 UNION ALL SELECT 'reloads', md5(row_to_json(r)::text) FROM game.pending_reloads r WHERE ship_id = $1) rows ORDER BY kind, hash", &[&ship]).await.unwrap().iter().map(|r| (r.get("kind"), r.get("hash"))).collect()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn persistent_damage_repair_guard_and_death() {
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.group_id.is_some_and(|group| sde.repairable_ship.enters_recovery(t.type_id, group, CATEGORY_SHIP)
            && !sde.repairable_ship.unrepairable(CATEGORY_SHIP, group, t.type_id))
        && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE].iter().all(|id| t.attribute(*id).is_some_and(|v| v.is_finite() && v > 0.0))).unwrap().type_id;
    let system = 1_i64;
    let home = sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).unwrap().station_id;
    let (account, owner, ship) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('hull-health-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'hull-health-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
        tx.execute("UPDATE game.characters SET home_station_id=$2 WHERE character_id=$1", &[&owner, &home]).await?;
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
        let old_membership = w.with(system, move |s| s.persistent_generation(ship).unwrap()).await.unwrap();
        let facts = w.with(system, move |s| s.prepare_combat_source(ship, None, None, None, crate::lua::events::resources::FactOrigin::Natural, generation, 3, 1, |_| true)).await.unwrap().unwrap();
        assert!(damage::apply_damage_guarded(&d, &w, system, ship, [1e30, 0.0, 0.0, 0.0], damage::DamageSource { facts: Some(facts), ..Default::default() }, false, |_| true).await.is_err());
        assert_eq!(fingerprint(&d, ship).await, before_refusal);
        assert!(matches!(w.with(system, move |s| s.combat_phase(ship)).await.flatten(), Some(crate::space::combat::CombatPhase::Dying { .. })));
        assert!(d.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.unwrap().is_empty());
        assert!(d.query("SELECT group_id FROM game.repairable_recovery_groups WHERE beneficiary_id=$1", &[&owner]).await.unwrap().is_empty());
        d.execute("UPDATE game.items SET owner_id = $2 WHERE item_id = $1", &[&ship, &owner]).await.unwrap();
        w.remove_ball(system, ship).await.unwrap();
        let row = crate::dao::items::space_ships::unpiloted(&d, system).await.unwrap().into_iter().find(|row| row.item_id == ship).unwrap();
        let prepared = crate::services::ship::balls::unpiloted(&d, w.sde().clone(), &row).await.unwrap();
        w.with(system, move |s| prepared.install(s, |s, ball| s.add_persistent_ship(ball))).await.unwrap();
        assert_ne!(w.with(system, move |s| s.persistent_generation(ship).unwrap()).await.unwrap(), old_membership);
        let (readmitted_health, readmitted_condition) = w.with(system, move |s| {
            (s.ball(ship).unwrap().health, s.combat_condition_snapshot(ship).unwrap().condition)
        }).await.unwrap();
        assert_eq!(readmitted_health[1..], before_health[1..]);
        assert_eq!(fingerprint(&d, ship).await, repaired);
        let before_rollback = fingerprint(&d, ship).await;
        let snapshot = w.with(system, move |s| {
            let facts = s.prepare_combat_source(ship, None, None, None, crate::lua::events::resources::FactOrigin::Natural, generation, 4, 1, |_| true).unwrap();
            s.apply_damage(ship, [1e30, 0.0, 0.0, 0.0], damage::DamageSource { facts: Some(facts), ..Default::default() }, false).unwrap().unwrap().first_lethal.unwrap()
        }).await.unwrap();
        let pending = Arc::new(crate::services::ship::loss::facts::PlayerLossContext::new(None, [0.0; 3]));
        let failed: Result<(), CallError> = d.transaction(|tx, outbox: &mut Outbox| {
            let snapshot = Arc::clone(&snapshot);
            let sde = w.sde().clone();
            let pending = pending.clone();
            Box::pin(async move {
            assert!(destroy_in(tx, outbox, &sde, system, &snapshot, &pending, &[]).await?);
            assert!(tx.query_opt("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.map_err(crate::db::Error::from).map_err(db_err)?.is_some());
            tx.query_one("SELECT 1 / 0 AS failure", &[]).await.map_err(crate::db::Error::from).map_err(db_err)?;
            Ok(())
        }) }, |value, _| async move { Ok(value) }).await;
        assert!(failed.is_err());
        assert_eq!(fingerprint(&d, ship).await, before_rollback);
        assert!(d.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.unwrap().is_empty());
        let (token, membership_generation) = (snapshot.token, snapshot.generation);
        assert!(w.with(system, move |s| s.restore_known_rollback(ship, token, membership_generation)).await.unwrap());
        assert_eq!(w.ball(system, ship).await.unwrap().health[1..], readmitted_health[1..]);
        let restored = w.with(system, move |s| s.combat_condition_snapshot(ship).unwrap().condition).await.unwrap();
        assert_eq!(restored.now[1..], readmitted_condition.now[1..]);
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
        let group: i64 = d.query_one("SELECT group_id FROM game.repairable_recovery_groups WHERE beneficiary_id=$1 AND source_item_id=$2", &[&owner, &ship]).await.unwrap().get("group_id");
        let root = crate::dao::items::loading::load_items(&d, crate::dao::items::loading::ItemQuery::Id(ship)).await.unwrap();
        assert_eq!(root.len(), 1);
        assert_eq!((root[0].type_id, root[0].owner_id, root[0].location_id, root[0].flag_id, root[0].quantity, root[0].singleton), (kind, infinity_core::consts::NPC_CORP_INTERBUS, group, 0, 1, true));
        assert_eq!(d.query("SELECT item_id FROM game.repairable_recovery_items WHERE group_id=$1", &[&group]).await.unwrap().iter().map(|r| r.get::<_, i64>("item_id")).collect::<Vec<_>>(), [ship]);
        assert!(pending_reloads::for_ship(&d, ship).await.unwrap().is_empty());
        assert!(crate::dao::insurance::contract(&d, ship).await.unwrap().is_none());
        assert!(!crate::dao::items::space_ships::unpiloted(&d, system).await.unwrap().iter().any(|row| row.item_id == ship));
    }).await;
    world.remove_ball(system, ship).await;
    forget_ship(ship);
    let groups: Vec<i64> = db.query("SELECT group_id FROM game.repairable_recovery_groups WHERE beneficiary_id=$1", &[&owner]).await.unwrap().iter().map(|r| r.get("group_id")).collect();
    let members: Vec<(i64, i64)> = db.query("SELECT member_id, item_id FROM game.repairable_recovery_items WHERE group_id=ANY($1)", &[&groups]).await.unwrap().iter().map(|r| (r.get("member_id"), r.get("item_id"))).collect();
    let member_ids: Vec<i64> = members.iter().map(|(id, _)| *id).collect();
    let jobs: Vec<String> = members.iter().flat_map(|(id, _)| ["repairable.payment.expiry", "repairable.delivery.arrival", "repairable.withdrawal.expiry"].map(|kind| format!("{kind}:{id}"))).collect();
    let extras: Vec<i64> = std::iter::once(ship).chain(members.iter().map(|(_, item)| *item)).collect();
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account, character: owner }], &extras).await.unwrap();
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        crate::services::tests::fixture::cleanup_recovery(tx, owner).await?;
        tx.execute("DELETE FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await?;
        tx.execute("DELETE FROM game.pending_reloads WHERE ship_id = $1", &[&ship]).await?;
        tx.execute("DELETE FROM game.insurance WHERE ship_id = $1", &[&ship]).await?;
        tx.execute("DELETE FROM game.items WHERE item_id = $1 OR location_id = $1", &[&ship]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    assert!(db.query("SELECT 1 FROM game.insurance WHERE ship_id=$1", &[&ship]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.repairable_recovery_groups WHERE group_id=ANY($1)", &[&groups]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.repairable_recovery_items WHERE member_id=ANY($1)", &[&member_ids]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.jobs WHERE job_id=ANY($1)", &[&jobs]).await.unwrap().is_empty());
    result.unwrap();
}

