//! Atomic startup cutover for legacy market orders, using game market tables and the
//! existing SDE station loader. A committed marker records completion; this does not
//! settle principal or reconstruct historical trades.
use anyhow::{bail, Result};
use infinity_core::consts::{CORP_HANGAR_DIVISIONS, FILETIME_DAY, FLAG_CORP_SAG1, TYPE_PLEX};
use tokio_postgres::Transaction;

/// Apply the market cutover inside the caller's schema transaction, before market seeding.
pub(crate) async fn migrate(tx: &Transaction<'_>) -> Result<()> {
    if cutover_complete(tx).await? {
        return Ok(());
    }
    let legacy = tx.query(
        "SELECT order_id, station_id, solar_system_id, constellation_id, region_id
         FROM game.market_orders WHERE type_id = $1 AND char_id IS NOT NULL AND market_scope = 0 ORDER BY order_id",
        &[&TYPE_PLEX],
    ).await?;
    if !legacy.is_empty() {
        let stations = crate::dao::sde::map::stations(tx).await?;
        for row in &legacy {
            let station: i64 = row.get("station_id");
            let system: i64 = row.get("solar_system_id");
            let constellation: i64 = row.get("constellation_id");
            let region: i64 = row.get("region_id");
            if !stations.binary_search_by_key(&station, |s| s.station_id).ok().is_some_and(|i| {
                let s = &stations[i];
                s.solar_system_id == Some(system) && s.constellation_id == Some(constellation) && s.region_id == Some(region)
            }) {
                bail!("legacy PLEX order {} has an invalid NPC station origin", row.get::<_, i64>("order_id"));
            }
        }
    }
    let invalid = tx.query_opt(
        "SELECT o.order_id FROM game.market_orders o
         LEFT JOIN game.characters c ON c.character_id = o.char_id
         LEFT JOIN game.accounts a ON a.user_id = c.user_id
         LEFT JOIN game.corporations corp ON corp.corporation_id = o.corporation_id
         WHERE o.type_id = $1 AND o.char_id IS NOT NULL AND o.market_scope = 0 AND (
           c.character_id IS NULL OR a.user_id IS NULL
           OR (o.corporation_id IS NOT NULL AND corp.corporation_id IS NULL)
           OR o.price <= 0 OR o.vol_entered <= 0 OR o.vol_remaining <= 0
           OR o.vol_remaining > o.vol_entered OR o.min_volume <= 0 OR o.min_volume > o.vol_entered
           OR o.issued IS NULL OR o.duration <= 0
           OR (o.corporation_id IS NOT NULL AND (corp.corporation_id IS NULL
               OR (NOT o.bid AND (o.source_location_id <= 0 OR o.source_flag_id < $3
                   OR o.source_flag_id >= $3 + $4))))
           OR o.issued::numeric + o.duration::numeric * $2::bigint > 9223372036854775807::numeric
           OR o.issued::numeric + o.duration::numeric * $2::bigint < -9223372036854775808::numeric
           OR (o.bid AND o.escrow <> o.price * o.vol_remaining)
           OR (NOT o.bid AND o.escrow <> 0)) LIMIT 1",
        &[&TYPE_PLEX, &FILETIME_DAY, &FLAG_CORP_SAG1, &CORP_HANGAR_DIVISIONS],
    ).await?;
    if let Some(row) = invalid {
        bail!("legacy PLEX order {} has inconsistent ownership, principal or expiry", row.get::<_, i64>("order_id"));
    }
    tx.batch_execute(
        "CREATE TEMP TABLE market_cutover_books (market_scope smallint, type_id integer, region_key bigint,
             PRIMARY KEY (market_scope, type_id, region_key)) ON COMMIT DROP"
    ).await?;
    let now = crate::client::filetime_now();
    let today = now / FILETIME_DAY * FILETIME_DAY;
    tx.execute(
        "WITH anchored AS (
             UPDATE game.market_orders SET issued = $2
             WHERE type_id <> $1 AND char_id IS NULL AND issued IS NULL
             RETURNING market_scope, type_id, region_id
         )
         INSERT INTO market_cutover_books
         SELECT DISTINCT market_scope, type_id, CASE WHEN market_scope = 1 THEN 0 ELSE region_id END FROM anchored
         UNION
         SELECT DISTINCT 0::smallint, type_id, region_id FROM game.market_orders WHERE type_id = $1 AND market_scope = 0
         ON CONFLICT DO NOTHING", &[&TYPE_PLEX, &today],
    ).await?;
    if !legacy.is_empty() {
        tx.execute("INSERT INTO market_cutover_books VALUES (1, $1, 0) ON CONFLICT DO NOTHING", &[&TYPE_PLEX]).await?;
    }
    if tx.query_opt("SELECT o.order_id FROM game.market_orders o JOIN game.market_legacy_npc_backup b USING (order_id) WHERE o.type_id = $1 AND o.char_id IS NULL LIMIT 1", &[&TYPE_PLEX]).await?.is_some() {
        bail!("legacy NPC PLEX backup order ID already exists");
    }
    tx.execute(
        "INSERT INTO game.market_legacy_npc_backup (order_id, row_data)
         SELECT order_id, to_jsonb(o) FROM game.market_orders o WHERE type_id = $1 AND char_id IS NULL", &[&TYPE_PLEX],
    ).await?;
    tx.execute("DELETE FROM game.market_orders WHERE type_id = $1 AND char_id IS NULL", &[&TYPE_PLEX]).await?;
    tx.execute(
        "UPDATE game.market_orders o SET market_scope = 1,
             vault_user_id = CASE WHEN o.corporation_id IS NULL THEN c.user_id ELSE NULL END,
             plex_escrow = CASE WHEN o.bid THEN 0 ELSE o.vol_remaining END,
             created_at = o.issued, expires_at = (o.issued::numeric + o.duration::numeric * $2::bigint)::bigint
         FROM game.characters c WHERE o.type_id = $1 AND o.char_id = c.character_id AND o.market_scope = 0",
        &[&TYPE_PLEX, &FILETIME_DAY],
    ).await?;
    tx.execute(
        "UPDATE game.market_book_versions v SET revision = v.revision + 1, changed_at = $1
         FROM market_cutover_books b WHERE (v.market_scope, v.type_id, v.region_key) = (b.market_scope, b.type_id, b.region_key)", &[&now],
    ).await?;
    tx.execute(
        "INSERT INTO game.market_book_versions (market_scope, type_id, region_key, revision, changed_at)
         SELECT market_scope, type_id, region_key, 1, $1::bigint FROM market_cutover_books ON CONFLICT DO NOTHING", &[&now],
    ).await?;
    tx.execute(
        "INSERT INTO game.market_book_versions (market_scope, type_id, region_key, revision, changed_at)
         SELECT DISTINCT market_scope, type_id, CASE WHEN market_scope = 1 THEN 0 ELSE region_id END, 1, $1::bigint
         FROM game.market_orders ON CONFLICT DO NOTHING", &[&now],
    ).await?;
    tx.execute("INSERT INTO game.market_cutover_complete (singleton) VALUES (true)", &[]).await?;
    Ok(())
}

async fn cutover_complete(tx: &Transaction<'_>) -> Result<bool> {
    let exists: bool = tx.query_one(
        "SELECT to_regclass('game.market_cutover_complete') IS NOT NULL AS exists", &[],
    ).await?.get("exists");
    if !exists {
        return Ok(false);
    }
    Ok(tx.query_opt(
        "SELECT singleton FROM game.market_cutover_complete WHERE singleton = true", &[],
    ).await?.is_some())
}

/// Refuse a startup with schema application disabled unless the cutover is already complete.
pub(crate) async fn validate(tx: &Transaction<'_>) -> Result<()> {
    if !cutover_complete(tx).await? {
        bail!("market cutover is incomplete; enable schema_on_start to apply game_schema.sql and complete the cutover");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
