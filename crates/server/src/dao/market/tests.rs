//! Dedicated market database test support.
use infinity_core::{consts::{FILETIME_DAY, FILETIME_SECOND}};
use crate::{client::filetime_now, db::{Db, Error}};
use infinity_core::{consts::{FLAG_HANGAR, TYPE_PLEX}};
use crate::{dao::market::orders::Station, sde::Sde, services::tests::fixture::{self, Fixture}};

mod plex_settlement;
mod lifecycle;
mod books;
mod stack_quantities;

pub(super) async fn isolated_stations(db: &Db, sde: &Sde) -> (Station, Station) {
    let occupied: i64 = db.query_one("SELECT count(*) AS n FROM game.market_orders WHERE type_id=$1", &[&TYPE_PLEX]).await.unwrap().get("n");
    assert_eq!(occupied, 0, "dedicated database must have an empty global PLEX book");
    let stations: Vec<_> = sde.map.stations().filter_map(|row| {
        let owner = row.owner_id?;
        let system = row.solar_system_id?;
        let constellation = row.constellation_id?;
        let region = row.region_id?;
        sde.npc_corporations.get(owner)?.faction_id?;
        if !sde.map.system(system).and_then(|s| s.security).is_some_and(|security| security >= crate::config::game().crimewatch.highsec_threshold) { return None; }
        Some(Station { id: row.station_id, system, constellation, region, owner })
    }).collect();
    let first = *stations.first().expect("NPC market station");
    let second = *stations.iter().find(|s| s.region != first.region).expect("NPC station in another region");
    (first, second)
}

pub(super) async fn fund(db: &Db, person: Fixture, amount: &str) {
    db.execute("UPDATE game.characters SET balance=$2::text::numeric WHERE character_id=$1", &[&person.character, &amount]).await.unwrap();
}

pub(super) async fn physical_plex(db: &Db, owner: i64, station: i64, quantity: i64) -> i64 {
    db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,false) RETURNING item_id", &[&TYPE_PLEX, &owner, &station, &FLAG_HANGAR, &quantity]).await.unwrap().get("item_id")
}

pub(super) async fn cleanup(db: &Db, people: &[Fixture]) {
    let characters: Vec<i64> = people.iter().map(|f| f.character).collect();
    let accounts: Vec<i64> = people.iter().map(|f| f.account).collect();
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE kind='MarketOrderFilled' AND ((payload->'MarketOrderFilled'->>'buyer_id')::bigint=ANY($1) OR (payload->'MarketOrderFilled'->>'seller_id')::bigint=ANY($1)))", &[&characters]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE kind='MarketOrderFilled' AND ((payload->'MarketOrderFilled'->>'buyer_id')::bigint=ANY($1) OR (payload->'MarketOrderFilled'->>'seller_id')::bigint=ANY($1))", &[&characters]).await.unwrap();
    db.execute("DELETE FROM game.plex_vault_transactions WHERE user_id=ANY($1)", &[&accounts]).await.unwrap();
    db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=ANY($1)) OR market_transaction_id IN (SELECT transaction_id FROM game.market_transactions WHERE buyer_id=ANY($1) OR seller_id=ANY($1))", &[&characters]).await.unwrap();
    db.execute("DELETE FROM game.market_transactions WHERE buyer_id=ANY($1) OR seller_id=ANY($1)", &[&characters]).await.unwrap();
    db.execute("DELETE FROM game.market_orders WHERE char_id=ANY($1)", &[&characters]).await.unwrap();
    db.execute("DELETE FROM game.market_order_history WHERE char_id=ANY($1)", &[&characters]).await.unwrap();
    for person in people { fixture::cleanup(db, *person).await.unwrap(); }
}

pub(crate) async fn age_order_for_modification(db: &Db, order_id: i64) -> Result<(), Error> {
    let issued = crate::config::game().market.modification_delay_seconds.checked_add(1)
        .and_then(|seconds| seconds.checked_mul(FILETIME_SECOND))
        .and_then(|ticks| filetime_now().checked_sub(ticks))
        .ok_or_else(|| Error::Progress(anyhow::anyhow!("test modification window outside FILETIME range")))?;
    db.execute("UPDATE game.market_orders SET issued=$2, expires_at=($2::bigint::numeric + duration::numeric * $3::bigint::numeric)::bigint WHERE order_id=$1", &[&order_id, &issued, &FILETIME_DAY]).await?;
    Ok(())
}
