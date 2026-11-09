//! One-time promotion of durable progress events to exact wallet and market amounts.
//! No client API is mirrored; the source is `game.progress_events`.
//! This is startup-only and does not replay or modify delivery receipts.

use crate::{dao::progress::migration as dao, db::{Db, Error}, isk::IskAmount, progress::codec};
use anyhow::Context;

fn convert(version: i32, kind: &str, payload: &str) -> anyhow::Result<String> {
    anyhow::ensure!(version == 1 || version == 2, "unsupported legacy progress schema");
    let mut value: serde_json::Value = serde_json::from_str(payload)?;
    let fields_to_convert: &[&str] = match (version, kind) {
        (1, "WalletChanged") => &["balance", "delta"],
        (_, "MarketOrderPlaced" | "MarketOrderFilled") => &["price"],
        _ => &[],
    };
    if !fields_to_convert.is_empty() {
        let fields = value.get_mut(kind).and_then(serde_json::Value::as_object_mut)
            .context("invalid legacy event")?;
        for name in fields_to_convert {
            let number = fields.get(*name).and_then(serde_json::Value::as_f64)
                .with_context(|| format!("invalid legacy {kind} {name}"))?;
            let amount = IskAmount::from_isk(number).with_context(|| format!("invalid legacy {kind} {name}"))?;
            fields.insert((*name).to_owned(), serde_json::Value::String(amount.decimal()));
        }
    }
    let result = serde_json::to_string(&value)?;
    codec::decode(codec::SCHEMA_VERSION, kind, &result)?;
    Ok(result)
}

async fn migrate_tx(tx: &tokio_postgres::Transaction<'_>) -> Result<usize, Error> {
    let events = dao::legacy(tx).await?;
    for event in &events {
        let payload = convert(event.version, &event.kind, &event.payload)
            .with_context(|| format!("invalid legacy progress event {}", event.event_id))
            .map_err(Error::Progress)?;
        if !dao::promote(tx, event, &payload, codec::SCHEMA_VERSION).await? {
            return Err(Error::Progress(anyhow::anyhow!("legacy progress event {} changed during migration", event.event_id)));
        }
    }
    Ok(events.len())
}

/// Run after schema setup and before any progress consumers or listeners start.
pub(crate) async fn migrate(db: &Db) -> Result<usize, Error> {
    db.transaction::<_, _, Error, (), _, _, _>(
        |tx, _| Box::pin(async move { migrate_tx(tx).await }),
        |count, ()| async move { Ok(count) },
    ).await
}

#[cfg(test)] mod tests;
