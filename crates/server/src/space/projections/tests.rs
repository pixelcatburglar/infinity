use super::*;
use crate::services::effects::{EffectKind, JammingType};

async fn database_fingerprint(db: &crate::db::Db) -> Vec<(String, i64, Vec<(String, String)>)> {
    let mut result = Vec::new();
    for (table, key) in [("accounts", "user_id"), ("characters", "character_id"),
        ("items", "item_id"), ("item_attributes", "item_id, attribute_id")] {
        let count = db.query_one(&format!("SELECT count(*) AS n FROM game.{table}"), &[]).await.unwrap().get("n");
        let rows = db.query(&format!("SELECT json_build_array({key})::text AS key, md5(row_to_json(t)::text) AS hash FROM game.{table} t ORDER BY {key}"), &[]).await.unwrap()
            .iter().map(|r| (r.get("key"), r.get("hash"))).collect();
        result.push((table.to_owned(), count, rows));
    }
    result
}

async fn unpiloted_fit(db: &crate::db::Db, world: &World, system: i64, ship: i64) -> crate::services::fit::Fit {
    let projections = world.with(system, move |s| s.projections_on(ship)).await.unwrap();
    crate::services::fit::Fit::load_unpiloted_projected(db, world.sde().clone(), ship, projections).await.unwrap()
}

