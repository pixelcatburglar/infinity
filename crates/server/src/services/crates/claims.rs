//! ClaimLoot receipt acknowledgement.
//! Protocol evidence: `lootItem.py:57` and hacking callers send the reward item,
//! destination location and destination flag. Claiming acknowledges a durable
//! server receipt only; it never moves or creates an item and emits no notice.

use crate::{dao::items::crates, services::{db_err, CallResult}, client::Client};
use crate::services::identity::Identity;
use marshal::Value;

/// Validate and acknowledge one direct-list opening receipt atomically.
pub(crate) async fn claim_loot(c: &Client, reward_item_id: i64, destination_id: i64, destination_flag: i32) -> CallResult {
    let character_id = Identity::of_client(c).char_id;
    if character_id <= 0 { return Err(super::invalid_source()); }
    let acknowledged_at = crate::client::filetime_now();
    c.db.transaction(|tx, _: &mut ()| Box::pin(async move {
        if crates::acknowledge(tx, reward_item_id, character_id, destination_id, destination_flag, acknowledged_at).await.map_err(db_err)? {
            Ok(Value::None)
        } else {
            Err(super::invalid_source())
        }
    }), |value, _| async move { Ok(value) }).await
}

pub(crate) fn arguments(call: &crate::services::Call<'_>) -> Option<(i64, i64, i32)> {
    let [item, destination, flag] = call.args else { return None };
    Some((crate::services::value_int(Some(item))?, crate::services::value_int(Some(destination))?, i32::try_from(crate::services::value_int(Some(flag))?).ok()?))
}
