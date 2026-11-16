//! Isolated database rows for atomicity tests.
use crate::db::Db;
use crate::db::Error;
use std::{future::Future, pin::Pin};

#[cfg(any(feature = "tests-economy", feature = "tests-social", feature = "tests-services"))]
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

#[cfg(any(feature = "tests-economy", feature = "tests-social", feature = "tests-content", feature = "tests-services"))]
pub(in crate::services) fn market_type_ids(sde: &crate::sde::Sde) -> impl Iterator<Item = i32> + '_ {
    sde.types.iter().filter(|ty| ty.type_id > 0 && ty.published == Some(true) && ty.market_group_id.is_some() && sde.is_tradable(ty.type_id)).map(|ty| ty.type_id)
}

#[cfg(any(feature = "tests-skills", feature = "tests-social", feature = "tests-content", feature = "tests-services", feature = "tests-storage"))]
pub(crate) fn isk_cents(amount: crate::isk::IskAmount) -> i128 {
    amount.decimal().replace('.', "").parse().unwrap()
}

#[cfg(any(feature = "tests-inventory", feature = "tests-economy", feature = "tests-social", feature = "tests-services"))]
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

/// Real SDE ancestry written to every shared fixture character.
#[derive(Clone, Copy)]
pub(in crate::services) struct FixtureLocation { system: i64, constellation: i64, region: i64 }

/// First SDE system with a complete, resolvable constellation/region ancestry.
pub(in crate::services) fn default_location(sde: &crate::sde::Sde) -> FixtureLocation {
    sde.map.systems().find_map(|system| {
        let constellation = system.constellation_id?;
        let region = system.region_id?;
        sde.map.constellation(constellation)?;
        sde.map.region(region)?;
        Some(FixtureLocation { system: system.solar_system_id, constellation, region })
    }).expect("dedicated SDE must contain a system with a complete ancestry")
}

/// Create a real recovery-custody row for consumer authorization tests.
#[cfg(feature = "tests-economy")]
pub(in crate::services) async fn recovery_custody(db: &Db, owner: i64, station: i64, type_id: i32) -> (i64, i64) {
    use crate::dao::{items::{parent_locks::{self, LockRequest, LockStrength}, repairable_ship as physical}, repairable_ship::{self as recovery, ItemId, LossSource, NewGroup, NewItem, SourceKind}};
    let now = crate::client::filetime_now();
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let raw: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,4,1,true) RETURNING item_id", &[&type_id, &owner, &station]).await?.get("item_id");
        let item = ItemId::new(raw).unwrap();
        let shape = physical::capture_many(tx, &[item]).await?.remove(0);
        let group = recovery::insert_group(tx, &NewGroup { loss: LossSource { kind: SourceKind::UnpilotedShip,
            beneficiary: owner, shape: shape.clone(), system: 1, lethal_at: now, sampled_position: [0.0; 3] },
            loss_id: None, destination_station: station, payment_expires_at: now + infinity_core::consts::FILETIME_DAY }).await?;
        let locks = parent_locks::lock(tx, &[], &[LockRequest { item_id: raw, strength: LockStrength::Update }]).await?;
        assert!(physical::move_to_custody(tx, &locks, group, &[shape]).await?);
        recovery::insert_items(tx, &[NewItem { group, item, original_owner: owner, type_id, flag: 4,
            quantity: 1, singleton: true, last_service_life: 1.0, repair_completed_at: now }]).await?;
        Ok::<_, Error>((raw, group.get()))
    }), |value, ()| async move { Ok(value) }).await.unwrap()
}