#[test]
fn expiration_invalidates_the_receiving_snapshot() {
    let mut system = System::default();
    let ball = Ball {
        id: 1, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: MODE_STOP,
        radius: 1.0, mass: 1.0, max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3], follow_id: 0,
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0,
        cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    };
    system.insert(ball.clone());
    system.insert(Ball { id: 2, ..ball });
    system.upsert_projection(1, Projection {
        source_ship: 2, source_module: 3, source_type: 1, effect_id: 1,
        kind: EffectKind::Projected { jamming: JammingType::Webify }, until_ms: 0,
        values: vec![], charge_values: vec![], charge_type: None, scramble: None,
    });
    let revision = system.projection_revision(1);
    assert_eq!(system.projections_on(1).len(), 1);
    system.expire_projections();
    assert!(system.projections_on(1).is_empty());
    assert!(system.projection_revision(1) > revision);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn projected_free_hull_refresh_keeps_fitting_and_overrides() {
    use infinity_core::consts::{CATEGORY_SHIP, FLAG_FITTING_FIRST, dogma::{ATTR_ARMOR_DAMAGE, ATTR_ARMOR_HP, ATTR_HULL_DAMAGE, ATTR_MASS, ATTR_MAX_VELOCITY}};
    use crate::dao::items::{creation, dogma};
    use crate::services::{effects::EffectKind, fit::{Fit, Key}};
    use std::sync::Arc;

    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let before = database_fingerprint(&db).await;
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_ARMOR_HP).unwrap_or(0.0) > 100.0
        && t.attribute(ATTR_MAX_VELOCITY).unwrap_or(0.0) > 0.0
        && t.attribute(ATTR_MASS).or(t.mass).unwrap_or(0.0) > 0.0).expect("ship with armor, velocity and mass").type_id;
    let plate = sde.types.iter().find(|t| t.name.as_deref() == Some("400mm Steel Plates I"))
        .expect("armor plate in SDE").type_id;
    let system = 1_i64;
    let mass = sde.types.get(hull).unwrap().attribute(ATTR_MASS).or(sde.types.get(hull).unwrap().mass).unwrap() * 1.25;
    let (account, owner, ship) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('projection-hull-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'projection-hull-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        let ship = creation::positioned(tx, hull, owner, system, [0.0; 3]).await?;
        creation::fitted_items(tx, ship, owner, &[(plate, FLAG_FITTING_FIRST, 1)]).await?;
        dogma::set_override(tx, ship, ATTR_MASS, mass).await?;
        dogma::set_override(tx, ship, ATTR_ARMOR_DAMAGE, 10.0).await?;
        dogma::set_override(tx, ship, ATTR_HULL_DAMAGE, 20.0).await?;
        Ok::<_, crate::db::Error>((account, owner, ship))
    }), |v, ()| async move { Ok(v) }).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let d = db.clone();
    let w = world.clone();
    let result = tokio::spawn(async move {
        let baseline = Fit::load_unpiloted(&d, sde.clone(), ship).await.unwrap();
        let bare = Fit::for_space_type(sde.clone(), hull, vec![]);
        assert!(baseline.value(Key::Ship, ATTR_ARMOR_HP) > bare.value(Key::Ship, ATTR_ARMOR_HP));
        assert_ne!(baseline.value(Key::Ship, ATTR_MASS), bare.value(Key::Ship, ATTR_MASS));
        let baseline_speed = baseline.value(Key::Ship, ATTR_MAX_VELOCITY) as f32;
        let baseline_mass = baseline.value(Key::Ship, ATTR_MASS);
        let baseline_armor = baseline.value(Key::Ship, ATTR_ARMOR_HP);
        let condition = d.query("SELECT attribute_id, value FROM game.item_attributes WHERE item_id=$1 AND attribute_id=ANY($2) ORDER BY attribute_id", &[&ship, &&[ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE][..]]).await.unwrap()
            .iter().map(|r| (r.get::<_, i32>("attribute_id"), r.get::<_, f64>("value"))).collect::<Vec<_>>();
        assert_eq!(condition.len(), 2);
        let mut selected = None;
        'search: for source in sde.types.iter() {
            let source_fit = Fit::for_space_type(sde.clone(), source.type_id, vec![]);
            for effect in source.effects() {
                let Some(dogma) = sde.dogma.effect(effect.effect_id) else { continue };
                if !crate::services::effects::registry::resolve(&sde, effect.effect_id)
                    .is_some_and(|e| e.kind == (EffectKind::Projected { jamming: JammingType::Webify })) { continue; }
                if !dogma.modifiers().iter().any(|m| m.domain == "targetID" && m.modified == ATTR_MAX_VELOCITY) { continue; }
                let mut values = Vec::new();
                for modifier in dogma.modifiers().iter().filter(|m| m.domain == "targetID") {
                    if values.iter().any(|(id, _)| *id == modifier.modifying) { continue; }
                    let attribute = crate::sde::dogma::python_effects::projected_interim_source(modifier.modifying).unwrap_or(modifier.modifying);
                    values.push((modifier.modifying, source_fit.value(Key::Ship, attribute)));
                }
                let projection = Projection { source_ship: ship, source_module: -1, source_type: source.type_id,
                    effect_id: effect.effect_id, kind: EffectKind::Projected { jamming: JammingType::Webify }, until_ms: i64::MAX,
                    values, charge_values: vec![], charge_type: None, scramble: None };
                let fitted = Fit::load_unpiloted_projected(&d, sde.clone(), ship, vec![projection.clone()]).await.unwrap();
                let speed = fitted.value(Key::Ship, ATTR_MAX_VELOCITY) as f32;
                if speed.is_finite() && speed > 0.0 && speed != baseline_speed {
                    assert_eq!(fitted.value(Key::Ship, ATTR_ARMOR_HP), baseline_armor);
                    selected = Some((projection, speed, fitted.value(Key::Ship, ATTR_MASS)));
                    break 'search;
                }
            }
        }
        let (projection, projected_speed, projected_mass) = selected.expect("SDE type/effect with effective targetID max velocity modifier");
        w.system(system).await.unwrap();
        let stale = unpiloted_fit(&d, &w, system, ship).await;
        let (generation, fit_revision, projection_revision) = w.with(system, move |s| (
            s.combat_membership_generation(ship).unwrap(), s.combat_fit_revision(ship).unwrap(), s.projection_revision(ship),
        )).await.unwrap();
        let health = w.ball(system, ship).await.unwrap().health;
        let mut expired = projection.clone();
        expired.until_ms = 0;
        w.with(system, move |s| s.upsert_projection(ship, projection)).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let ball = w.ball(system, ship).await.unwrap();
                if ball.max_vel == projected_speed && (ball.mass - projected_mass).abs() < 1e-6 { break; }
                assert!(w.after(system, 100).await);
            }
        }).await.expect("projected kinematics published");
        let expected = unpiloted_fit(&d, &w, system, ship).await;
        let stats = crate::services::combat::groups_health::ShipStats::from_fit(&expected, Key::Ship);
        let snapshot = w.with(system, move |s| s.combat_condition_snapshot(ship)).await.flatten().unwrap();
        assert_eq!(snapshot.condition.stats.max, stats.max);
        assert_eq!(snapshot.condition.stats.resonance, stats.resonance);
        assert_eq!(snapshot.condition.stats.shield_tau_ms, stats.shield_tau_ms);
        assert_eq!(snapshot.condition.stats.max[1] - snapshot.condition.now[1], 10.0);
        assert_eq!(snapshot.condition.stats.max[2] - snapshot.condition.now[2], 20.0);
        assert!(!w.with(system, move |s| s.replace_fitted_cache(ship, generation, fit_revision, stale, Key::Ship, Some(projection_revision))).await.unwrap());
        assert_eq!(w.ball(system, ship).await.unwrap().health, health);
        let projected_condition = d.query("SELECT attribute_id, value FROM game.item_attributes WHERE item_id=$1 AND attribute_id=ANY($2) ORDER BY attribute_id", &[&ship, &&[ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE][..]]).await.unwrap()
            .iter().map(|r| (r.get::<_, i32>("attribute_id"), r.get::<_, f64>("value"))).collect::<Vec<_>>();
        assert_eq!(projected_condition, condition);
        w.with(system, move |s| {
            s.upsert_projection(ship, expired);
            s.expire_projections();
        }).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let ball = w.ball(system, ship).await.unwrap();
                if ball.max_vel == baseline_speed && (ball.mass - baseline_mass).abs() < 1e-6 { break; }
                assert!(w.after(system, 100).await);
            }
        }).await.expect("fitted baseline kinematics restored");
        let expected = unpiloted_fit(&d, &w, system, ship).await;
        let stats = crate::services::combat::groups_health::ShipStats::from_fit(&expected, Key::Ship);
        let snapshot = w.with(system, move |s| s.combat_condition_snapshot(ship)).await.flatten().unwrap();
        assert_eq!(snapshot.condition.stats.max, stats.max);
        assert_eq!(snapshot.condition.stats.resonance, stats.resonance);
        assert_eq!(snapshot.condition.stats.shield_tau_ms, stats.shield_tau_ms);
        assert_eq!(snapshot.condition.stats.max[1] - snapshot.condition.now[1], 10.0);
        assert_eq!(snapshot.condition.stats.max[2] - snapshot.condition.now[2], 20.0);
        assert_eq!(w.ball(system, ship).await.unwrap().health, health);
        let restored_condition = d.query("SELECT attribute_id, value FROM game.item_attributes WHERE item_id=$1 AND attribute_id=ANY($2) ORDER BY attribute_id", &[&ship, &&[ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE][..]]).await.unwrap()
            .iter().map(|r| (r.get::<_, i32>("attribute_id"), r.get::<_, f64>("value"))).collect::<Vec<_>>();
        assert_eq!(restored_condition, condition);
        let stale = unpiloted_fit(&d, &w, system, ship).await;
        let (old_generation, old_fit_revision, old_projection_revision) = w.with(system, move |s| {
            let generation = s.combat_membership_generation(ship).unwrap();
            assert_eq!(s.persistent_generation(ship), Some(generation));
            (generation, s.combat_fit_revision(ship).unwrap(), s.projection_revision(ship))
        }).await.unwrap();
        let market = crate::services::market::lock().await;
        let row = crate::dao::items::space_ships::FreeShip { item_id: ship, type_id: hull, owner_id: owner, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let replacement = crate::services::ship::balls::unpiloted(&d, sde.clone(), &row).await.unwrap();
        w.with(system, move |s| {
            s.remove_ball(ship).expect("original persistent hull");
            replacement.install(s, |s, ball| s.add_persistent_ship(ball));
        }).await.unwrap();
        drop(market);
        let generation = w.with(system, move |s| {
            let generation = s.combat_membership_generation(ship).unwrap();
            assert_eq!(s.persistent_generation(ship), Some(generation));
            generation
        }).await.unwrap();
        assert_ne!(old_generation, generation);
        assert!(!w.with(system, move |s| s.replace_fitted_cache(ship, old_generation, old_fit_revision, stale, Key::Ship, Some(old_projection_revision))).await.unwrap());
    }).await;
    world.remove_ball(system, ship).await;
    crate::services::combat::forget_ship(ship);
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&ship]).await?;
        tx.execute("DELETE FROM game.items WHERE location_id=$1", &[&ship]).await?;
        tx.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    assert_eq!(database_fingerprint(&db).await, before);
    result.unwrap();
}
