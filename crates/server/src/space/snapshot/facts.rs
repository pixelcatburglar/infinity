//! Frozen combat facts and corresponding persistent condition checkpoints.
//! Server-side actor snapshots have no client protocol or independent timer.
//! Pending batches retain their original identities across transaction retries.
use super::*;
use crate::db::{executor::Executor, outbox::{Outbox, SourceKey}};
use crate::space::combat_facts::{CapturedFacts, Refusal};
use std::collections::HashSet;

pub(crate) struct FactCheckpoint {
    batches: Vec<Arc<CapturedFacts>>,
    conditions: Vec<combat::DirtyConditionCapture>,
}

pub(crate) fn capture_fact_checkpoint(s: &mut System) -> std::result::Result<Option<Arc<FactCheckpoint>>, Refusal> {
    prepare_fact_checkpoint(s).map(|(frame, _)| frame)
}

pub(super) fn prepare_fact_checkpoint(s: &mut System) -> std::result::Result<(Option<Arc<FactCheckpoint>>, i64), Refusal> {
    s.observe_combat_utc()?;
    s.checkpoint_combat_scrambles()?;
    let at = s.combat_observed_time();
    Ok((capture_fact_checkpoint_at(s, at)?, at))
}

pub(super) fn capture_fact_checkpoint_at(s: &mut System, at: i64) -> std::result::Result<Option<Arc<FactCheckpoint>>, Refusal> {
    let Some(facts) = s.combat_facts.as_mut() else { return Ok(None) };
    match facts.capture(at) {
        Ok(_) => {},
        Err(Refusal::Full) if facts.batch_count() != 0 => {},
        Err(error) => return Err(error),
    }
    let batches = facts.pending_batches();
    if batches.is_empty() { return Ok(None); }
    let conditions = s.persistent_balls().filter_map(|ball| s.capture_dirty_condition(ball.id, true, at)).collect();
    Ok(Some(Arc::new(FactCheckpoint { batches, conditions })))
}

impl FactCheckpoint {
    pub(crate) fn events(&self) -> impl Iterator<Item = crate::space::combat_facts::CapturedEvent> + '_ {
        self.batches.iter().flat_map(|batch| batch.events())
    }

    pub(crate) fn append<E>(&self, outbox: &mut Outbox<E>) {
        for captured in self.events() {
            let key = SourceKey::canonical(captured.event.kind(), captured.identity, captured.ordinal);
            outbox.event_from(key, captured.event);
        }
    }

    pub(crate) fn condition_rows(&self) -> impl Iterator<Item = (i64, [f64; 3], i64)> + '_ {
        self.conditions.iter().map(|c| (c.item_id, [c.shield, c.armor_damage, c.hull_damage], c.sampled_at))
    }
    /// Cold reconciliation only: acknowledge collateral HP only when its frozen
    /// values and shield timestamp are actually present in the durable rows.
    pub(crate) async fn verified_condition_ids(&self, ex: &(impl Executor + ?Sized)) -> std::result::Result<HashSet<i64>, crate::db::Error> {
        let ids: Vec<_> = self.conditions.iter().map(|condition| condition.item_id).collect();
        if ids.is_empty() { return Ok(HashSet::new()); }
        let mut rows = crate::dao::items::dogma::overrides_for(ex, &ids).await?;
        rows.sort_unstable_by_key(|row| (row.item_id, row.attribute_id));
        let attrs = [infinity_core::consts::dogma::ATTR_SHIELD_CHARGE, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, infinity_core::consts::dogma::ATTR_HULL_DAMAGE];
        Ok(self.conditions.iter().filter(|condition| {
            let values = [condition.shield, condition.armor_damage, condition.hull_damage];
            attrs.iter().enumerate().all(|(i, attribute)| {
                rows.binary_search_by_key(&(condition.item_id, *attribute), |row| (row.item_id, row.attribute_id))
                    .ok().is_some_and(|index| {
                        let row = &rows[index];
                        row.value == values[i] && row.updated_at == if i == 0 { condition.sampled_at } else { 0 }
                    })
            })
        }).map(|condition| condition.item_id).collect())
    }


    /// Absence is not proof of rollback until the caller reacquires its domain row locks.
    pub(crate) async fn is_committed(&self, ex: &(impl Executor + ?Sized)) -> std::result::Result<bool, crate::db::Error> {
        let mut complete = true;
        for captured in self.events() {
            let key = SourceKey::canonical(captured.event.kind(), captured.identity, captured.ordinal);
            let Some(stored) = crate::dao::progress::source(ex, "business", key.as_str()).await? else {
                complete = false;
                continue;
            };
            let (occurred_at, context) = crate::lua::events::combat::metadata(&captured.event)
                .ok_or_else(|| crate::db::Error::Progress(anyhow::anyhow!("combat fact missing metadata")))?;
            let expected_context = serde_json::to_value(context).map_err(|e| crate::db::Error::Progress(e.into()))?;
            let actual_context: serde_json::Value = serde_json::from_str(&stored.context).map_err(|e| crate::db::Error::Progress(e.into()))?;
            if stored.event != captured.event || stored.occurred_at != occurred_at || actual_context != expected_context
                || stored.origin != context.origin.as_str() || stored.ordinal != captured.ordinal
                || stored.subject_character_id != crate::lua::events::combat::subject(&captured.event) {
                return Err(crate::db::Error::Progress(anyhow::anyhow!("combat source collision: {}", key.as_str())));
            }
        }
        Ok(complete)
    }

    pub(crate) fn acknowledge(&self, s: &mut System, written: &HashSet<i64>) {
        for condition in &self.conditions {
            if written.contains(&condition.item_id) {
                s.acknowledge_condition(condition.item_id, condition.generation, condition.revision);
            }
        }
        if let Some(facts) = s.combat_facts.as_mut() {
            for batch in &self.batches { facts.ack(batch); }
        }
    }
}

#[cfg(test)] mod tests;
#[cfg(test)] pub(crate) use tests::facts_test_init;
