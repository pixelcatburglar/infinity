//! Scoped database snapshots for isolated service fixtures.
use crate::db::{Db, Error};

pub(crate) async fn structure_snapshot(db: &Db, character: i64) -> Result<String, Error> {
    let row = db.query_one("SELECT COALESCE((SELECT row_to_json(c)::text FROM game.characters c WHERE character_id=$1), '') || '|' || (SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') FROM game.items i WHERE owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(w)::text, '' ORDER BY account_id), '') FROM game.wallet_accounts w WHERE owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(j)::text, '' ORDER BY j.journal_id), '') FROM game.wallet_journal j WHERE j.account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1)) AS snapshot", &[&character]).await?;
    Ok(row.get("snapshot"))
}

pub(crate) async fn reward_snapshot(db: &Db, user: i64) -> Result<String, Error> {
    let row = db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(c)::text, '' ORDER BY user_id, campaign_id, day), '') FROM game.login_reward_claims c WHERE user_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(r)::text, '' ORDER BY user_id, campaign_id, day), '') FROM game.login_reward_retro_grants r WHERE user_id=$1) AS snapshot", &[&user]).await?;
    Ok(row.get("snapshot"))
}
