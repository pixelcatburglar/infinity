//! Scoped database snapshots for isolated service fixtures.
#[cfg(feature = "tests-services")]
use crate::db::Db;
#[cfg(any(feature = "tests-combat", feature = "tests-economy", feature = "tests-effects", feature = "tests-inventory", feature = "tests-lua", feature = "tests-runtime", feature = "tests-services", feature = "tests-ships", feature = "tests-slash", feature = "tests-social", feature = "tests-space"))]
use crate::db::{executor::Executor, Error};

#[cfg(feature = "tests-services")]
pub(crate) async fn structure_snapshot(db: &Db, character: i64) -> Result<String, Error> {
    let row = db.query_one("SELECT COALESCE((SELECT row_to_json(c)::text FROM game.characters c WHERE character_id=$1), '') || '|' || (SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') FROM game.items i WHERE owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(w)::text, '' ORDER BY account_id), '') FROM game.wallet_accounts w WHERE owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(j)::text, '' ORDER BY j.journal_id), '') FROM game.wallet_journal j WHERE j.account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1)) AS snapshot", &[&character]).await?;
    Ok(row.get("snapshot"))
}

#[cfg(feature = "tests-services")]
pub(crate) async fn reward_snapshot(db: &Db, user: i64) -> Result<String, Error> {
    let row = db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(c)::text, '' ORDER BY user_id, campaign_id, day), '') FROM game.login_reward_claims c WHERE user_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(r)::text, '' ORDER BY user_id, campaign_id, day), '') FROM game.login_reward_retro_grants r WHERE user_id=$1) AS snapshot", &[&user]).await?;
    Ok(row.get("snapshot"))
}

#[cfg(any(feature = "tests-combat", feature = "tests-economy", feature = "tests-effects", feature = "tests-inventory", feature = "tests-lua", feature = "tests-runtime", feature = "tests-services", feature = "tests-ships", feature = "tests-slash", feature = "tests-social", feature = "tests-space"))]
#[derive(Debug, PartialEq)]
pub(crate) struct ItemRowState { pub(crate) item_id: i64, pub(crate) state: String, pub(crate) coordinates: [Option<f64>; 3] }

#[cfg(any(feature = "tests-combat", feature = "tests-economy", feature = "tests-effects", feature = "tests-inventory", feature = "tests-lua", feature = "tests-runtime", feature = "tests-services", feature = "tests-ships", feature = "tests-slash", feature = "tests-social", feature = "tests-space"))]
#[derive(Debug, PartialEq)]
pub(crate) struct ItemState {
    pub(crate) items: Vec<ItemRowState>,
    attributes: Vec<String>, charges: Vec<String>,
    banks: Vec<String>, reloads: Vec<String>,
}
#[cfg(any(feature = "tests-combat", feature = "tests-economy", feature = "tests-effects", feature = "tests-lua", feature = "tests-runtime", feature = "tests-services", feature = "tests-ships", feature = "tests-slash", feature = "tests-social", feature = "tests-space"))]
impl ItemState {
    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty() && self.attributes.is_empty() && self.charges.is_empty() && self.banks.is_empty() && self.reloads.is_empty()
    }
}

/// Ordered five-category row state for the supplied item IDs plus their
/// current descendants; supplied IDs stay in scope even when moved or deleted.
#[cfg(any(feature = "tests-combat", feature = "tests-economy", feature = "tests-effects", feature = "tests-inventory", feature = "tests-lua", feature = "tests-runtime", feature = "tests-services", feature = "tests-ships", feature = "tests-slash", feature = "tests-social", feature = "tests-space"))]
pub(crate) async fn item_state(ex: &(impl Executor + ?Sized), item_ids: &[i64]) -> Result<ItemState, Error> {
    let mut ids = crate::dao::items::inventory::descendants(ex, item_ids).await?;
    ids.extend_from_slice(item_ids);
    ids.sort_unstable();
    ids.dedup();
    let items = ex.query("SELECT i.item_id, (to_jsonb(i) - 'x' - 'y' - 'z')::text AS state, i.x, i.y, i.z FROM game.items i WHERE i.item_id = ANY($1) ORDER BY i.item_id", &[&ids]).await?
        .iter().map(|row| ItemRowState { item_id: row.get("item_id"), state: row.get("state"), coordinates: [row.get("x"), row.get("y"), row.get("z")] }).collect();
    let attributes = ex.query("SELECT row_to_json(a)::text AS row FROM game.item_attributes a WHERE a.item_id = ANY($1) ORDER BY a.item_id, a.attribute_id", &[&ids]).await?.iter().map(|row| row.get("row")).collect();
    let charges = ex.query("SELECT row_to_json(c)::text AS row FROM game.module_charges c WHERE c.ship_id = ANY($1) ORDER BY c.ship_id, c.flag_id", &[&ids]).await?.iter().map(|row| row.get("row")).collect();
    let banks = ex.query("SELECT row_to_json(b)::text AS row FROM game.weapon_banks b WHERE b.ship_id = ANY($1) OR b.master_id = ANY($1) OR b.slave_id = ANY($1) ORDER BY b.slave_id", &[&ids]).await?.iter().map(|row| row.get("row")).collect();
    let reloads = ex.query("SELECT row_to_json(r)::text AS row FROM game.pending_reloads r WHERE r.ship_id = ANY($1) OR r.module_id = ANY($1) OR r.source_location_id = ANY($1) ORDER BY r.reload_id", &[&ids]).await?.iter().map(|row| row.get("row")).collect();
    Ok(ItemState { items, attributes, charges, banks, reloads })
}