pub(crate) async fn cleanup_progress(tx: &crate::db::Transaction<'_>, owner_ids: &[i64], subject_ids: &[i64]) -> Result<(), Error> {
    tx.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE subject_character_id = ANY($2) OR (kind = 'WalletChanged' AND (payload->'WalletChanged'->>'owner_id')::bigint = ANY($1)) OR (kind IN ('DamageApplied', 'RemoteRepairApplied', 'WarpScrambleApplied') AND (payload->kind->'target'->>'character_id')::bigint = ANY($2)))", &[&owner_ids, &subject_ids]).await?;
    tx.execute("DELETE FROM game.progress_events WHERE subject_character_id = ANY($2) OR (kind = 'WalletChanged' AND (payload->'WalletChanged'->>'owner_id')::bigint = ANY($1)) OR (kind IN ('DamageApplied', 'RemoteRepairApplied', 'WarpScrambleApplied') AND (payload->kind->'target'->>'character_id')::bigint = ANY($2))", &[&owner_ids, &subject_ids]).await?;
    Ok(())
}

#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-social", feature = "tests-lua"))]
pub(crate) async fn cleanup_killmail_rewards(tx: &crate::db::Transaction<'_>, kills: &[i64]) -> Result<(), Error> {
    use crate::services::bounty::payout::KIND;
    for kill in kills {
        tx.execute("DELETE FROM game.jobs WHERE job_id = $1", &[&format!("{KIND}:{kill}")]).await?;
    }
    tx.execute("DELETE FROM game.bounty_payouts WHERE killmail_id = ANY($1)", &[&kills]).await?;
    Ok(())
}

pub(crate) fn create<'a>(db: &'a Db) -> Pin<Box<dyn Future<Output = Result<Fixture, Error>> + Send + 'a>> { Box::pin(async move {
    let sde = crate::tests::shared_sde(db).await;
    let location = default_location(&sde);
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let seq: i64 = tx.query_one("SELECT nextval('game.accounts_user_id_seq') AS id", &[]).await?.get("id");
        let registered = crate::dao::accounts::authentication::register(tx, &format!("atomicity-{seq}"), b"fixture", 0, 0).await?;
        let account = registered.user_id;
        let character = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'atomicity-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
        assert_eq!(crate::dao::characters::ship::location(tx, character, location.system, location.constellation, location.region).await?, 1);
        let fixture = Fixture { account, character };
        cleanup_progress(tx, &[fixture.account, fixture.character], &[fixture.character]).await?;
        Ok(fixture)
    }), |fixture, ()| async move { Ok(fixture) }).await
}) }

#[cfg(feature = "tests-economy")]
pub(in crate::services) async fn create_with_character_id(tx: &crate::db::Transaction<'_>, character: i64, location: FixtureLocation) -> Result<Fixture, Error> {
    let name = format!("owner-range-{character}");
    let account = crate::dao::accounts::authentication::register(tx, &name, b"fixture", 0, 0).await?.user_id;
    tx.execute("INSERT INTO game.characters (character_id, user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, $2, $3, 1, 1, 1, 1)", &[&character, &account, &name]).await?;
    assert_eq!(crate::dao::characters::ship::location(tx, character, location.system, location.constellation, location.region).await?, 1);
    Ok(Fixture { account, character })
}

#[cfg(any(feature = "tests-skills", feature = "tests-content", feature = "tests-slash", feature = "tests-services"))]
pub(in crate::services) async fn seed_skill(db: &Db, character: i64) -> Result<(), Error> {
    db.execute("INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, 1, 0, 0)", &[&character]).await?;
    db.execute("INSERT INTO game.skill_queue (character_id, position, type_id, to_level, start_sp, dest_sp, start_time, end_time) VALUES ($1, 0, 1, 1, 0, 100, 0, 1)", &[&character]).await?;
    Ok(())
}

#[cfg(any(feature = "tests-skills", feature = "tests-economy", feature = "tests-slash", feature = "tests-services"))]
pub(in crate::services) async fn skill_fingerprint(db: &Db, character: i64) -> Result<String, Error> {
    Ok(db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY type_id), '') FROM game.skills s WHERE character_id = $1) || '|' || (SELECT COALESCE(string_agg(row_to_json(q)::text, '' ORDER BY position), '') FROM game.skill_queue q WHERE character_id = $1) AS fingerprint", &[&character]).await?.get("fingerprint"))
}

