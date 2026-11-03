mod physical;
mod logistics;
mod delegation;
pub(in crate::services) mod fleet;

use anyhow::{anyhow, ensure, Result};
use crate::{consts::{*, dogma::*}, services::{effects::{registry, EffectKind}, fit::{Fit, Key}, mining::{self, Field}}};
use super::fixture::Fixture;

#[tokio::test]
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
        for (item, attr, value) in [(module, 77, volume * 2.5), (module, ATTR_MAX_RANGE, 100_000.0),
            (module, 5967, 0.0), (module, 3154, 0.0), (f.ship, 1556, 0.0), (f.ship, ATTR_CAPACITY, volume * 3.5)] {
            f.set(item, attr, value).await;
        }
        let ship = f.ship;
        let position = ball.pos;
        f.world.with(f.system, move |s| s.set_ball_position(ship, position, |_, _| true)).await.ok_or_else(|| anyhow!("system stopped"))?;
        ensure!(f.world.after(f.system, 100).await, "system stopped while positioning");
        let fit = Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.map_err(|e| anyhow!("fit: {e:?}"))?;
        let yield_values = mining::yield_values(&fit, Key::Module(module));
        let (flag, capacity) = mining::destination(&fit);
        ensure!(flag == FLAG_CARGO && capacity >= volume * 3.0 && capacity < volume * 4.0
            && yield_values[0] >= volume * 2.0 && yield_values[0] < volume * 3.0
            && yield_values[1] == 0.0 && yield_values[3] == 0.0,
            "unexpected fitted laser fixture: rock type={} volume={volume:?} flag={flag} capacity={capacity:?} yield={yield_values:?}", ball.type_id);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(f.owner, tx.clone());
        let quantity = |row: &tokio_postgres::Row| row.get::<_, i64>("quantity");
        let initial = quantity(&f.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2", &[&rock, &f.system]).await?);
        ensure!(mining::extract(&f.db, &f.world, &tx, f.owner, f.ship, f.system, &fit, module, module_type, FLAG_HI_SLOT_FIRST, rock).await.map_err(|e| anyhow!("extract: {e:?}"))?.repeat, "initial extraction did not repeat");
        let remaining = quantity(&f.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2", &[&rock, &f.system]).await?);
        let stack = f.db.query_one("SELECT item_id,quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&f.owner, &f.ship, &FLAG_CARGO, &ball.type_id]).await?;
        let item: i64 = stack.get("item_id");
        ensure!(initial - remaining == 2 && stack.get::<_, i32>("quantity") == 2, "first extraction was not two whole units: rock type={} volume={volume:?} capacity={capacity:?} yield={yield_values:?} initial={initial} remaining={remaining} stack={}", ball.type_id, stack.get::<_, i32>("quantity"));
        let mut mined = 0;
        while let Ok((event, _)) = rx.try_recv() { if event == "OnOreMined" { mined += 1; } }
        ensure!(mined == 1, "expected one post-commit OnOreMined");
        ensure!(!mining::extract(&f.db, &f.world, &tx, f.owner, f.ship, f.system, &fit, module, module_type, FLAG_HI_SLOT_FIRST, rock).await.map_err(|e| anyhow!("boundary extraction: {e:?}"))?.repeat, "one-unit boundary must stop");
        let after = quantity(&f.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2", &[&rock, &f.system]).await?);
        ensure!(remaining - after == 1, "one-unit boundary decremented wrong amount: rock type={} volume={volume:?} capacity={capacity:?} yield={yield_values:?} remaining={remaining} after={after}", ball.type_id);
        ensure!(f.db.query_one("SELECT quantity FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&item, &f.owner]).await?.get::<_, i32>("quantity") == 3, "one-unit boundary deposited wrong amount");
        let mut mined = 0; let mut full = false;
        while let Ok((event, args)) = rx.try_recv() { if event == "OnOreMined" { mined += 1; } if event == "OnRemoteMessage" && format!("{args:?}").contains(crate::messages::CARGO_HOLD_FULL) { full = true; } }
        ensure!(mined == 1 && full, "boundary notices missing");
        ensure!(!mining::extract(&f.db, &f.world, &tx, f.owner, f.ship, f.system, &fit, module, module_type, FLAG_HI_SLOT_FIRST, rock).await.map_err(|e| anyhow!("full extraction: {e:?}"))?.repeat, "full hold must stop");
        ensure!(quantity(&f.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2", &[&rock, &f.system]).await?) == after, "full hold consumed rock");
        ensure!(f.db.query_one("SELECT quantity FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&item, &f.owner]).await?.get::<_, i32>("quantity") == 3, "full hold changed stack");
        let mut full = false; let mut mined = 0;
        while let Ok((event, args)) = rx.try_recv() { if event == "OnOreMined" { mined += 1; } if event == "OnRemoteMessage" && format!("{args:?}").contains(crate::messages::CARGO_HOLD_FULL) { full = true; } }
        ensure!(full && mined == 0, "full hold notices incorrect");
        Ok(())
    }.await;
    let cleanup: Result<()> = async {
        let rows = f.db.query("SELECT asteroid_id FROM game.asteroids WHERE system_id=$1 AND field_id=$2", &[&f.system, &f.ship]).await?;
        for row in rows { mining::reconcile_depleted(&f.db, &f.world, row.get("asteroid_id")).await.map_err(|e| anyhow!("reconciling rock: {e:?}"))?; }
        f.db.execute("DELETE FROM game.asteroid_fields WHERE system_id=$1 AND field_id=$2", &[&f.system, &f.ship]).await?;
        Ok(())
    }.await;
    f.cleanup().await;
    cleanup?;
    result
}
