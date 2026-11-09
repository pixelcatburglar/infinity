//! Bounded online daily-goal rollover using the shared durable scheduler.
//! Client daily periods use UTC boundaries from game daily-goal persistence.
//! Offline characters are not assigned missed periods; login handles their current period.
use std::{future::Future, pin::Pin, sync::Arc};
use serde::{Deserialize, Serialize};
use crate::{client::filetime_now, config, dao::{daily_goals::periods, jobs}, db::Db, lua, scheduler::{self, Job}, space::World};
use super::{assignment, Resources};

/// Server design: one durable cursor shared by all online characters.
pub(crate) const KIND: &str = "periodic:daily_goals.rollover";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor { day_start: i64, after_character: Option<i64> }

fn cursor(payload: &str, day_start: i64) -> anyhow::Result<Cursor> {
    if payload == "{}" { return Ok(Cursor { day_start, after_character: None }); }
    let cursor: Cursor = serde_json::from_str(payload)?;
    anyhow::ensure!(cursor.after_character.is_none_or(|id| id > 0), "invalid daily rollover cursor");
    Ok(if cursor.day_start == day_start { cursor } else { Cursor { day_start, after_character: None } })
}

pub(crate) async fn seed(db: &Db) -> Result<(), crate::db::Error> {
    let now = filetime_now();
    jobs::seed(db, &Job { id: KIND.into(), kind: KIND.into(), due_at: now, payload: "{}".into() }, now).await?;
    scheduler::wake();
    Ok(())
}

pub(crate) fn run(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    run_batch(db, world, job, config::game().daily_goals.rollover_batch_size)
}

fn run_batch(db: Db, world: Arc<World>, job: Job, batch: usize) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        anyhow::ensure!(job.id == KIND && job.kind == KIND, "invalid daily rollover job identity");
        let now = filetime_now();
        let bounds = periods::bounds(&db, now).await?;
        let cursor = cursor(&job.payload, bounds.day_start)?;
        let mut ids = world.online_pilots();
        ids.retain(|id| *id > 0 && cursor.after_character.is_none_or(|last| *id > last));
        ids.sort_unstable();
        let more = ids.len() > batch;
        if lua::enabled() {
            let resources = Resources::from_world(&world).map_err(|error| anyhow::anyhow!("daily rollover resources: {error:?}"))?;
            for &character in ids.iter().take(batch) {
                let Some(session) = world.online_session(character) else { continue };
                let Some(user) = session.user_id.filter(|id| *id > 0) else { continue };
                assignment::ensure_at(&resources, user, character, filetime_now(), true).await
                    .map_err(|error| anyhow::anyhow!("daily rollover character {character}: {error:?}"))?;
            }
        }
        let now = filetime_now();
        let current = periods::bounds(&db, now).await?;
        let (next, due_at) = if current.day_start != bounds.day_start {
            (Cursor { day_start: current.day_start, after_character: None }, now)
        } else if more && lua::enabled() {
            (Cursor { day_start: bounds.day_start, after_character: ids.get(batch - 1).copied() }, now)
        } else {
            (Cursor { day_start: bounds.day_start, after_character: None }, bounds.day_end)
        };
        scheduler::schedule(&db, &Job { id: KIND.into(), kind: KIND.into(), due_at, payload: serde_json::to_string(&next)? }).await?;
        scheduler::wake();
        Ok(())
    })
}

#[cfg(test)] mod tests;