#[cfg(any(feature = "tests-skills", feature = "tests-economy"))]
pub(in crate::services) fn assert_division_by_zero(result: Result<(), crate::services::CallError>) {
    match result {
        Err(crate::services::CallError::Internal(error)) => match error.downcast_ref::<Error>() {
            Some(Error::Postgres(error)) => assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::DIVISION_BY_ZERO)),
            _ => panic!("expected a PostgreSQL division-by-zero error"),
        },
        _ => panic!("expected a PostgreSQL division-by-zero error"),
    }
}


#[cfg(feature = "tests-content")]
pub(in crate::services) async fn seed_mission(db: &Db, sde: &crate::sde::Sde, character: i64, agent: i64, station: i64, system: i64, mission: i32, arc: i64) -> Result<(), Error> {
    let cargo_type = market_type_ids(sde).next().expect("dedicated SDE must contain a published tradable market type");
    db.execute("INSERT INTO game.agent_missions (character_id, agent_id, mission_id, dungeon_id, faction_id, state, system_id, x, y, z, reward_isk, reward_lp, offered_at, kind, arc_id, cargo_type, cargo_qty) VALUES ($1, $2, $3, 0, 0, 1, $4, 0, 0, 0, 0, 0, 0, 'courier', $5, $6, 1)", &[&character, &agent, &mission, &system, &arc, &cargo_type]).await?;
    db.execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5, false)", &[&cargo_type, &character, &station, &infinity_core::consts::FLAG_HANGAR, &i64::from(i32::MAX)]).await?;
    Ok(())
}

#[cfg(any(feature = "tests-economy", feature = "tests-content"))]
pub(in crate::services) async fn mission_fingerprint(db: &Db, character: i64) -> Result<String, Error> {
    Ok(db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(m)::text, '' ORDER BY agent_id), '') FROM game.agent_missions m WHERE character_id = $1) || '|' || (SELECT COALESCE(string_agg(row_to_json(a)::text, '' ORDER BY arc_id, mission_id), '') FROM game.epic_arc_missions a WHERE character_id = $1) || '|' || (SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') FROM game.items i WHERE owner_id = $1) AS fingerprint", &[&character]).await?.get("fingerprint"))
}
#[cfg(feature = "tests-content")]
pub(in crate::services) async fn clear_cargo_stack(db: &Db, character: i64) -> Result<(), Error> {
    db.execute("DELETE FROM game.items WHERE owner_id = $1", &[&character]).await?;
    Ok(())
}

#[cfg(feature = "tests-economy")]
pub(in crate::services) async fn store_purchase_count(db: &Db, account: i64, offer: i32) -> Result<i64, Error> {
    Ok(db.query_one("SELECT count(*) AS count FROM game.store_purchases WHERE user_id = $1 AND offer_id = $2", &[&account, &offer]).await?.get("count"))
}


#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-inventory", feature = "tests-skills", feature = "tests-economy", feature = "tests-social", feature = "tests-content", feature = "tests-slash", feature = "tests-services", feature = "tests-space", feature = "tests-lua", feature = "tests-runtime"))]
pub(in crate::services) async fn remaining(db: &Db, f: Fixture) -> Result<i64, Error> {
    Ok(db.query_one("SELECT (SELECT count(*) FROM game.accounts WHERE user_id = $1) + (SELECT count(*) FROM game.characters WHERE character_id = $2) + (SELECT count(*) FROM game.items WHERE owner_id = $2) + (SELECT count(*) FROM game.skills WHERE character_id = $2) + (SELECT count(*) FROM game.skill_queue WHERE character_id = $2) + (SELECT count(*) FROM game.agent_missions WHERE character_id = $2) + (SELECT count(*) FROM game.epic_arc_missions WHERE character_id = $2) + (SELECT count(*) FROM game.wallet_accounts WHERE owner_id = $2) + (SELECT count(*) FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id = $2)) + (SELECT count(*) FROM game.jobs WHERE job_id = $3) AS remaining", &[&f.account, &f.character, &format!("{}:{}", crate::services::skills::completion::KIND, f.character)]).await?.get("remaining"))
}

