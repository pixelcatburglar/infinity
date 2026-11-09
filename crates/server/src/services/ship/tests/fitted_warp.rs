//! Real SDE and Fit regression for fitted warp speed and destiny output.
//! Client display corruption is not modelled.
use super::*;
use infinity_core::consts::{CATEGORY_IMPLANT, CATEGORY_SHIP, FLAG_IMPLANT, dogma::{ATTR_IMPLANTNESS, ATTR_BASE_WARP_SPEED, ATTR_WARP_SPEED_MULTIPLIER}};
use crate::services::fit::{Fit, Key};
use std::{sync::Arc, time::Duration};


async fn receive_warp(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<Value>>, hull: i64) -> i64 {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            for event in rx.recv().await.expect("observer closed") {
                let Value::Tuple(parts) = event else { continue };
                let Some(Value::Tuple(call)) = parts.get(1) else { continue };
                if call.first() != Some(&Value::str("WarpTo")) { continue; }
                let Some(Value::Tuple(args)) = call.get(1) else { continue };
                if args.first() == Some(&Value::Int(hull)) { return args[5].as_int().unwrap(); }
            }
        }
    }).await.unwrap()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database with SDE; run serially"]
async fn fitted_warp_real_data_evidence() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let system = -1;
    let (account, character, hull) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
        let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        let hull = crate::dao::items::creation::positioned(tx, TYPE_CAPSULE, character, system, [0.0; 3]).await?;
        Ok::<_, crate::db::Error>((account, character, hull))
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    let world = space::World::new(db.clone(), sde.clone(), 1);
    let d = db.clone();
    let w = world.clone();
    let result = tokio::spawn(async move {
        for attr in [ATTR_BASE_WARP_SPEED, ATTR_WARP_SPEED_MULTIPLIER] {
            let default = sde.dogma.attribute(attr).and_then(|a| a.default_value);
            let mut missing = Vec::new();
            let mut count = 0;
            for ship in sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true)) {
                count += 1;
                let value = ship.fit_attribute(attr).or(default).unwrap_or(0.0);
                if !value.is_finite() || value <= 0.0 { missing.push((ship.type_id, value)); }
            }
            eprintln!("published_ship_base_coverage attr={attr} default={default:?} count={count} invalid={missing:?}");
            assert!(count > 0, "SDE contains no published ships");
            assert!(missing.is_empty(), "published ships have invalid warp attribute {attr}: {missing:?}");
        }
        admit_fixture_ship(&d, &w, system, character, hull, true).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        w.observe(system, character, hull, tx).await.unwrap();
        let point = [149_597_870_700.0, 0.0, 0.0];
        assert!(warp_pilot(&d, &w, system, character, hull, WarpDestination::Exact(point), None, None).await.unwrap());
        let baseline_wire = receive_warp(&mut rx, hull).await;
        let baseline_state = w.ball(system, hull).await.unwrap().warp.factor;
        eprintln!("baseline emitted_wire={baseline_wire} actor_warp_factor={baseline_state}");
        assert_eq!(baseline_wire, 4000);
        assert_eq!(baseline_state, baseline_wire);
        let baseline = Fit::load_in_space(&d, &w, system, character, hull).await.ok().unwrap();
        let before = [baseline.value(Key::Ship, ATTR_BASE_WARP_SPEED), baseline.value(Key::Ship, ATTR_WARP_SPEED_MULTIPLIER)];
        assert!(before.iter().all(|v| v.is_finite() && *v > 0.0));
        let baseline_factor = fitted_warp_factor(&Fit::load(&d, sde.clone(), character, hull).await.unwrap()).unwrap();
        assert_eq!(baseline_factor, baseline_wire);
        let mut queued = space::System::new(system, Arc::default(), 12_345);
        let mut control = space::System::new(system, Arc::default(), 12_345);
        let mut queued_ball = w.ball(system, hull).await.unwrap();
        queued_ball.stop();
        queued_ball.pos = [0.0; 3];
        queued_ball.vel = [0.0; 3];
        control.insert(queued_ball.clone());
        queued.insert(queued_ball);
        for s in [&mut queued, &mut control] {
            let fit = Fit::load(&d, sde.clone(), character, hull).await.unwrap();
            let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, Key::Ship);
            s.install_condition(hull, crate::services::combat::groups_health::Condition {
                now: stats.max, stats, shield_at: crate::client::filetime_now(),
            }, fit);
        }
        for s in [&mut queued, &mut control] {
            let revision = s.projection_revision(hull);
            assert_eq!(commit_warp(s, hull, revision, false, baseline_factor, WarpDestination::Exact(point), None), WarpCommit::Queued);
        }
        // This ordinary warp-speed implant is also used by the fleet regression.
        let data = sde.types.iter().find(|t| t.type_id == 3117).expect("warp speed implant");
        assert_eq!(data.category_id, Some(CATEGORY_IMPLANT));
        assert_eq!(data.published, Some(true));
        let slot = data.attribute(ATTR_IMPLANTNESS).unwrap();
        assert!(slot >= 1.0 && slot.fract() == 0.0);
        let (effect, modifier) = [ATTR_BASE_WARP_SPEED, ATTR_WARP_SPEED_MULTIPLIER].into_iter()
            .flat_map(|attr| data.modifiers(&sde.dogma, attr))
            .find(|(effect, modifier)| effect.effect_category.unwrap_or(0) == 0
                && modifier.func == "ItemModifier" && modifier.domain == "shipID"
                && !effect.fitting_usage_chance_attribute_id.is_some_and(|id| data.attribute(id).is_some()))
            .expect("ordinary passive ship warp modifier");
        let implant: i64 = d.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,brain_slot) VALUES ($1,$2,$2,$3,1,true,$4) RETURNING item_id", &[&data.type_id, &character, &FLAG_IMPLANT, &(slot as i32)]).await.unwrap().get("item_id");
        let modified = Fit::load(&d, sde.clone(), character, hull).await.unwrap();
        let after = [modified.value(Key::Ship, ATTR_BASE_WARP_SPEED), modified.value(Key::Ship, ATTR_WARP_SPEED_MULTIPLIER)];
        let modified_factor = fitted_warp_factor(&modified).unwrap();
        eprintln!("implant type={} name={:?} slot={slot} effect={} name={:?} category={:?} func={} domain={} op={} modified={} modifying={} source_raw={:?} source_effective={}", data.type_id, data.name, effect.effect_id, effect.name, effect.effect_category, modifier.func, modifier.domain, modifier.op, modifier.modified, modifier.modifying, data.attribute(modifier.modifying), modified.value(Key::Implant(implant), modifier.modifying));
        assert_ne!(modified_factor, baseline_factor, "fixture must expose an integer discrepancy");
        assert_ne!(before, after);
        // The implant is installed after enqueue but before the first action boundary.
        assert_eq!(queued.ball(hull).unwrap().warp.factor, control.ball(hull).unwrap().warp.factor);
        queued.tick(13_000);
        control.tick(13_000);
        assert_eq!(queued.ball(hull).unwrap().warp.factor, baseline_factor);
        assert_eq!((queued.ball(hull).unwrap().pos, queued.ball(hull).unwrap().vel),
            (control.ball(hull).unwrap().pos, control.ball(hull).unwrap().vel));
        assert!(!queued.ball(hull).unwrap().is_warping(), "fixture must observe alignment");
        d.execute("DELETE FROM game.items WHERE item_id=$1", &[&implant]).await.unwrap();
        let aligned_fit = Fit::load(&d, sde.clone(), character, hull).await.unwrap();
        assert_eq!(fitted_warp_factor(&aligned_fit).unwrap(), baseline_factor);
        assert_ne!(fitted_warp_factor(&aligned_fit).unwrap(), modified_factor);
        assert_eq!(queued.ball(hull).unwrap().warp.factor, baseline_factor);
        let mut ms = 13_000;
        for _ in 0..500 {
            if queued.ball(hull).unwrap().is_warping() { break; }
            ms += 1000;
            queued.tick(ms);
            control.tick(ms);
            let (actual, expected) = (queued.ball(hull).unwrap(), control.ball(hull).unwrap());
            assert_eq!((actual.mode, actual.effect_stamp, actual.pos, actual.vel, actual.warp.factor),
                (expected.mode, expected.effect_stamp, expected.pos, expected.vel, expected.warp.factor));
        }
        assert!(queued.ball(hull).unwrap().is_warping(), "warp did not enter flight");
        d.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,brain_slot) VALUES ($1,$2,$2,$3,1,true,$4) RETURNING item_id", &[&data.type_id, &character, &FLAG_IMPLANT, &(slot as i32)]).await.unwrap();
        let inflight_fit = Fit::load(&d, sde.clone(), character, hull).await.unwrap();
        assert_eq!(fitted_warp_factor(&inflight_fit).unwrap(), modified_factor);
        assert_ne!(modified_factor, baseline_factor);
        let mut exited = false;
        for _ in 0..500 {
            ms += 1000;
            queued.tick(ms);
            control.tick(ms);
            let (actual, expected) = (queued.ball(hull).unwrap(), control.ball(hull).unwrap());
            assert_eq!((actual.mode, actual.effect_stamp, actual.pos, actual.vel, actual.warp.factor, actual.warp.length),
                (expected.mode, expected.effect_stamp, expected.pos, expected.vel, expected.warp.factor, expected.warp.length));
            assert_eq!(actual.warp.factor, baseline_factor);
            if !actual.is_warping() { exited = true; break; }
        }
        assert!(exited, "warp did not exit");
        let return_point = [0.0; 3];
        for (s, factor) in [(&mut queued, fitted_warp_factor(&inflight_fit).unwrap()), (&mut control, baseline_factor)] {
            let revision = s.projection_revision(hull);
            assert_eq!(commit_warp(s, hull, revision, false, factor, WarpDestination::Exact(return_point), None), WarpCommit::Queued);
        }
        let mut diverged = false;
        for _ in 0..500 {
            ms += 1000;
            queued.tick(ms);
            control.tick(ms);
            let (actual, expected) = (queued.ball(hull).unwrap(), control.ball(hull).unwrap());
            assert_eq!(actual.warp.factor, modified_factor);
            assert_eq!(expected.warp.factor, baseline_factor);
            if actual.is_warping() && expected.is_warping()
                && (actual.pos != expected.pos || actual.vel != expected.vel) {
                diverged = true;
                break;
            }
        }
        assert!(diverged, "new fitted speed must change the next trajectory");
        assert_eq!(w.ball(system, hull).await.unwrap().warp.factor, baseline_wire);
        admit_fixture_ship(&d, &w, system, character, hull, true).await;
        let (tx, new_rx) = tokio::sync::mpsc::unbounded_channel();
        w.observe(system, character, hull, tx).await.unwrap();
        rx = new_rx;
        assert!(warp_pilot(&d, &w, system, character, hull, WarpDestination::Exact(point), None, None).await.unwrap());
        let wire = receive_warp(&mut rx, hull).await;
        let state = w.ball(system, hull).await.unwrap().warp.factor;
        eprintln!("modified emitted_wire={wire} actor_warp_factor={state}");
        assert_eq!(wire, modified_factor);
        assert_eq!(state, wire);
        for (base, multiplier) in [(-1.0, -1.0), (0.0, 1.0), (f64::INFINITY, 1.0), (f64::MAX, f64::MAX)] {
            assert!(crate::dao::items::dogma::set_override(&d, hull, ATTR_BASE_WARP_SPEED, base).await.unwrap());
            assert!(crate::dao::items::dogma::set_override(&d, hull, ATTR_WARP_SPEED_MULTIPLIER, multiplier).await.unwrap());
            let fit = Fit::load_in_space(&d, &w, system, character, hull).await.ok().unwrap();
            assert!(fitted_warp_factor(&fit).is_err());
        }
        assert!(crate::dao::items::dogma::set_override(&d, hull, ATTR_BASE_WARP_SPEED, 0.0001).await.unwrap());
        assert!(crate::dao::items::dogma::set_override(&d, hull, ATTR_WARP_SPEED_MULTIPLIER, 1.0).await.unwrap());
        let fit = Fit::load_in_space(&d, &w, system, character, hull).await.ok().unwrap();
        assert_eq!(fitted_warp_factor(&fit).unwrap(), MIN_SHIP_WARP_FACTOR);
    }).await;
    world.remove_ball(system, hull).await;
    db.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}
