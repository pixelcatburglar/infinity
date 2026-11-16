#[cfg(feature = "tests-combat")] mod physical;
#[cfg(feature = "tests-combat")] mod logistics;
#[cfg(feature = "tests-combat")] mod delegation;
#[cfg(any(feature = "tests-combat", feature = "tests-economy", feature = "tests-ships", feature = "tests-effects"))] pub(in crate::services) mod fleet;

#[cfg(feature = "tests-combat")] use anyhow::{anyhow, ensure, Result};
#[cfg(feature = "tests-combat")] use infinity_core::{consts::{*, dogma::*}};
#[cfg(feature = "tests-combat")] use crate::{services::{effects::{registry, EffectKind}, fit::{Fit, Key}, mining::{self, ATTR_MINING_AMOUNT}}, space::asteroids::Field};
#[cfg(feature = "tests-combat")] use super::fixture::Fixture;

#[cfg(feature = "tests-combat")]
async fn remove_fixture_rock(db: &crate::db::Db, world: &crate::space::World, system: i64, field: i64, rock: i64) -> Result<()> {
    let actor = world.asteroid_actor(system).ok_or_else(|| anyhow!("fixture system stopped"))?;
    ensure!(actor.rock_field(rock).await == Some(field), "fixture rock belongs to another field or actor");
    db.execute("DELETE FROM game.asteroids WHERE system_id=$1 AND field_id=$2 AND asteroid_id=$3", &[&system, &field, &rock]).await?;
    ensure!(crate::space::asteroids::publish_depletion(world, system, &actor, rock, field, None, None).await, "fixture rock removal was not applied");
    Ok(())
}