/// Captured owned keys for fixture teardown: exact account/character pairs,
/// current owner item IDs merged with caller extras, and wallet account IDs.
#[cfg(any(feature = "tests-combat", feature = "tests-economy", feature = "tests-effects", feature = "tests-lua", feature = "tests-runtime", feature = "tests-services", feature = "tests-ships", feature = "tests-slash", feature = "tests-social", feature = "tests-space"))]
pub(crate) struct FixtureRows {
    fixtures: Vec<Fixture>,
    item_ids: Vec<i64>,
    wallet_account_ids: Vec<i64>,
}

/// Capture each fixture's current owner item IDs and wallet account keys
/// before the first delete; caller-supplied IDs are merged and deduplicated.
#[cfg(any(feature = "tests-combat", feature = "tests-economy", feature = "tests-effects", feature = "tests-lua", feature = "tests-runtime", feature = "tests-services", feature = "tests-ships", feature = "tests-slash", feature = "tests-social", feature = "tests-space"))]
pub(crate) async fn capture_fixture_rows(db: &Db, fixtures: &[Fixture], extra_item_ids: &[i64]) -> Result<FixtureRows, Error> {
    let characters: Vec<i64> = fixtures.iter().map(|fixture| fixture.character).collect();
    let mut item_ids: Vec<i64> = db.query("SELECT item_id FROM game.items WHERE owner_id = ANY($1)", &[&characters]).await?.iter().map(|row| row.get("item_id")).collect();
    item_ids.extend_from_slice(extra_item_ids);
    item_ids.sort_unstable();
    item_ids.dedup();
    let wallet_account_ids = db.query("SELECT account_id FROM game.wallet_accounts WHERE owner_id = ANY($1)", &[&characters]).await?.iter().map(|row| row.get("account_id")).collect();
    Ok(FixtureRows { fixtures: fixtures.to_vec(), item_ids, wallet_account_ids })
}

/// Assert every captured fixture row is gone: zero remaining rows, empty
/// five-category item state, and no wallet rows under the retained keys.
#[cfg(any(feature = "tests-combat", feature = "tests-economy", feature = "tests-effects", feature = "tests-lua", feature = "tests-runtime", feature = "tests-services", feature = "tests-ships", feature = "tests-slash", feature = "tests-social", feature = "tests-space"))]
pub(crate) async fn assert_fixture_removed(db: &Db, rows: &FixtureRows) -> Result<(), Error> {
    for fixture in rows.fixtures.iter().copied() {
        assert_eq!(remaining(db, fixture).await?, 0);
    }
    assert!(crate::dao::tests::item_state(db, &rows.item_ids).await?.is_empty());
    assert!(db.query("SELECT 1 FROM game.wallet_accounts WHERE account_id = ANY($1)", &[&rows.wallet_account_ids]).await?.is_empty());
    assert!(db.query("SELECT 1 FROM game.wallet_journal WHERE account_id = ANY($1)", &[&rows.wallet_account_ids]).await?.is_empty());
    Ok(())
}

#[cfg(all(test, feature = "tests-services"))]
mod tests;

/// Remove only this fixture's recovery jobs, retained items and source records.
pub(crate) async fn cleanup_recovery(
    ex: &(impl crate::db::executor::Executor + ?Sized), owner: i64,
) -> Result<(), Error> {
    ex.execute("DELETE FROM game.jobs WHERE job_id IN (SELECT kind || ':' || m.member_id::text FROM game.repairable_recovery_items m JOIN game.repairable_recovery_groups g ON g.group_id=m.group_id CROSS JOIN unnest(ARRAY['repairable.payment.expiry','repairable.delivery.arrival','repairable.withdrawal.expiry']) kind WHERE g.beneficiary_id=$1)", &[&owner]).await?;
    ex.execute("DELETE FROM game.items WHERE item_id IN (SELECT m.item_id FROM game.repairable_recovery_items m JOIN game.repairable_recovery_groups g ON g.group_id=m.group_id WHERE g.beneficiary_id=$1)", &[&owner]).await?;
    ex.execute("DELETE FROM game.repairable_recovery_groups WHERE beneficiary_id=$1", &[&owner]).await?;
    Ok(())
}

