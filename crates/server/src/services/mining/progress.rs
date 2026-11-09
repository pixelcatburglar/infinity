//! Bounded aggregation of committed mining ledger snapshots into durable progress events.
//! There is no client counterpart; game.character_mining_ledger is the source.
//! Each source row remains a separate time-filterable slice; current world state is not read.

use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};
use crate::{dao::mining_ledger::progress::{self, ProgressLog}, db::{Db, outbox::{Outbox, SourceKey}}, lua::events::{GameEvent, resources::{FactContext, FactOrigin, MiningResourceKind, MiningSlice}}, scheduler::Job, services::{CallError, db_err}, space::World};

struct Group { char_id: i64, type_id: i32, kind: MiningResourceKind, context: FactContext, slices: Vec<MiningSlice> }

type GroupKey = (i64, i32, bool, i64, Option<i64>, Option<i64>, Option<i64>, i64, i32);

fn group(rows: Vec<ProgressLog>) -> anyhow::Result<Vec<Group>> {
    let mut groups: BTreeMap<GroupKey, Group> = BTreeMap::new();
    let mut previous = None;
    for row in rows {
        anyhow::ensure!(row.log_id > 0 && previous.is_none_or(|id| id < row.log_id), "invalid mining source order or id");
        previous = Some(row.log_id);
        let volume = row.quantity as f64 * row.unit_volume_m3;
        anyhow::ensure!(row.event_time > 0 && row.quantity > 0 && row.unit_volume_m3.is_finite() && row.unit_volume_m3 > 0.0 && volume.is_finite() && volume > 0.0, "invalid mining source slice {}", row.log_id);
        let key = (row.character_id, row.type_id, row.resource_kind == MiningResourceKind::Gas, row.solar_system_id, row.corporation_id, row.alliance_id, row.faction_id, row.source_ship_id, row.source_ship_type_id);
        let entry = groups.entry(key).or_insert_with(|| Group {
            char_id: row.character_id, type_id: row.type_id, kind: row.resource_kind,
            context: FactContext { corporation_id: row.corporation_id, alliance_id: row.alliance_id, faction_id: row.faction_id, ship_id: Some(row.source_ship_id), ship_type_id: Some(row.source_ship_type_id), system_id: Some(row.solar_system_id), station_id: None, origin: FactOrigin::Natural },
            slices: Vec::new(),
        });
        entry.slices.push(MiningSlice { log_id: row.log_id, occurred_at: row.event_time, quantity: row.quantity, volume_m3: volume });
    }
    Ok(groups.into_values().collect())
}

pub(crate) async fn aggregate_tx(tx: &tokio_postgres::Transaction<'_>, outbox: &mut Outbox, limit: i64) -> Result<usize, CallError> {
    let rows = progress::lock_pending(tx, limit).await.map_err(db_err)?;
    let ids: Vec<i64> = rows.iter().map(|row| row.log_id).collect();
    let groups = group(rows)?;
    for group in groups {
        let id = group.slices[0].log_id;
        let (kind, event) = match group.kind {
            MiningResourceKind::Ore => ("OreMined", GameEvent::OreMined { char_id: group.char_id, ore_type: group.type_id, context: group.context, slices: group.slices }),
            MiningResourceKind::Gas => ("GasHarvested", GameEvent::GasHarvested { char_id: group.char_id, gas_type: group.type_id, context: group.context, slices: group.slices }),
        };
        outbox.event_from(SourceKey::new(kind, id), event);
    }
    if !ids.is_empty() {
        let marked = progress::mark_aggregated(tx, &ids).await.map_err(db_err)?;
        if marked != ids.len() as u64 { return Err(anyhow::anyhow!("mining aggregation marker count mismatch").into()); }
    }
    Ok(ids.len())
}

pub(crate) fn handle(db: Db, _world: Arc<World>, _job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let limit = crate::config::game().mining.progress_batch_rows;
        db.transaction(|tx, outbox: &mut Outbox| Box::pin(aggregate_tx(tx, outbox, limit)), |value, _outbox| async move { Ok(value) })
            .await.map_err(|error| anyhow::anyhow!("mining progress aggregation failed: {error:?}"))?;
        Ok(())
    })
}

#[cfg(test)] mod tests;
