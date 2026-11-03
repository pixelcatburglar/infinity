use super::*;
use crate::{consts::{CATEGORY_SHIP, dogma::{ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE}}, dao::items::{creation, pending_reloads}, space::World};
use std::sync::Arc;

#[test]
fn changing_fitted_maxima_and_resistance_preserves_condition() {
    let stats = ShipStats { max: [100.0, 200.0, 300.0], resonance: [[1.0; 4]; 3], shield_tau_ms: 1000.0 };
    let mut cond = Condition { stats: stats.clone(), now: [80.0, 150.0, 210.0], shield_at: 0 };
    let mut changed = stats.clone();
    changed.max = [60.0, 300.0, 400.0];
    changed.resonance[0][0] = 0.5;
    rebase(&mut cond, changed);
    assert_eq!(cond.now, [60.0, 250.0, 310.0]);
    assert_eq!(absorb(&mut cond.now, &cond.stats.resonance, [20.0, 0.0, 0.0, 0.0]), 10.0);
    assert_eq!(cond.now[0], 50.0);
    rebase(&mut cond, stats.clone());
    assert_eq!(cond.now, [50.0, 150.0, 210.0]);
    assert_eq!(absorb(&mut cond.now, &cond.stats.resonance, [20.0, 0.0, 0.0, 0.0]), 20.0);
    let mut boundary = stats;
    boundary.max = [20.0, 50.0, 90.0];
    rebase(&mut cond, boundary);
    assert_eq!(cond.now, [20.0, 0.0, 0.0]);
}

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
        pending_reloads::insert(tx, owner, ship, ship, 1, kind, 1).await?;
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
        w.with(system, move |s| {
            let mut ball = s.take(ship).unwrap();
            ball.char_id = Some(owner);
            s.add_ship(ball);
        }).await.unwrap();
        assert_eq!(groups_health::damage_npc(&d, &w, system, ship, [1e30, 0.0, 0.0, 0.0], None).await.unwrap(), 0.0);
        assert!(w.ball(system, ship).await.is_some());
        assert!(!state().hp.contains_key(&ship));
        assert_eq!(groups_health::damage(&d, &w, system, ship, [0.0; 4], None, None).await.unwrap(), 0.0);
        assert!(w.ball(system, ship).await.unwrap().char_id.is_some());
        w.with(system, move |s| {
            let mut ball = s.take(ship).unwrap();
            ball.char_id = None;
            s.add_persistent_ship(ball);
        }).await.unwrap();
        assert!(groups_health::damage(&d, &w, system, ship, [1.0, 0.0, 0.0, 0.0], None, None).await.unwrap() > 0.0);
        let before_guard = fingerprint(&d, ship).await;
        let health_guard = w.ball(system, ship).await.unwrap().health;
        repair(&d, &w, system, ship, 0, 1.0, |_| false).await.unwrap();
        assert_eq!(fingerprint(&d, ship).await, before_guard);
        assert_eq!(w.ball(system, ship).await.unwrap().health, health_guard);
        let initial = fingerprint(&d, ship).await;
        assert!(damage(&d, &w, system, ship, [1.0, 0.0, 0.0, 0.0]).await.unwrap().unwrap() > 0.0);
        let damaged = fingerprint(&d, ship).await;
        assert_ne!(initial, damaged);
        assert!(d.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.unwrap().is_empty());
        repair(&d, &w, system, ship, 0, 1.0, |_| true).await.unwrap();
        let repaired = fingerprint(&d, ship).await;
        assert_ne!(damaged, repaired);
        d.execute("UPDATE game.items SET owner_id = $2 WHERE item_id = $1", &[&ship, &(owner + 1)]).await.unwrap();
        let before_refusal = fingerprint(&d, ship).await;
        let before_health = w.ball(system, ship).await.unwrap().health;
        assert_eq!(damage(&d, &w, system, ship, [1e30, 0.0, 0.0, 0.0]).await.unwrap(), Some(0.0));
        assert_eq!(fingerprint(&d, ship).await, before_refusal);
        assert_eq!(w.ball(system, ship).await.unwrap().health, before_health);
        assert!(d.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.unwrap().is_empty());
        d.execute("UPDATE game.items SET owner_id = $2 WHERE item_id = $1", &[&ship, &owner]).await.unwrap();
        let before_rollback = fingerprint(&d, ship).await;
        let failed: Result<(), CallError> = d.transaction(|tx, _: &mut Outbox| Box::pin(async move {
            assert!(crate::dao::items::space_ships::claim_unpiloted(tx, ship, kind, owner, system).await.map_err(db_err)?);
            delete_in(tx, ship).await?;
            crate::services::map::events::unpiloted_lost(tx, ship, system, filetime_now()).await?;
            assert!(tx.query_opt("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.map_err(crate::db::Error::from).map_err(db_err)?.is_some());
            tx.query_one("SELECT 1 / 0 AS failure", &[]).await.map_err(crate::db::Error::from).map_err(db_err)?;
            Ok(())
        }), |value, _| async move { Ok(value) }).await;
        assert!(failed.is_err());
        assert_eq!(fingerprint(&d, ship).await, before_rollback);
        assert!(d.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.unwrap().is_empty());
        assert_eq!(w.ball(system, ship).await.unwrap().health, before_health);
        damage(&d, &w, system, ship, [1e30, 0.0, 0.0, 0.0]).await.unwrap();
        assert!(w.ball(system, ship).await.is_none());
        let events = d.query("SELECT solar_system_id, kind FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].get::<_, i64>("solar_system_id"), events[0].get::<_, i16>("kind")), (system, crate::dao::map_telemetry::EventKind::Ship as i16));
        assert_eq!(damage(&d, &w, system, ship, [1e30, 0.0, 0.0, 0.0]).await.unwrap(), None);
        assert_eq!(d.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::UnpilotedShip as i16), &ship]).await.unwrap().len(), 1);
        repair(&d, &w, system, ship, 2, 1e30, |_| true).await.unwrap();
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