#[cfg(any(feature = "tests-combat", feature = "tests-ships"))]
pub(in crate::services) async fn capture_recovery_jobs(tx: &crate::db::Transaction<'_>, character: i64) -> (Vec<i64>, Vec<String>) {
    let members: Vec<i64> = tx.query("SELECT m.member_id FROM game.repairable_recovery_items m JOIN game.repairable_recovery_groups g ON g.group_id=m.group_id WHERE g.beneficiary_id=$1 ORDER BY m.member_id", &[&character]).await.unwrap().iter().map(|r| r.get("member_id")).collect();
    assert!(!members.is_empty());
    let jobs: Vec<String> = members.iter().map(|id| format!("repairable.payment.expiry:{id}")).collect();
    assert_eq!(tx.query("SELECT job_id FROM game.jobs WHERE job_id=ANY($1)", &[&jobs]).await.unwrap().len(), jobs.len());
    (members, jobs)
}
#[cfg(feature = "tests-ships")]
pub(in crate::services) async fn assert_recovery_jobs_rolled_back(db: &Db, captured: &(Vec<i64>, Vec<String>)) {
    assert!(!captured.0.is_empty() && captured.0.len() == captured.1.len());
    assert!(db.query("SELECT 1 FROM game.repairable_recovery_items WHERE member_id=ANY($1)", &[&captured.0]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.jobs WHERE job_id=ANY($1)", &[&captured.1]).await.unwrap().is_empty());
}

pub(crate) fn cleanup<'a>(db: &'a Db, fixture: Fixture) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'a>> { Box::pin(async move {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        cleanup_recovery(tx, fixture.character).await?;
        cleanup_progress(tx, &[fixture.account, fixture.character], &[fixture.character]).await?;
        tx.execute("DELETE FROM game.account_clone_grade_audit WHERE user_id = $1 OR actor_user_id = $1", &[&fixture.account]).await?;
        tx.execute("DELETE FROM game.jobs WHERE job_id = $1", &[&format!("subscription.clone_grade.expire:{}", fixture.account)]).await?;
        tx.execute("DELETE FROM game.agent_missions WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.epic_arc_missions WHERE character_id = $1", &[&fixture.character]).await?;
        crate::dao::jobs::cancel(tx, &format!("{}:{}", crate::services::skills::completion::KIND, fixture.character)).await?;
        tx.execute("DELETE FROM game.skill_queue WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.skills WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.bill_autopay WHERE owner_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.bills WHERE debtor_id = $1 OR creditor_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.items WHERE owner_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.store_purchases WHERE user_id = $1", &[&fixture.account]).await?;
        tx.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id = $1)", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.wallet_accounts WHERE owner_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&fixture.account]).await?;
        Ok(())
    }), |(), ()| async move { Ok(()) }).await
}) }

#[cfg(any(feature = "tests-effects", feature = "tests-ships", feature = "tests-inventory", feature = "tests-economy", feature = "tests-social", feature = "tests-content", feature = "tests-services"))]
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

#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-inventory", feature = "tests-economy", feature = "tests-social", feature = "tests-content", feature = "tests-services"))]
pub(in crate::services) async fn assign_corporation(db: &Db, character: i64, corporation: i64, roles: i64) -> Result<(), Error> {
    let local = infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1 | infinity_core::consts::ROLE_HANGAR_CAN_QUERY_1;
    db.execute("UPDATE game.characters SET corporation_id=$2,corp_roles=$3,corp_roles_hq=$4,corp_roles_base=0,corp_roles_other=0 WHERE character_id=$1", &[&character, &corporation, &(roles & !local), &(roles & local)]).await?;
    Ok(())
}

#[cfg(any(feature = "tests-ships", feature = "tests-inventory", feature = "tests-economy", feature = "tests-services"))]
pub(in crate::services) async fn create_office(db: &Db, office: i64, corporation: i64, station: i64) -> Result<(), Error> {
    crate::dao::offices::insert(db, office, corporation, station, i64::MAX, 0).await?;
    Ok(())
}

