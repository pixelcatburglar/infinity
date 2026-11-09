//! Isolated database rows for atomicity tests.
use crate::db::{Db, Error};

pub(in crate::services) async fn isolated_market_stations<'a>(db: &Db, sde: &'a crate::sde::Sde) -> Result<impl Iterator<Item = crate::dao::market::orders::Station> + 'a, Error> {
    let occupied: std::collections::HashSet<i64> = db.query("SELECT region_id FROM game.market_orders UNION SELECT region_id FROM game.market_transactions", &[]).await?.into_iter().map(|row| row.get("region_id")).collect();
    Ok(sde.map.stations().filter_map(move |row| {
        let owner = row.owner_id?;
        row.solar_system_id?;
        row.constellation_id?;
        let region = row.region_id?;
        sde.npc_corporations.get(owner)?.faction_id?;
        if occupied.contains(&region) { return None; }
        crate::services::market::station(sde, row.station_id).ok()
    }))
}

pub(in crate::services) fn market_type_ids(sde: &crate::sde::Sde) -> impl Iterator<Item = i32> + '_ {
    sde.types.iter().filter(|ty| ty.type_id > 0 && ty.published == Some(true) && ty.market_group_id.is_some() && sde.is_tradable(ty.type_id)).map(|ty| ty.type_id)
}

pub(crate) fn isk_cents(amount: crate::isk::IskAmount) -> i128 {
    amount.decimal().replace('.', "").parse().unwrap()
}

pub(in crate::services) async fn market_fingerprint(db: &Db, owners: &[i64], characters: &[i64]) -> Result<String, Error> {
    let mut ids = owners.to_vec();
    ids.extend_from_slice(characters);
    let queries = [
        "SELECT COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY t.character_id),'') AS value FROM game.characters t WHERE t.character_id=ANY($1)",
        "SELECT COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY t.item_id),'') AS value FROM game.items t WHERE t.owner_id=ANY($1)",
        "SELECT COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY t.order_id),'') AS value FROM game.market_orders t WHERE t.char_id=ANY($1) OR t.corporation_id=ANY($1)",
        "SELECT COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY t.order_id),'') AS value FROM game.market_order_history t WHERE t.char_id=ANY($1) OR t.corporation_id=ANY($1)",
        "SELECT COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY t.transaction_id),'') AS value FROM game.market_transactions t WHERE t.buyer_id=ANY($1) OR t.seller_id=ANY($1)",
        "SELECT COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY t.account_id),'') AS value FROM game.wallet_accounts t WHERE t.owner_id=ANY($1)",
        "SELECT COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY row_to_json(t)::text),'') AS value FROM game.wallet_journal t WHERE t.account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=ANY($1))",
        "SELECT COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY t.account_key),'') AS value FROM game.corp_wallets t WHERE t.corporation_id=ANY($1)",
        "SELECT COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY t.event_id),'') AS value FROM game.progress_events t WHERE t.subject_character_id=ANY($1) OR (t.kind='MarketOrderFilled' AND ((t.payload->'MarketOrderFilled'->>'buyer_id')::bigint=ANY($1) OR (t.payload->'MarketOrderFilled'->>'seller_id')::bigint=ANY($1))) OR (t.kind='WalletChanged' AND (t.payload->'WalletChanged'->>'owner_id')::bigint=ANY($1))",
        "SELECT COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY row_to_json(t)::text),'') AS value FROM game.progress_deliveries t WHERE t.event_id IN (SELECT event_id FROM game.progress_events WHERE subject_character_id=ANY($1) OR (kind='MarketOrderFilled' AND ((payload->'MarketOrderFilled'->>'buyer_id')::bigint=ANY($1) OR (payload->'MarketOrderFilled'->>'seller_id')::bigint=ANY($1))) OR (kind='WalletChanged' AND (payload->'WalletChanged'->>'owner_id')::bigint=ANY($1)))",
    ];
    let mut values = Vec::new();
    for query in queries { values.push(db.query_one(query, &[&ids]).await?.get::<_, String>("value")); }
    Ok(values.join("\n"))
}

#[derive(Clone, Copy)]
pub(crate) struct Fixture { pub account: i64, pub character: i64 }