#[cfg(feature = "tests-combat")] #[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn laser_generated_rock_full_hold_and_one_unit_boundary() -> Result<()> {
    let f = Fixture::new().await;
    let result: Result<()> = async {
        let rocks = mining::grow_field(&f.db, &f.world, f.system, f.ship, [0.0; 3], Field::Ore, 1.0, false)
            .await.map_err(|e| anyhow!("growing field: {e:?}"))?;
        let rock = *rocks.first().ok_or_else(|| anyhow!("generated field has no rocks"))?;
        let ball = f.world.ball(f.system, rock).await.ok_or_else(|| anyhow!("generated rock has no ball"))?;
        let volume = f.sde.types.get(ball.type_id).and_then(|t| t.volume).unwrap_or(1.0).max(0.01);
        let module_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
            && t.effects().iter().any(|e| registry::resolve(&f.sde, e.effect_id).is_some_and(|effect| effect.kind == EffectKind::Mining)))
            .ok_or_else(|| anyhow!("no published mining laser"))?.type_id;
        let module: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&module_type, &f.owner, &f.ship, &FLAG_HI_SLOT_FIRST]).await?.get("item_id");
        // A whole-unit yield makes the client's randomized rounding independent of its draw.
        for (item, attr, value) in [(module, 77, volume * 2.0), (module, ATTR_MAX_RANGE, 100_000.0),
            (module, 5967, 0.0), (module, 3154, 0.0), (f.ship, ATTR_GENERAL_MINING_HOLD_CAPACITY, 0.0), (f.ship, ATTR_CAPACITY, volume * 3.5)] {
            f.set(item, attr, value).await;
        }
        let ship = f.ship;
        let position = ball.pos;
        f.world.with(f.system, move |s| s.set_ball_position(ship, position, |_, _| true)).await.ok_or_else(|| anyhow!("system stopped"))?;
        ensure!(f.world.after(f.system, 100).await, "system stopped while positioning");
        let fit = Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.map_err(|e| anyhow!("fit: {e:?}"))?;
        let yield_values = mining::yield_values(&fit, Key::Module(module));
        let destination = mining::destination(&fit, ball.type_id).map_err(|e| anyhow!("destination: {e:?}"))?;
        let capacity = match crate::services::inventory::holds::effective_capacity(&fit, destination.flag()) {
            crate::services::inventory::holds::CapacityLimit::Finite(value) => value,
            crate::services::inventory::holds::CapacityLimit::Unlimited => panic!("fixture cargo must be finite"),
        };
        let flag = destination.flag();
        ensure!(flag == FLAG_CARGO && capacity >= volume * 3.0 && capacity < volume * 4.0
            && yield_values[0] == volume * 2.0
            && yield_values[1] == 0.0 && yield_values[3] == 0.0,
            "unexpected fitted laser fixture: rock type={} volume={volume:?} flag={flag} capacity={capacity:?} yield={yield_values:?}", ball.type_id);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(f.owner, tx.clone());
        let quantity = |row: &tokio_postgres::Row| row.get::<_, i64>("quantity");
        let initial = quantity(&f.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2", &[&rock, &f.system]).await?);
        let inputs = || crate::services::effects::parameters::MiningInputs {
            range: fit.value(Key::Module(module), ATTR_MAX_RANGE),
            values: mining::yield_values(&fit, Key::Module(module)),
            destination: mining::destination(&fit, ball.type_id).map(|capacity| Some((capacity.flag(), capacity))),
        };
        ensure!(mining::extract(&f.db, &f.world, &tx, f.owner, f.ship, f.system, inputs(), module, module_type, FLAG_HI_SLOT_FIRST, rock).await.map_err(|e| anyhow!("extract: {e:?}"))?.repeat, "initial extraction did not repeat");
        let remaining = quantity(&f.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2", &[&rock, &f.system]).await?);
        let stack = f.db.query_one("SELECT item_id,quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&f.owner, &f.ship, &FLAG_CARGO, &ball.type_id]).await?;
        let item: i64 = stack.get("item_id");
        ensure!(initial - remaining == 2 && stack.get::<_, i64>("quantity") == 2, "first extraction was not two whole units: rock type={} volume={volume:?} capacity={capacity:?} yield={yield_values:?} initial={initial} remaining={remaining} stack={}", ball.type_id, stack.get::<_, i64>("quantity"));
        let mut mined = 0;
        while let Ok((event, _)) = rx.try_recv() { if event == "OnOreMined" { mined += 1; } }
        ensure!(mined == 1, "expected one post-commit OnOreMined");
        ensure!(!mining::extract(&f.db, &f.world, &tx, f.owner, f.ship, f.system, inputs(), module, module_type, FLAG_HI_SLOT_FIRST, rock).await.map_err(|e| anyhow!("boundary extraction: {e:?}"))?.repeat, "one-unit boundary must stop");
        let after = quantity(&f.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2", &[&rock, &f.system]).await?);
        ensure!(remaining - after == 1, "one-unit boundary decremented wrong amount: rock type={} volume={volume:?} capacity={capacity:?} yield={yield_values:?} remaining={remaining} after={after}", ball.type_id);
        ensure!(f.db.query_one("SELECT quantity FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&item, &f.owner]).await?.get::<_, i64>("quantity") == 3, "one-unit boundary deposited wrong amount");
        let mut mined = 0; let mut full = false;
        while let Ok((event, args)) = rx.try_recv() { if event == "OnOreMined" { mined += 1; } if event == "OnRemoteMessage" && format!("{args:?}").contains(infinity_core::messages::CARGO_HOLD_FULL) { full = true; } }
        ensure!(mined == 1 && full, "boundary notices missing");
        ensure!(!mining::extract(&f.db, &f.world, &tx, f.owner, f.ship, f.system, inputs(), module, module_type, FLAG_HI_SLOT_FIRST, rock).await.map_err(|e| anyhow!("full extraction: {e:?}"))?.repeat, "full hold must stop");
        ensure!(quantity(&f.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2", &[&rock, &f.system]).await?) == after, "full hold consumed rock");
        ensure!(f.db.query_one("SELECT quantity FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&item, &f.owner]).await?.get::<_, i64>("quantity") == 3, "full hold changed stack");
        let mut full = false; let mut mined = 0;
        while let Ok((event, args)) = rx.try_recv() { if event == "OnOreMined" { mined += 1; } if event == "OnRemoteMessage" && format!("{args:?}").contains(infinity_core::messages::CARGO_HOLD_FULL) { full = true; } }
        ensure!(full && mined == 0, "full hold notices incorrect");
        f.db.execute("DELETE FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&item, &f.owner]).await?;
        for (skill, level) in crate::services::skills::requirements::requirements(&f.sde, module_type) {
            let rank = f.sde.types.get(skill).and_then(|ty| ty.fit_attribute(ATTR_SKILL_TIME_CONSTANT)).unwrap();
            let points = crate::services::skills::formulas::sp_for_level(rank, i64::from(level));
            f.db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,$4) ON CONFLICT (character_id,type_id) DO UPDATE SET level=GREATEST(game.skills.level,EXCLUDED.level),skill_points=GREATEST(game.skills.skill_points,EXCLUDED.skill_points)",
                &[&f.owner, &skill, &level, &points]).await?;
        }
        let effect = f.sde.types.effects(module_type).iter().filter_map(|row| registry::resolve(&f.sde, row.effect_id))
            .find(|effect| effect.kind == EffectKind::Mining).unwrap();
        let name = f.sde.dogma.effect(effect.id as i32).unwrap().name.clone().unwrap();
        let duration = effect.attribute(registry::EffectAttribute::Duration).unwrap();
        for (id, attribute, value) in [(f.ship, ATTR_MAX_TARGET_RANGE, 1e9),
            (f.ship, ATTR_SCAN_RESOLUTION, 1e9), (f.ship, ATTR_MAX_LOCKED_TARGETS, 10.0),
            (f.ship, ATTR_CAPACITY, volume * 10.0), (module, ATTR_MINING_AMOUNT, volume),
            (module, duration, 2000.0), (module, effect.attribute(registry::EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED), 0.0)] {
            f.set(id, attribute, value).await;
        }
        let effective_amount = Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.map_err(|e| anyhow!("calibration fit: {e:?}"))?.value(Key::Module(module), ATTR_MINING_AMOUNT);
        ensure!(effective_amount.is_finite() && effective_amount > 0.0, "invalid effective mining amount: {effective_amount}");
        let whole_unit_amount = volume * volume / effective_amount;
        f.set(module, ATTR_MINING_AMOUNT, whole_unit_amount).await;
        let resident = Fit::load_in_space(&f.db, &f.world, f.system, f.owner, f.ship).await.map_err(|e| anyhow!("cold scheduled fit: {e:?}"))?;
        let condition = crate::services::combat::prepare_condition_with_fit(&f.db, f.ship, &resident).await.map_err(|e| anyhow!("condition: {e:?}"))?;
        f.world.with(f.system, move |s| s.install_condition(ship, condition.into_condition(), resident)).await.ok_or_else(|| anyhow!("actor stopped"))?;
        let cx = crate::services::combat::Ctx { db: f.db.clone(), world: f.world.clone(), notify: tx.clone(),
            char_id: f.owner, ship_id: f.ship, system_id: f.system };
        crate::services::combat::targeting_charges::add_target_ctx(&cx, rock, true).await.map_err(|e| anyhow!("lock: {e:?}"))?;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !crate::services::combat::targeting_charges::targets_of(f.owner).contains(&rock) {
                ensure!(f.world.after(f.system, 100).await, "actor stopped");
            }
            Ok::<_, anyhow::Error>(())
        }).await.map_err(|_| anyhow!("mining lock timed out"))??;
        while rx.try_recv().is_ok() {}
        let identity = crate::services::identity::Identity::of_client(&f.client);
        crate::services::effects::lifecycle::activate(&identity, &f.db, &f.world, &f.sde, Some(f.system),
            tx.clone(), f.client.login.user_id, module, &name, Some(rock), 2, |_| Ok(()))
            .await.map_err(|e| anyhow!("mining activation: {e:?}"))?;
        let mut completed = 0;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while completed < 2 {
                let (name, _) = rx.recv().await.ok_or_else(|| anyhow!("mining notifications closed"))?;
                if name == "OnOreMined" { completed += 1; }
            }
            while crate::services::combat::state().active.contains_key(&module) {
                ensure!(f.world.after(f.system, 100).await, "actor stopped");
            }
            Ok::<_, anyhow::Error>(())
        }).await.map_err(|_| anyhow!("two scheduled mining cycles timed out"))??;
        ensure!(completed == 2, "wrong scheduled completion count");
        let deposited: i64 = f.db.query_one("SELECT quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4",
            &[&f.owner, &f.ship, &FLAG_CARGO, &ball.type_id]).await?.get("quantity");
        ensure!(deposited == 2, "scheduled mining did not deliver one unit twice");
        Ok(())
    }.await;
    let cleanup: Result<()> = async {
        let rows = f.db.query("SELECT asteroid_id FROM game.asteroids WHERE system_id=$1 AND field_id=$2", &[&f.system, &f.ship]).await?;
        for row in rows { remove_fixture_rock(&f.db, &f.world, f.system, f.ship, row.get("asteroid_id")).await?; }
        f.db.execute("DELETE FROM game.asteroid_fields WHERE system_id=$1 AND field_id=$2", &[&f.system, &f.ship]).await?;
        Ok(())
    }.await;
    f.cleanup().await;
    cleanup?;
    result
}
