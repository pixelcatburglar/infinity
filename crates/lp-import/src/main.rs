//! Operator-only import of ESI LP offers into `game.lp_offers`.
//! The client `lpStoreSvc.py` consumes the resulting catalogue; this tool has
//! no client mirror. Sources are ESI and `sde.crp_npc_corporations` / `sde.inv_types`.
//! It does not fetch during server operation or delete offers absent from ESI.
//! The server reads the catalogue into memory once at startup; restart it for
//! imported or adjusted offers to take effect.

mod offers;

use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::time::Duration;
use tokio_postgres::NoTls;

/// Operator HTTP timeout; a stalled ESI request must not hang an import indefinitely.
const HTTP_TIMEOUT_SECS: u64 = 60;
/// Public ESI version root, as documented by the EVE developer API explorer.
const DEFAULT_ESI_BASE_URL: &str = "https://esi.evetech.net/latest";
/// Server design default for the maximum size of one ESI corporation response.
const DEFAULT_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut db_url = None;
    let mut esi_base = None;
    let mut max_response_bytes = DEFAULT_MAX_RESPONSE_BYTES;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--database-url" => db_url = Some(args.next().context("--database-url needs a value")?),
            "--esi-base-url" => esi_base = Some(args.next().context("--esi-base-url needs a value")?),
            "--max-response-bytes" => {
                max_response_bytes = args.next().context("--max-response-bytes needs a value")?
                    .parse().context("--max-response-bytes must be a positive integer")?;
                if max_response_bytes == 0 { bail!("--max-response-bytes must be nonzero"); }
            }
            _ => bail!("unknown argument {arg}; usage: lp-import --database-url <postgres-url> [--esi-base-url <esi-root>] [--max-response-bytes <bytes>]"),
        }
    }
    let db_url = db_url.context("--database-url is required")?;
    let esi_base = esi_base.unwrap_or_else(|| DEFAULT_ESI_BASE_URL.to_owned());
    let (mut db, connection) = tokio_postgres::connect(&db_url, NoTls).await.context("connecting to database")?;
    tokio::spawn(async move {
        if let Err(error) = connection.await { eprintln!("database connection error: {error}"); }
    });
    let corporations = db.query("SELECT corporation_id::bigint AS corporation_id FROM sde.crp_npc_corporations WHERE NOT COALESCE(deleted, false) AND jsonb_typeof(data->'lpOfferTables') = 'array' AND jsonb_array_length(data->'lpOfferTables') > 0 ORDER BY corporation_id", &[]).await.context("loading eligible corporations")?;
    let types: HashSet<i32> = db.query("SELECT type_id FROM sde.inv_types", &[]).await.context("loading SDE types")?.iter().map(|row| row.get("type_id")).collect();
    let http = reqwest::Client::builder().user_agent("infinity-eve lp-import operator").timeout(Duration::from_secs(HTTP_TIMEOUT_SECS)).build()?;
    let mut imported = 0usize;
    let mut failed = 0usize;
    let mut count = 0usize;
    for row in &corporations {
        let corporation_id: i64 = row.get("corporation_id");
        let result = async {
            let offers = offers::fetch(&http, &esi_base, corporation_id, &types, max_response_bytes).await?;
            store(&mut db, corporation_id, &offers).await?;
            Ok::<usize, anyhow::Error>(offers.len())
        }.await;
        match result {
            Ok(n) => { imported += 1; count += n; eprintln!("corporation {corporation_id}: imported {n} offers"); }
            Err(error) => { failed += 1; eprintln!("corporation {corporation_id}: {error:#}"); }
        }
    }
    eprintln!("corporations={} succeeded={} failed={} offers={} errors={}", corporations.len(), imported, failed, count, failed);
    eprintln!("Restart the server for imported LP offers to take effect.");
    if failed != 0 { bail!("one or more corporation imports failed"); }
    Ok(())
}

async fn store(db: &mut tokio_postgres::Client, corporation_id: i64, offers: &[offers::Offer]) -> Result<()> {
    let tx = db.transaction().await?;
    for offer in offers {
        let items = offer.required_items.to_string();
        tx.execute("INSERT INTO game.lp_offers (corporation_id, offer_id, type_id, quantity, lp_cost, isk_cost, ak_cost, required_items) VALUES ($1, $2, $3, $4, $5, $6, $7, $8::text::jsonb) ON CONFLICT (corporation_id, offer_id) DO UPDATE SET type_id = EXCLUDED.type_id, quantity = EXCLUDED.quantity, lp_cost = EXCLUDED.lp_cost, isk_cost = EXCLUDED.isk_cost, ak_cost = EXCLUDED.ak_cost, required_items = EXCLUDED.required_items", &[&corporation_id, &offer.offer_id, &offer.type_id, &offer.quantity, &offer.lp_cost, &offer.isk_cost, &offer.ak_cost, &items]).await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests;