pub(crate) async fn cleanup_progress(tx: &tokio_postgres::Transaction<'_>, owner_ids: &[i64], subject_ids: &[i64]) -> Result<(), Error> {
    tx.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE subject_character_id = ANY($2) OR (kind = 'WalletChanged' AND (payload->'WalletChanged'->>'owner_id')::bigint = ANY($1)) OR (kind IN ('DamageApplied', 'RemoteRepairApplied', 'WarpScrambleApplied') AND (payload->kind->'target'->>'character_id')::bigint = ANY($2)))", &[&owner_ids, &subject_ids]).await?;
    tx.execute("DELETE FROM game.progress_events WHERE subject_character_id = ANY($2) OR (kind = 'WalletChanged' AND (payload->'WalletChanged'->>'owner_id')::bigint = ANY($1)) OR (kind IN ('DamageApplied', 'RemoteRepairApplied', 'WarpScrambleApplied') AND (payload->kind->'target'->>'character_id')::bigint = ANY($2))", &[&owner_ids, &subject_ids]).await?;
    Ok(())
}

pub(crate) async fn cleanup_killmail_rewards(tx: &tokio_postgres::Transaction<'_>, kills: &[i64]) -> Result<(), Error> {
    use crate::services::bounty::payout::KIND;
    for kill in kills {
        tx.execute("DELETE FROM game.jobs WHERE job_id = $1", &[&format!("{KIND}:{kill}")]).await?;
    }
    tx.execute("DELETE FROM game.bounty_payouts WHERE killmail_id = ANY($1)", &[&kills]).await?;
    Ok(())
}

pub(crate) async fn create(db: &Db) -> Result<Fixture, Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let seq: i64 = tx.query_one("SELECT nextval('game.accounts_user_id_seq') AS id", &[]).await?.get("id");
        let registered = crate::dao::accounts::authentication::register(tx, &format!("atomicity-{seq}"), b"fixture", 0, 0).await?;
        let account = registered.user_id;
        let character = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'atomicity-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
        let fixture = Fixture { account, character };
        cleanup_progress(tx, &[fixture.account, fixture.character], &[fixture.character]).await?;
        Ok(fixture)
    }), |fixture, ()| async move { Ok(fixture) }).await
}

pub(in crate::services) async fn seed_skill(db: &Db, character: i64) -> Result<(), Error> {
    db.execute("INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, 1, 0, 0)", &[&character]).await?;
    db.execute("INSERT INTO game.skill_queue (character_id, position, type_id, to_level, start_sp, dest_sp, start_time, end_time) VALUES ($1, 0, 1, 1, 0, 100, 0, 1)", &[&character]).await?;
    Ok(())
}

pub(in crate::services) async fn skill_fingerprint(db: &Db, character: i64) -> Result<String, Error> {
    Ok(db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY type_id), '') FROM game.skills s WHERE character_id = $1) || '|' || (SELECT COALESCE(string_agg(row_to_json(q)::text, '' ORDER BY position), '') FROM game.skill_queue q WHERE character_id = $1) AS fingerprint", &[&character]).await?.get("fingerprint"))
}

pub(in crate::services) fn assert_division_by_zero(result: Result<(), crate::services::CallError>) {
    match result {
        Err(crate::services::CallError::Internal(error)) => match error.downcast_ref::<Error>() {
            Some(Error::Postgres(error)) => assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::DIVISION_BY_ZERO)),
            _ => panic!("expected a PostgreSQL division-by-zero error"),
        },
        _ => panic!("expected a PostgreSQL division-by-zero error"),
    }
}


pub(in crate::services) async fn seed_mission(db: &Db, sde: &crate::sde::Sde, character: i64, agent: i64, station: i64, system: i64, mission: i32, arc: i64) -> Result<(), Error> {
    let cargo_type = market_type_ids(sde).next().expect("dedicated SDE must contain a published tradable market type");
    db.execute("INSERT INTO game.agent_missions (character_id, agent_id, mission_id, dungeon_id, faction_id, state, system_id, x, y, z, reward_isk, reward_lp, offered_at, kind, arc_id, cargo_type, cargo_qty) VALUES ($1, $2, $3, 0, 0, 1, $4, 0, 0, 0, 0, 0, 0, 'courier', $5, $6, 1)", &[&character, &agent, &mission, &system, &arc, &cargo_type]).await?;
    db.execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5, false)", &[&cargo_type, &character, &station, &infinity_core::consts::FLAG_HANGAR, &i64::from(i32::MAX)]).await?;
    Ok(())
}

pub(in crate::services) async fn mission_fingerprint(db: &Db, character: i64) -> Result<String, Error> {
    Ok(db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(m)::text, '' ORDER BY agent_id), '') FROM game.agent_missions m WHERE character_id = $1) || '|' || (SELECT COALESCE(string_agg(row_to_json(a)::text, '' ORDER BY arc_id, mission_id), '') FROM game.epic_arc_missions a WHERE character_id = $1) || '|' || (SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') FROM game.items i WHERE owner_id = $1) AS fingerprint", &[&character]).await?.get("fingerprint"))
}
pub(in crate::services) async fn clear_cargo_stack(db: &Db, character: i64) -> Result<(), Error> {
    db.execute("DELETE FROM game.items WHERE owner_id = $1", &[&character]).await?;
    Ok(())
}

