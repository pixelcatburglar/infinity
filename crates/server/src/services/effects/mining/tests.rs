use super::*;

#[test]
fn extraction_controls_cycle_continuation() {
    use crate::lua::events::ModuleCycleOutcome;
    use crate::services::mining::Extraction;
    assert_eq!(extraction_outcome(Extraction { repeat: true, outcome: Some(ModuleCycleOutcome::Completed), error: None }), CycleOutcome::Continue);
    assert_eq!(extraction_outcome(Extraction { repeat: false, outcome: None, error: None }), CycleOutcome::Stop("mining finished"));
    for outcome in [ModuleCycleOutcome::Completed, ModuleCycleOutcome::TargetLost, ModuleCycleOutcome::CargoFull] {
        assert_eq!(extraction_outcome(Extraction { repeat: false, outcome: Some(outcome), error: None }), CycleOutcome::Ended { reason: "mining finished", outcome });
        assert_eq!(extraction_outcome(Extraction { repeat: false, outcome: Some(outcome), error: Some(CallError::Internal(anyhow::anyhow!("reconcile"))) }), CycleOutcome::Ended { reason: "mining failed", outcome });
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn validation_uses_visible_system_owned_rock() -> anyhow::Result<()> {
    use anyhow::{anyhow, ensure};
    use infinity_core::consts::{CATEGORY_MODULE, FLAG_HI_SLOT_FIRST, dogma::{ATTR_MAX_LOCKED_TARGETS, ATTR_MAX_RANGE, ATTR_MAX_TARGET_RANGE, ATTR_SCAN_RESOLUTION}};
    use crate::services::{combat::{self, targeting_charges}, drones::tests::fixture::Fixture, effects::{registry, EffectKind}, mining};
    use crate::space::{asteroids::Field, World};
    let f = Fixture::new().await;
    let result: anyhow::Result<()> = async {
        let rocks = mining::grow_field(&f.db, &f.world, f.system, f.ship, [0.0; 3], Field::Ore, 1.0, false).await.map_err(|e| anyhow!("grow: {e:?}"))?;
        let rock = *rocks.first().ok_or_else(|| anyhow!("empty field"))?;
        let ball = f.world.ball(f.system, rock).await.ok_or_else(|| anyhow!("missing rock"))?;
        let module_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
            && t.effects().iter().any(|e| registry::resolve(&f.sde, e.effect_id).is_some_and(|effect| effect.kind == EffectKind::Mining)))
            .ok_or_else(|| anyhow!("missing mining module"))?.type_id;
        let module_id: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&module_type, &f.owner, &f.ship, &FLAG_HI_SLOT_FIRST]).await?.get("item_id");
        for (item, attr, value) in [(f.ship, ATTR_MAX_TARGET_RANGE, 1_000_000.0), (f.ship, ATTR_SCAN_RESOLUTION, 1_000_000.0),
            (f.ship, ATTR_MAX_LOCKED_TARGETS, 10.0), (module_id, ATTR_MAX_RANGE, 1_000_000.0)] { f.set(item, attr, value).await; }
        let ship = f.ship;
        let position = ball.pos;
        f.world.with(f.system, move |s| s.set_ball_position(ship, position, |_, _| true)).await.ok_or_else(|| anyhow!("system stopped"))?;
        ensure!(f.world.after(f.system, 100).await, "position action failed");
        let cx = combat::ctx(&f.client).ok_or_else(|| anyhow!("missing context"))?;
        targeting_charges::add_target_ctx(&cx, rock, false).await.map_err(|e| anyhow!("target lock: {e:?}"))?;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !targeting_charges::targets_of(f.owner).contains(&rock) {
                ensure!(f.world.after(f.system, 100).await, "system stopped");
            }
            Ok::<_, anyhow::Error>(())
        }).await??;
        let fit = Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.map_err(|e| anyhow!("fit: {e:?}"))?;
        let module = Module { id: module_id, type_id: module_type, flag: FLAG_HI_SLOT_FIRST };
        let error = validate(&cx, &fit, &module, None).await.unwrap_err();
        ensure!(matches!(error, CallError::User { ref msg, .. } if msg == messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT), "missing lock reported wrong refusal: {error:?}");
        ensure!(validate(&cx, &fit, &module, Some(rock)).await.is_ok(), "valid rock refused");
        let mut wrong = ball.clone();
        wrong.type_id = f.world.ball(f.system, ship).await.ok_or_else(|| anyhow!("missing ship"))?.type_id;
        f.world.with(f.system, move |s| s.insert(wrong)).await.ok_or_else(|| anyhow!("system stopped"))?;
        let error = validate(&cx, &fit, &module, Some(rock)).await.unwrap_err();
        ensure!(format!("{error:?}").contains(messages::MINING_MODULE_TARGET_INVALID), "mismatched ball was not invalid: {error:?}");
        let original = ball.clone();
        f.world.with(f.system, move |s| s.insert(original)).await.ok_or_else(|| anyhow!("system stopped"))?;
        f.world.with(f.system, move |s| s.remove_ball(rock)).await.ok_or_else(|| anyhow!("system stopped"))?;
        let error = validate(&cx, &fit, &module, Some(rock)).await.unwrap_err();
        ensure!(format!("{error:?}").contains(messages::MINING_MODULE_TARGET_INVALID), "metadata without ball accepted: {error:?}");
        let original = ball.clone();
        f.world.with(f.system, move |s| s.insert(original)).await.ok_or_else(|| anyhow!("system stopped"))?;
        let other = f.system + 1;
        let ship_ball = f.world.ball(f.system, ship).await.ok_or_else(|| anyhow!("missing ship"))?;
        f.world.add_ship(other, ship_ball).await.map_err(|e| anyhow!("other system: {e:?}"))?;
        let mut elsewhere = cx.clone();
        elsewhere.system_id = other;
        let error = validate(&elsewhere, &fit, &module, Some(rock)).await.unwrap_err();
        ensure!(format!("{error:?}").contains(messages::MINING_MODULE_TARGET_INVALID), "other system accepted rock: {error:?}");
        elsewhere.system_id = f.system;
        elsewhere.world = World::new(f.db.clone(), f.sde.clone(), 1);
        let error = validate(&elsewhere, &fit, &module, Some(rock)).await.unwrap_err();
        ensure!(format!("{error:?}").contains(messages::MINING_MODULE_TARGET_INVALID), "other world accepted rock: {error:?}");
        f.set(module_id, ATTR_MAX_RANGE, 0.0).await;
        let fit = Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.map_err(|e| anyhow!("fit: {e:?}"))?;
        f.world.with(f.system, move |s| s.set_ball_position(ship, [1_000_000.0; 3], |_, _| true)).await.ok_or_else(|| anyhow!("system stopped"))?;
        ensure!(f.world.after(f.system, 100).await, "position action failed");
        let error = validate(&cx, &fit, &module, Some(rock)).await.unwrap_err();
        ensure!(format!("{error:?}").contains(&messages::mining_range(0.0)), "out-of-range rock did not report range: {error:?}");
        Ok(())
    }.await;
    let cleanup: anyhow::Result<()> = async {
        let rows = f.db.query("SELECT asteroid_id FROM game.asteroids WHERE system_id=$1 AND field_id=$2", &[&f.system, &f.ship]).await?;
        let actor = f.world.asteroid_actor(f.system).ok_or_else(|| anyhow!("cleanup actor stopped"))?;
        for row in rows {
            let id: i64 = row.get("asteroid_id");
            let field = actor.rock_field(id).await.ok_or_else(|| anyhow!("cleanup rock not owned by actor"))?;
            ensure!(field == f.ship, "cleanup rock has wrong field");
            ensure!(f.db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&id, &f.system, &field]).await? == 1, "cleanup rock was not deleted");
            ensure!(crate::space::asteroids::publish_depletion(&f.world, f.system, &actor, id, field, None, None).await, "cleanup publication failed");
        }
        f.db.execute("DELETE FROM game.asteroid_fields WHERE system_id=$1 AND field_id=$2", &[&f.system, &f.ship]).await?;
        Ok(())
    }.await;
    f.cleanup().await;
    cleanup?;
    result
}