#[cfg(any(feature = "tests-effects", feature = "tests-ships", feature = "tests-inventory", feature = "tests-economy", feature = "tests-social", feature = "tests-content", feature = "tests-services"))]
pub(in crate::services) async fn cleanup_corporation(db: &Db, corporation: i64, members: &[i64], office: i64) -> Result<(), Error> {
    db.transaction(|tx, _: &mut ()| {
        let members = members.to_vec();
        Box::pin(async move {
            cleanup_progress(tx, &[corporation], &[corporation]).await?;
            tx.execute("DELETE FROM game.bill_autopay WHERE owner_id = $1", &[&corporation]).await?;
            tx.execute("DELETE FROM game.bills WHERE debtor_id = $1 OR creditor_id = $1", &[&corporation]).await?;
            tx.execute("DELETE FROM game.items WHERE owner_id = $1", &[&corporation]).await?;
            tx.execute("DELETE FROM game.offices WHERE office_id = $1", &[&office]).await?;
            tx.execute("DELETE FROM game.corp_wallets WHERE corporation_id = $1", &[&corporation]).await?;
            tx.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id = $1)", &[&corporation]).await?;
            tx.execute("DELETE FROM game.wallet_accounts WHERE owner_id = $1", &[&corporation]).await?;
            tx.execute("DELETE FROM game.employment_history WHERE char_id = ANY($1)", &[&members]).await?;
            tx.execute("UPDATE game.characters SET corporation_id = 1, corp_roles = 0 WHERE character_id = ANY($1)", &[&members]).await?;
            tx.execute("DELETE FROM game.corporations WHERE corporation_id = $1", &[&corporation]).await?;
            Ok(())
        })
    }, |(), ()| async move { Ok(()) }).await
}
#[cfg(any(feature = "tests-skills", feature = "tests-ships", feature = "tests-social"))]
pub(in crate::services) struct LiveRecipient {
    world: std::sync::Arc<crate::space::World>,
    pub(in crate::services) system: i64,
    pub(in crate::services) item: i64,
    membership: u64,
}

#[cfg(any(feature = "tests-skills", feature = "tests-ships", feature = "tests-social"))]
impl LiveRecipient {
    pub(in crate::services) async fn cold(
        world: &std::sync::Arc<crate::space::World>, owner: i64, avoid: Option<i64>,
        market: &crate::services::market::economy_lock::Guard,
    ) -> Self {
        use infinity_core::consts::CATEGORY_DRONE;
        let sde = world.sde();
        let system = sde.map.systems().find(|row| Some(row.solar_system_id) != avoid)
            .expect("separate fixture system").solar_system_id;
        assert!(world.with(system, |_| ()).await.is_none(), "recipient requires a cold system");
        let kind = sde.types.iter().find(|row| row.published == Some(true) && row.category_id == Some(CATEGORY_DRONE))
            .expect("published drone").type_id;
        let item = crate::dao::items::creation::positioned(world.db(), kind, owner, system, [0.0; 3])
            .await.unwrap();
        let mut live = Self { world: world.clone(), system, item, membership: 0 };
        let result = crate::tests::catch_scenario(async {
            world.system_locked(system, market).await.unwrap();
            live.membership = world.with(system, move |s| {
                assert!(s.pilot_ball(owner).is_none());
                let drone = s.drone(item).unwrap();
                assert_eq!(drone.owner, owner);
                assert!(drone.controller.is_none());
                s.with_combat_fit(item, |_, _, stamp| stamp.membership).unwrap()
            }).await.unwrap();
        }).await;
        if let Err(panic) = result {
            live.close().await;
            std::panic::resume_unwind(panic);
        }
        live
    }

