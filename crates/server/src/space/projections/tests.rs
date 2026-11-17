use super::*;
use crate::services::effects::{EffectKind, JammingType};
use crate::space::tests::fit_delta::{assert_batch, attr_batches, pilot_channels};

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
        values: vec![], charge_values: vec![], charge_type: None, resistance_attribute: None, scramble: None,
    });
    let revision = system.projection_revision(1);
    assert_eq!(system.projections_on(1).len(), 1);
    system.expire_projections();
    assert!(system.projections_on(1).is_empty());
    assert!(system.projection_revision(1) > revision);
}

fn fixture_ball(id: i64, owner: i64, character: Option<i64>) -> Ball {
    Ball { id, type_id: 1, owner_id: owner, corp_id: 1, alliance_id: -1,
        char_id: character, signature_radius: None, mode: MODE_STOP,
        radius: 1.0, mass: 1.0, max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3], follow_id: 0,
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0,
        cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

fn fixture_projection(module: i64, until_ms: i64, values: Vec<(i32, f64)>) -> Projection {
    Projection { source_ship: 2, source_module: module, source_type: 1, effect_id: 1,
        kind: EffectKind::Projected { jamming: JammingType::Webify }, until_ms,
        values, charge_values: vec![], charge_type: None, resistance_attribute: None, scramble: None }
}

#[test]
fn resident_refresh_renews_deadline_and_keeps_drone_domains_separate() {
    crate::tests::init_config();
    let sde = crate::sde::types::tests::combat_fixture();
    let mut system = System::new(1, std::sync::Arc::new(crate::space::Pilots::default()), 0);
    system.add_persistent_ship(fixture_ball(1, 2, Some(2)));
    system.insert(fixture_ball(2, 3, Some(3)));
    let fit = crate::services::fit::Fit::for_space_type(sde.clone(), 1, vec![]);
    let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
    system.install_condition(1, crate::services::combat::groups_health::Condition {
        now: stats.max, stats, shield_at: 0 }, fit);
    system.upsert_projection(1, fixture_projection(9, 100, vec![(1, -10.0)]));
    let revision = system.projection_revision(1);
    let fit_revision = system.combat_fit_revision(1).unwrap();
    // Same identity and operands: only the stored deadline moves, no rebuild.
    system.upsert_projection(1, fixture_projection(9, 200, vec![(1, -10.0)]));
    assert_eq!(system.combat_fit_revision(1).unwrap(), fit_revision);
    assert_eq!(system.projections_on(1)[0].until_ms, 200);
    // Changed operands synchronously rebuild the resident fit, no deferred pass.
    system.upsert_projection(1, fixture_projection(9, 300, vec![(1, -20.0)]));
    assert!(system.projection_revision(1) > revision);
    assert!(system.combat_fit_revision(1).unwrap() > fit_revision);
    assert_eq!(system.with_combat_fit(1, |fit, _, _| fit.source_type(crate::services::fit::Key::Projected(9))), Some(Some(1)));
    // A controlled drone's own projections stay in its Drone domain while the
    // owner's ship snapshot lands in its Ship domain.
    use crate::space::drone::{DroneEffects, DroneSpec};
    system.insert(fixture_ball(5, 2, None));
    let spec = DroneSpec { type_id: 1, bandwidth: 5.0, scan_resolution: 100.0, control_range: 1000.0,
        max_target_range: 1000.0, optimal_range: 100.0, orbit_range: 50.0, duration_ms: 1000.0, effect_id: Some(1) };
    let effects: DroneEffects = std::sync::Arc::new(|_, _| None);
    system.register_drone(5, spec, effects, Some(1), Some(2), false);
    let fit = crate::services::fit::Fit::for_space_type(sde, 1, vec![]);
    let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Drone(5));
    system.install_condition_with_key(5, crate::services::combat::groups_health::Condition {
        now: stats.max, stats, shield_at: 0 }, fit, crate::services::fit::Key::Drone(5));
    system.upsert_projection(5, fixture_projection(7, i64::MAX, vec![(2, -5.0)]));
    system.upsert_projection(1, fixture_projection(11, i64::MAX, vec![(3, -7.0)]));
    let observed = system.with_combat_fit(5, |fit, _, _| (
        fit.source_type(crate::services::fit::Key::Projected(11)).is_some(),
        fit.source_type(crate::services::fit::Key::Projected(7)).is_some()));
    assert_eq!(observed, Some((true, true)));
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
                    values, charge_values: vec![], charge_type: None, resistance_attribute: None, scramble: None };
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
        let old_generation = w.with(system, move |s| {
            let generation = s.combat_membership_generation(ship).unwrap();
            assert_eq!(s.persistent_generation(ship), Some(generation));
            generation
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
    }).await;
    world.remove_ball(system, ship).await;
    crate::services::combat::forget_ship(ship);
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account, character: owner }], &[ship]).await.unwrap();
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&ship]).await?;
        tx.execute("DELETE FROM game.items WHERE location_id=$1", &[&ship]).await?;
        tx.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn resident_projection_publication_tracks_source_lifecycle() {
    use crate::services::{combat::groups_health::{Condition, ShipStats}, fit::{Fit, Key}};
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && ShipStats::from_fit(&Fit::for_space_type(sde.clone(), t.type_id, vec![]), Key::Ship)
            .max.iter().all(|v| v.is_finite() && *v > 0.0)).unwrap().type_id;
    let bare = Fit::for_space_type(sde.clone(), hull, vec![]);
    // Expected deltas of one projection swap on the resident ship fit.
    let changes = |old: Vec<Projection>, new: Vec<Projection>| {
        let before = Fit::for_space_type(sde.clone(), hull, old);
        let after = Fit::for_space_type(sde.clone(), hull, new);
        after.fitting_changes(&before.fitting_values(&before.fitting_candidates(&after)))
    };
    // Two real projected sources whose targetID modifiers move the hull.
    let mut sources = Vec::new();
    'search: for source in sde.types.iter().filter(|t| !t.effects().is_empty()) {
        let fit = Fit::for_space_type(sde.clone(), source.type_id, vec![]);
        for e in source.effects() {
            let Some(dogma) = sde.dogma.effect(e.effect_id) else { continue };
            let mut values: Vec<(i32, f64)> = Vec::new();
            for m in dogma.modifiers().iter().filter(|m| m.domain == "targetID") {
                if !values.iter().any(|v| v.0 == m.modifying) {
                    values.push((m.modifying, fit.value(Key::Ship,
                        crate::sde::dogma::python_effects::projected_interim_source(m.modifying)
                            .unwrap_or(m.modifying))));
                }
            }
            if values.is_empty() { continue; }
            let p = Projection { source_type: source.type_id, effect_id: e.effect_id,
                ..fixture_projection(9, i64::MAX, values) };
            let changes = changes(vec![], vec![p.clone()]);
            if changes.is_empty() { continue; }
            sources.push((p, changes));
            if sources.len() == 2 { break 'search; }
        }
    }
    let [applied, replaced]: [(Projection, Vec<(Key, i32, f64, f64)>); 2] = sources.try_into()
        .ok().expect("SDE needs two projected sources with real target modifiers");
    assert!(applied.1.iter()
        .any(|c| !bare.fitting_candidates(&bare).contains(&(c.0, c.1))),
        "an incoming-only receiver attribute is required");
    let registry = Arc::new(crate::space::Pilots::default());
    let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
    let (tx3, mut rx3) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(2, tx1);
    registry.register_pilot(4, tx3);
    let mut system = System::new(1, registry, 0);
    system.insert(fixture_ball(2, 3, Some(3)));
    for (ship, owner, pilot) in [(1, 2, Some(2)), (3, 4, Some(4))] {
        let mut ball = fixture_ball(ship, owner, pilot);
        ball.type_id = hull;
        system.add_persistent_ship(ball);
        let fit = Fit::for_space_type(sde.clone(), hull, vec![]);
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        system.install_condition(ship, Condition { now: stats.max, stats, shield_at: 0 }, fit);
    }
    // Apply, changed operands and a same-target identity replacement.
    system.upsert_projection(1, applied.0.clone());
    assert_batch(&mut rx1, 1, 2, Key::Ship, &applied.1);
    let mut moved = applied.0.clone();
    for v in &mut moved.values { v.1 *= 1.5; }
    let operand = changes(vec![applied.0.clone()], vec![moved.clone()]);
    system.upsert_projection(1, moved.clone());
    assert_batch(&mut rx1, 1, 2, Key::Ship, &operand);
    system.upsert_projection(1, replaced.0.clone());
    assert_batch(&mut rx1, 1, 2, Key::Ship, &changes(vec![moved], vec![replaced.0.clone()]));
    // A target move completes each side's own publication.
    system.upsert_projection(3, replaced.0.clone());
    assert_batch(&mut rx1, 1, 2, Key::Ship, &changes(vec![replaced.0.clone()], vec![]));
    assert_batch(&mut rx3, 3, 4, Key::Ship, &replaced.1);
    // Removal restores the base rows on the receiving side.
    system.remove_projection(9);
    assert_batch(&mut rx3, 3, 4, Key::Ship, &changes(vec![replaced.0.clone()], vec![]));
    // A renewal moves the stored deadline silently; expiry removes once.
    system.upsert_projection(1, applied.0.clone());
    assert_batch(&mut rx1, 1, 2, Key::Ship, &applied.1);
    system.upsert_projection(1, applied.0.clone());
    assert!(attr_batches(&mut rx1).is_empty(), "deadline renewal must not republish");
    let mut expiring = applied.0.clone(); expiring.until_ms = 0;
    system.upsert_projection(1, expiring);
    assert!(attr_batches(&mut rx1).is_empty());
    system.expire_projections();
    assert_batch(&mut rx1, 1, 2, Key::Ship, &changes(vec![applied.0.clone()], vec![]));
    // A same-identity renewal consumes a due expiry once, then stays quiet.
    use crate::{dao::skills::expert_systems::ExpertSystem, services::fit::character::CharacterInputs};
    let now = crate::client::filetime_now();
    let sec = infinity_core::consts::FILETIME_SECOND;
    let inputs = |type_id: i32| CharacterInputs { at: now - 2 * sec,
        installations: vec![ExpertSystem { character_id: 2, type_id,
            installed_at: now - 2 * sec, expires_at: now - sec }], ..Default::default() };
    let final_fit = Fit::for_space_type(sde.clone(), hull, vec![applied.0.clone()]);
    let mut expiry = None;
    for def in sde.expert_systems.iter() {
        let mut before = Fit::for_space_type(sde.clone(), hull, vec![applied.0.clone()]);
        before.apply_character_inputs(&inputs(def.type_id));
        let delta = final_fit.fitting_changes(&before.fitting_values(&before.fitting_candidates(&final_fit)));
        if delta.iter().any(|(key, _, _, _)| *key == Key::Ship) { expiry = Some((before, delta)); break; }
    }
    let (before, expected) = expiry.expect("SDE needs a ship-modifying expert grant");
    system.upsert_projection(1, applied.0.clone());
    assert_batch(&mut rx1, 1, 2, Key::Ship, &applied.1);
    let stats = ShipStats::from_fit(&before, Key::Ship);
    system.install_condition(1, Condition { now: stats.max, stats, shield_at: 0 }, before);
    system.upsert_projection(1, applied.0.clone());
    assert_batch(&mut rx1, 1, 2, Key::Ship, &expected);
    system.upsert_projection(1, applied.0.clone());
    assert!(attr_batches(&mut rx1).is_empty());
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn resident_drone_projections_publish_own_key_to_real_owner() {
    use crate::services::{combat::groups_health::{Condition, ShipStats}, fit::{Fit, Key, character::CharacterInputs}};
    use crate::space::drone::{DroneEffects, DroneSpec};
    use infinity_core::consts::{CATEGORY_DRONE, CATEGORY_SHIP, dogma::ATTR_MAX_VELOCITY};
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    const DRONE: i64 = 5;
    const SHIP: i64 = 4;
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && ShipStats::from_fit(&Fit::for_space_type(sde.clone(), t.type_id, vec![]), Key::Ship)
            .max.iter().all(|v| v.is_finite() && *v > 0.0)).unwrap().type_id;
    let build = |drone: i32, receiver: Key, projections: Vec<Projection>| {
        let mut fit = Fit::for_space_type(sde.clone(), hull, vec![]);
        fit.include_drone(DRONE, drone, vec![]);
        fit.replace_projections_for(receiver, projections, None);
        fit
    };
    let drone_type = sde.types.iter().find(|t| {
        t.category_id == Some(CATEGORY_DRONE) && t.attribute(ATTR_MAX_VELOCITY).unwrap_or(0.0) > 0.0 && {
            let stats = ShipStats::from_fit(&build(t.type_id, Key::Drone(DRONE), vec![]), Key::Drone(DRONE));
            stats.max.iter().all(|v| v.is_finite() && *v > 0.0) && stats.shield_tau_ms.is_finite()
                && stats.resonance.iter().flatten().all(|v| v.is_finite() && *v >= 0.0)
        }
    }).map(|t| t.type_id).expect("drone type with finite condition and velocity");
    // Expected wire rows of one projection swap on the given receiver domain.
    let delta = |receiver: Key, old: Vec<Projection>, new: Vec<Projection>| {
        let before = build(drone_type, receiver, old);
        let after = build(drone_type, receiver, new);
        after.fitting_changes(&before.fitting_values(&before.fitting_candidates(&after)))
            .into_iter().filter(|&(key, ..)| key == receiver).collect::<Vec<_>>()
    };
    // Real projected sources whose targetID modifiers move the drone's own
    // key and the copied Ship domain; the applied one must also change under
    // a rescaled operand and land on an incoming-only attribute.
    let bare = build(drone_type, Key::Drone(DRONE), vec![]);
    let bare_candidates = bare.fitting_candidates(&bare);
    let mut sources = Vec::new();
    'search: for source in sde.types.iter().filter(|t| !t.effects().is_empty()) {
        let fit = Fit::for_space_type(sde.clone(), source.type_id, vec![]);
        for e in source.effects() {
            let Some(dogma) = sde.dogma.effect(e.effect_id) else { continue };
            let mut values: Vec<(i32, f64)> = Vec::new();
            for m in dogma.modifiers().iter().filter(|m| m.domain == "targetID") {
                if !values.iter().any(|v| v.0 == m.modifying) {
                    values.push((m.modifying, fit.value(Key::Ship,
                        crate::sde::dogma::python_effects::projected_interim_source(m.modifying)
                            .unwrap_or(m.modifying))));
                }
            }
            if values.is_empty() { continue; }
            let p = Projection { source_type: source.type_id, effect_id: e.effect_id,
                ..fixture_projection(9, i64::MAX, values) };
            let mut scaled = p.clone();
            for v in &mut scaled.values { v.1 *= 1.5; }
            if delta(Key::Drone(DRONE), vec![], vec![p.clone()]).is_empty()
                || delta(Key::Drone(DRONE), vec![p.clone()], vec![scaled]).is_empty()
                || delta(Key::Ship, vec![], vec![p.clone()]).is_empty()
                || sources.iter().any(|q: &Projection| q.effect_id == p.effect_id) { continue; }
            sources.push(p);
            if sources.len() == 8 { break 'search; }
        }
    }
    let applied = sources.iter().find(|p| delta(Key::Drone(DRONE), vec![], vec![(*p).clone()])
        .iter().any(|c| !bare_candidates.contains(&(c.0, c.1)))).cloned()
        .expect("SDE needs a projected source with an incoming-only drone attribute");
    let replaced = sources.iter().find(|p| p.effect_id != applied.effect_id).cloned()
        .expect("SDE needs a second distinct drone-modifying projection");
    let (registry, mut rx_owner, mut rx_delegate) = pilot_channels();
    let mut system = System::new(1, registry, 0);
    system.insert(fixture_ball(2, 9, Some(9)));
    let mut ball = fixture_ball(SHIP, 3, Some(3));
    ball.type_id = hull;
    system.add_persistent_ship(ball);
    // The delegate's ship fit caches a Drone node for the same item; its rows
    // still must never borrow the resident drone's authoritative key.
    let fit = build(drone_type, Key::Ship, vec![]);
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(SHIP, Condition { now: stats.max, stats, shield_at: 0 }, fit);
    system.insert(fixture_ball(DRONE, 2, None));
    let spec = DroneSpec { type_id: drone_type, bandwidth: 5.0, scan_resolution: 100.0, control_range: 1000.0,
        max_target_range: 1000.0, optimal_range: 100.0, orbit_range: 50.0, duration_ms: 1000.0, effect_id: Some(1) };
    let effects: DroneEffects = Arc::new(|_, _| None);
    system.register_drone(DRONE, spec, effects, Some(SHIP), Some(3), false);
    let fit = build(drone_type, Key::Drone(DRONE), vec![]);
    let stats = ShipStats::from_fit(&fit, Key::Drone(DRONE));
    system.install_condition_with_key(DRONE, Condition { now: stats.max, stats, shield_at: 0 }, fit, Key::Drone(DRONE));
    // Direct Drone-domain lifecycle: apply, operand, identity replacement.
    system.upsert_projection(DRONE, applied.clone());
    assert_batch(&mut rx_owner, DRONE, 2, Key::Drone(DRONE), &delta(Key::Drone(DRONE), vec![], vec![applied.clone()]));
    let mut moved = applied.clone();
    for v in &mut moved.values { v.1 *= 1.5; }
    system.upsert_projection(DRONE, moved.clone());
    assert_batch(&mut rx_owner, DRONE, 2, Key::Drone(DRONE), &delta(Key::Drone(DRONE), vec![applied.clone()], vec![moved.clone()]));
    system.upsert_projection(DRONE, replaced.clone());
    assert_batch(&mut rx_owner, DRONE, 2, Key::Drone(DRONE), &delta(Key::Drone(DRONE), vec![moved], vec![replaced.clone()]));
    // Target move: the drone restores its base rows while the ship's own
    // publication lands on the delegate channel carrying only ship items.
    system.upsert_projection(SHIP, replaced.clone());
    assert_batch(&mut rx_owner, DRONE, 2, Key::Drone(DRONE), &delta(Key::Drone(DRONE), vec![replaced.clone()], vec![]));
    assert_batch(&mut rx_delegate, SHIP, 3, Key::Ship, &delta(Key::Ship, vec![], vec![replaced.clone()]));
    system.remove_projection(9);
    assert_batch(&mut rx_delegate, SHIP, 3, Key::Ship, &delta(Key::Ship, vec![replaced.clone()], vec![]));
    // Renewal is quiet; a stored deadline expires into one removal batch.
    system.upsert_projection(DRONE, applied.clone());
    assert_batch(&mut rx_owner, DRONE, 2, Key::Drone(DRONE), &delta(Key::Drone(DRONE), vec![], vec![applied.clone()]));
    system.upsert_projection(DRONE, applied.clone());
    assert!(attr_batches(&mut rx_owner).is_empty(), "deadline renewal must not republish");
    let mut expiring = applied.clone();
    expiring.until_ms = 0;
    system.upsert_projection(DRONE, expiring);
    system.expire_projections();
    assert_batch(&mut rx_owner, DRONE, 2, Key::Drone(DRONE), &delta(Key::Drone(DRONE), vec![applied.clone()], vec![]));
    // The controller's ship snapshot is copied into the drone's Ship domain:
    // the copied values land while the own Drone values stay put, so the real
    // owner's channel stays silent for a pure ship-domain change.
    let ship_attr = delta(Key::Ship, vec![], vec![applied.clone()])[0].1;
    let before_copy = system.with_combat_fit(DRONE, |fit, _, _| fit.value(Key::Ship, ship_attr)).unwrap();
    let mut copied = applied.clone();
    copied.source_module = 11;
    system.upsert_projection(SHIP, copied.clone());
    assert_batch(&mut rx_delegate, SHIP, 3, Key::Ship, &delta(Key::Ship, vec![], vec![copied.clone()]));
    assert!(attr_batches(&mut rx_owner).is_empty(), "copied ship projection emits no drone rows");
    let observed = system.with_combat_fit(DRONE, |fit, _, _|
        (fit.source_type(Key::Projected(11)), fit.value(Key::Ship, ship_attr))).unwrap();
    assert_eq!(observed.0, Some(applied.source_type));
    assert_ne!(observed.1, before_copy);
    // The same source direct onto the drone gives each channel its own item:
    // the delegate sees no drone rows and the owner sees no ship rows.
    let mut direct = applied.clone();
    direct.source_module = 31;
    system.upsert_projection(DRONE, direct.clone());
    assert_batch(&mut rx_owner, DRONE, 2, Key::Drone(DRONE), &delta(Key::Drone(DRONE), vec![], vec![direct.clone()]));
    assert!(attr_batches(&mut rx_delegate).is_empty());
    assert_eq!(system.with_combat_fit(DRONE, |fit, _, _| (
        fit.source_type(Key::Projected(11)), fit.source_type(Key::Projected(31)))).unwrap(),
        (Some(applied.source_type), Some(applied.source_type)));
    // Removing the controller's projection clears only the copied domain.
    system.remove_projection(11);
    assert_batch(&mut rx_delegate, SHIP, 3, Key::Ship, &delta(Key::Ship, vec![copied.clone()], vec![]));
    assert!(attr_batches(&mut rx_owner).is_empty());
    assert_eq!(system.with_combat_fit(DRONE, |fit, _, _| (
        fit.source_type(Key::Projected(11)), fit.source_type(Key::Projected(31)).is_some())).unwrap(),
        (None, true));
    // A consumed character deadline merges into the one batch of a planned
    // projection change; afterwards a renewal stays quiet.
    let now = crate::client::filetime_now();
    let sec = infinity_core::consts::FILETIME_SECOND;
    let inputs = |type_id: i32| CharacterInputs { at: now - 2 * sec,
        installations: vec![crate::dao::skills::expert_systems::ExpertSystem { character_id: 2, type_id,
            installed_at: now - 2 * sec, expires_at: now - sec }], ..Default::default() };
    let mut scaled = direct.clone();
    for v in &mut scaled.values { v.1 *= 1.5; }
    let final_fit = build(drone_type, Key::Drone(DRONE), vec![scaled.clone()]);
    let mut expiry = None;
    for def in sde.expert_systems.iter() {
        let mut before = build(drone_type, Key::Drone(DRONE), vec![direct.clone()]);
        before.apply_character_inputs(&inputs(def.type_id));
        let delta = final_fit.fitting_changes(&before.fitting_values(&before.fitting_candidates(&final_fit)))
            .into_iter().filter(|&(key, ..)| key == Key::Drone(DRONE)).collect::<Vec<_>>();
        if !delta.is_empty() { expiry = Some((before, delta)); break; }
    }
    let (before, expected) = expiry.expect("SDE needs a drone-modifying expert grant");
    let stats = ShipStats::from_fit(&before, Key::Drone(DRONE));
    system.install_condition_with_key(DRONE, Condition { now: stats.max, stats, shield_at: 0 }, before, Key::Drone(DRONE));
    system.upsert_projection(DRONE, scaled.clone());
    assert_batch(&mut rx_owner, DRONE, 2, Key::Drone(DRONE), &expected);
    system.upsert_projection(DRONE, scaled.clone());
    assert!(attr_batches(&mut rx_owner).is_empty(), "consumed expiry does not republish");
}


