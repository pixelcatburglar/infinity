//! ClaimLoot receipt acknowledgement.
//! Protocol evidence: `lootItem.py:57` and hacking callers send the reward item,
//! destination location and destination flag. Claiming acknowledges a durable
//! server receipt only; it never moves or creates an item and emits no notice.

use infinity_core::messages::crates as messages;
use crate::{dao::items::crates, services::{db_err, notify_error, CallError, CallResult}, client::Client};
use crate::services::identity::Identity;
use marshal::Value;

fn invalid() -> CallError { notify_error(messages::INVALID_SOURCE) }

/// Validate and acknowledge one direct-list opening receipt atomically.
pub(crate) async fn claim_loot(c: &Client, reward_item_id: i64, destination_id: i64, destination_flag: i32) -> CallResult {
    let character_id = Identity::of_client(c).char_id;
    if character_id <= 0 { return Err(invalid()); }
    let acknowledged_at = crate::client::filetime_now();
    c.db.transaction(|tx, _: &mut ()| Box::pin(async move {
        if crates::acknowledge(tx, reward_item_id, character_id, destination_id, destination_flag, acknowledged_at).await.map_err(db_err)? {
            Ok(Value::None)
        } else {
            Err(invalid())
        }
    }), |value, _| async move { Ok(value) }).await
}

pub(crate) fn arguments(call: &crate::services::Call<'_>) -> Option<(i64, i64, i32)> {
    let [Value::Int(item), Value::Int(destination), Value::Int(flag)] = call.args else { return None };
    Some((*item, *destination, i32::try_from(*flag).ok()?))
}