    pub(in crate::services) async fn read<R: Send + 'static>(
        &self, read: impl FnOnce(&crate::services::fit::Fit) -> R + Send + 'static,
    ) -> R {
        let (item, membership) = (self.item, self.membership);
        self.world.with(self.system, move |s| s.with_combat_fit(item, move |fit, _, stamp| {
            assert_eq!(stamp.membership, membership, "recipient was replaced");
            read(fit)
        })).await.flatten().expect("same live recipient")
    }

    pub(in crate::services) async fn close(self) {
        self.world.remove_ball(self.system, self.item).await;
        crate::services::combat::forget_ship(self.item);
        self.world.flush_positions().await.unwrap();
        self.world.db().execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&self.item]).await.unwrap();
        self.world.db().execute("DELETE FROM game.items WHERE item_id=$1", &[&self.item]).await.unwrap();
    }
}
#[cfg(feature = "tests-skills")]
pub(in crate::services) fn collected_notifications(
    outbox: crate::db::outbox::Outbox<crate::services::effects::online::OnlineChanged>, world: &crate::space::World,
) -> Vec<crate::db::outbox::Effect<crate::services::effects::online::OnlineChanged>> {
    let mut operations = 0;
    let mut notifications = Vec::new();
    crate::services::effects::online::dispatch_mixed_entries((), outbox, world,
        |change| {
            assert!(matches!(change, crate::services::effects::online::OnlineChanged::Character { .. }), "unexpected operation");
            operations += 1;
            Ok(change)
        },
        |_| unreachable!("only Character operations are admitted"),
        |notification| {
            notifications.push(crate::db::outbox::Effect::Notification(notification));
            Ok(())
        },
    ).unwrap();
    assert_eq!(operations, 1, "missing or duplicate Character operation");
    notifications
}

#[cfg(any(feature = "tests-combat", feature = "tests-effects", feature = "tests-lua"))]
pub(crate) async fn publish_fitting(
    world: &std::sync::Arc<crate::space::World>, system: i64, owner: i64, ship: i64,
    fit: crate::services::fit::Fit,
) -> bool {
    let generation = world.with(system, move |s| s.combat_membership_generation(ship))
        .await.flatten().expect("resident fit generation");
    world.with(system, move |s| {
        let patch = s.with_combat_fit(ship, |before, _, _| fit.into_committed_fitting(before))?;
        Some(s.publish_fitting(ship, owner, generation, patch, vec![]))
    }).await.flatten().unwrap_or(false)
}

/// Hull placement for `create_ship`: docked inserts the supplied station as
/// the item location; space uses the actor-ready DAO helpers.
#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-slash", feature = "tests-runtime"))]
#[derive(Clone, Copy)]
pub(crate) enum ShipLocation {
    #[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-slash"))]
    Docked { station: i64 },
    #[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-runtime"))]
    Space { system: i64 },
}

/// One carried or fitted item on a shared fixture ship, created in spec order.
#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-slash", feature = "tests-runtime"))]
#[derive(Clone, Copy)]
pub(crate) struct ShipItemSpec<'a> {
    pub type_id: i32,
    pub flag_id: i32,
    pub quantity: i64,
    pub singleton: bool,
    pub online: bool,
    pub attributes: &'a [(i32, f64)],
}

/// Account, character, hull and the created item ids in spec order.
#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-slash", feature = "tests-runtime"))]
pub(crate) struct ShipFixture { pub account: i64, pub character: i64, pub ship: i64, pub items: Vec<i64> }

#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-slash", feature = "tests-runtime"))]
impl ShipFixture {
    /// First created item; module-bearing fixtures place the module first.
    #[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-slash"))]
    pub(crate) fn module(&self) -> i64 { *self.items.first().expect("fixture ship has no module") }
}