pub(in crate::services) async fn store_purchase_count(db: &Db, account: i64, offer: i32) -> Result<i64, Error> {
    Ok(db.query_one("SELECT count(*) AS count FROM game.store_purchases WHERE user_id = $1 AND offer_id = $2", &[&account, &offer]).await?.get("count"))
}


pub(in crate::services) async fn remaining(db: &Db, f: Fixture) -> Result<i64, Error> {
    Ok(db.query_one("SELECT (SELECT count(*) FROM game.accounts WHERE user_id = $1) + (SELECT count(*) FROM game.characters WHERE character_id = $2) + (SELECT count(*) FROM game.items WHERE owner_id = $2) + (SELECT count(*) FROM game.skills WHERE character_id = $2) + (SELECT count(*) FROM game.skill_queue WHERE character_id = $2) + (SELECT count(*) FROM game.agent_missions WHERE character_id = $2) + (SELECT count(*) FROM game.epic_arc_missions WHERE character_id = $2) + (SELECT count(*) FROM game.wallet_accounts WHERE owner_id = $2) + (SELECT count(*) FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id = $2)) AS remaining", &[&f.account, &f.character]).await?.get("remaining"))
}

pub(crate) async fn cleanup(db: &Db, fixture: Fixture) -> Result<(), Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        cleanup_progress(tx, &[fixture.account, fixture.character], &[fixture.character]).await?;
        tx.execute("DELETE FROM game.account_clone_grade_audit WHERE user_id = $1 OR actor_user_id = $1", &[&fixture.account]).await?;
        tx.execute("DELETE FROM game.jobs WHERE job_id = $1", &[&format!("subscription.clone_grade.expire:{}", fixture.account)]).await?;
        tx.execute("DELETE FROM game.agent_missions WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.epic_arc_missions WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.skill_queue WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.skills WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.items WHERE owner_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.store_purchases WHERE user_id = $1", &[&fixture.account]).await?;
        tx.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id = $1)", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.wallet_accounts WHERE owner_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&fixture.account]).await?;
        Ok(())
    }), |(), ()| async move { Ok(()) }).await
}

pub(in crate::services) async fn create_corporation(db: &Db, ceo: i64, station: i64) -> Result<i64, Error> {
    let name = format!("fixture-corporation-{ceo}");
    let ticker = format!("F{ceo}");
    let corporation = crate::dao::corporations::registry::create(db, &crate::dao::corporations::registry::NewCorporation {
        name: &name, ticker: &ticker, description: "", url: "", tax: 0.0, lp_tax: 0.0, ceo,
        station, recruiting: false, friendly: false, shapes: [None; 7], now: 0, member_limit: None,
    }).await?;
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        cleanup_progress(tx, &[corporation], &[corporation]).await
    }), |(), ()| async move { Ok(()) }).await?;
    Ok(corporation)
}

pub(in crate::services) async fn assign_corporation(db: &Db, character: i64, corporation: i64, roles: i64) -> Result<(), Error> {
    let local = infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1 | infinity_core::consts::ROLE_HANGAR_CAN_QUERY_1;
    db.execute("UPDATE game.characters SET corporation_id=$2,corp_roles=$3,corp_roles_hq=$4,corp_roles_base=0,corp_roles_other=0 WHERE character_id=$1", &[&character, &corporation, &(roles & !local), &(roles & local)]).await?;
    Ok(())
}

pub(in crate::services) async fn create_office(db: &Db, office: i64, corporation: i64, station: i64) -> Result<(), Error> {
    crate::dao::offices::insert(db, office, corporation, station, i64::MAX).await?;
    Ok(())
}

pub(in crate::services) async fn cleanup_corporation(db: &Db, corporation: i64, members: &[i64], office: i64) -> Result<(), Error> {
    db.transaction(|tx, _: &mut ()| {
        let members = members.to_vec();
        Box::pin(async move {
            cleanup_progress(tx, &[corporation], &[corporation]).await?;
            tx.execute("DELETE FROM game.items WHERE owner_id = $1", &[&corporation]).await?;
            tx.execute("DELETE FROM game.offices WHERE office_id = $1", &[&office]).await?;
            tx.execute("DELETE FROM game.employment_history WHERE char_id = ANY($1)", &[&members]).await?;
            tx.execute("UPDATE game.characters SET corporation_id = 1, corp_roles = 0 WHERE character_id = ANY($1)", &[&members]).await?;
            tx.execute("DELETE FROM game.corporations WHERE corporation_id = $1", &[&corporation]).await?;
            Ok(())
        })
    }, |(), ()| async move { Ok(()) }).await
}