/// One transaction: role-0 account and character, then the hull and each spec
/// item under it with its optional `game.item_attributes` overrides.
#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-slash", feature = "tests-runtime"))]
pub(crate) async fn create_ship(db: &Db, ship_type: i32, location: ShipLocation, items: &[ShipItemSpec<'_>]) -> Result<ShipFixture, Error> {
    db.transaction(|tx, _: &mut ()| {
        let items: Vec<(i32, i32, i64, bool, bool, Vec<(i32, f64)>)> = items.iter()
            .map(|spec| (spec.type_id, spec.flag_id, spec.quantity, spec.singleton, spec.online, spec.attributes.to_vec()))
            .collect();
        Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('ship-fixture-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let character = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'ship-fixture-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
        let ship = match location {
            #[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-slash"))]
            ShipLocation::Docked { station } => tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z) VALUES ($1, $2, $3, 0, 1, true, 0, 0, 0) RETURNING item_id", &[&ship_type, &character, &station]).await?.get("item_id"),
            #[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-runtime"))]
            ShipLocation::Space { system } => {
                let ship = crate::dao::items::creation::positioned(tx, ship_type, character, system, [0.0; 3]).await?;
                crate::dao::characters::selection::set_ship(tx, character, ship).await?;
                crate::dao::characters::ship::undock(tx, character, system).await?;
                ship
            }
        };
        let mut created = Vec::with_capacity(items.len());
        for (type_id, flag_id, quantity, singleton, online, attributes) in items {
            let item: i64 = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, online) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING item_id", &[&type_id, &character, &ship, &flag_id, &quantity, &singleton, &online]).await?.get("item_id");
            for (attribute, value) in attributes {
                tx.execute("INSERT INTO game.item_attributes (item_id, attribute_id, value) VALUES ($1, $2, $3)", &[&item, &attribute, &value]).await?;
            }
            created.push(item);
        }
        Ok(ShipFixture { account, character, ship, items: created })
        })
    }, |fixture, ()| async move { Ok(fixture) }).await
}

/// Sole teardown for `create_ship` fixtures. With a world, the hull's ball is
/// removed at its persisted location, the pilot generation unregistered and
/// the combat cache forgotten, then one position flush; `None` never loads an
/// actor. One transaction removes the fixture-owned rows in dependency order.
#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-slash", feature = "tests-runtime"))]
pub(crate) async fn cleanup_ship(db: &Db, world: Option<&crate::space::World>, fixture: ShipFixture) -> Result<(), Error> {
    if let Some(world) = world {
        if let Some(system) = db.query_opt("SELECT location_id FROM game.items WHERE item_id = $1", &[&fixture.ship]).await?.map(|row| row.get::<_, i64>("location_id")) {
            world.remove_ball(system, fixture.ship).await;
        }
        world.unregister_pilot_generation(fixture.character, None);
        crate::services::combat::forget_ship(fixture.ship);
        world.flush_positions().await.map_err(Error::Progress)?;
    }
    let mut ids = fixture.items.clone();
    ids.push(fixture.ship);
    db.transaction(|tx, _: &mut ()| {
        let ids = ids.clone();
        Box::pin(async move {
        tx.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE kind = 'ItemsMoved' AND (payload->'ItemsMoved'->>'item_id')::bigint = ANY($1))", &[&ids]).await?;
        tx.execute("DELETE FROM game.progress_events WHERE kind = 'ItemsMoved' AND (payload->'ItemsMoved'->>'item_id')::bigint = ANY($1)", &[&ids]).await?;
        cleanup_progress(tx, &[fixture.account, fixture.character], &[fixture.character]).await?;
        tx.execute("DELETE FROM game.pending_reloads WHERE owner_id = $1 AND (ship_id = ANY($2) OR source_location_id = ANY($2))", &[&fixture.character, &ids]).await?;
        tx.execute("DELETE FROM game.module_charges WHERE ship_id = ANY($1)", &[&ids]).await?;
        tx.execute("DELETE FROM game.weapon_banks WHERE ship_id = ANY($1)", &[&ids]).await?;
        tx.execute("DELETE FROM game.item_attributes WHERE item_id = ANY($1)", &[&ids]).await?;
        crate::dao::jobs::cancel(tx, &format!("{}:{}", crate::services::skills::completion::KIND, fixture.character)).await?;
        tx.execute("DELETE FROM game.skill_history WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.skill_queue WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.skills WHERE character_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id = $1)", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.wallet_accounts WHERE owner_id = $1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.items WHERE owner_id = $1 AND item_id = ANY($2)", &[&fixture.character, &ids]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id = $1 AND user_id = $2", &[&fixture.character, &fixture.account]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&fixture.account]).await?;
        Ok(())
        })
    }, |(), ()| async move { Ok(()) }).await
}
