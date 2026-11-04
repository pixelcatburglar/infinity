//! Round-one direct-list crate opening.
//! Only owned, reachable, unbound ordinary definitions are accepted.

use crate::{client::Client, consts::{FLAG_CARGO, FLAG_HANGAR, IX_LOCATION_ID, IX_STACK_SIZE}, dao::items::{crates as receipt, quantities::{self, QuantityChange}, transfers, loading::ItemQuery}, db::outbox::{dispatch_client, Outbox}, messages::crates as messages, services::{db_err, notify_error, CallError, CallResult}, services::identity::Identity, services::inventory::{self, reachability::{Endpoint, Reachability}}, services::items::load_items};
use marshal::Value;

/// Client crate controller's multi-open limit.
const MAX_OPEN: i32 = 30;

fn invalid() -> CallError { notify_error(messages::INVALID_SOURCE) }

pub(super) async fn open(c: &mut Client, source_id: i64, amount: i32, stack_response: bool) -> CallResult {
    let identity = Identity::of_client(c);
    if identity.char_id == 0 || !(1..=MAX_OPEN).contains(&amount) { return Err(invalid()); }
    let item = load_items(&c.db, &c.sde, ItemQuery::Id(source_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(invalid)?;
    if item.singleton || item.quantity < amount { return Err(invalid()); }
    if item.owner_id != identity.char_id {
        let Some((corp, _, flag)) = inventory::access::corporate_custody(&c.db, Some(&c.sde), source_id).await? else { return Err(invalid()); };
        if item.owner_id != corp || !inventory::access::may_take_corp_hangar(&c.db, identity.char_id, corp, flag).await? { return Err(invalid()); }
    }
    let definition = c.sde.crates.get(item.type_id).ok_or_else(invalid)?;
    let rewards = definition.preview(false, None).map_err(|_| notify_error(messages::METADATA_UNAVAILABLE))?;
    if rewards.is_empty() { return Err(notify_error(messages::OPENING_UNAVAILABLE)); }
    let (destination_location, destination_flag) = match identity.station_id {
        Some(station) => (station, FLAG_HANGAR),
        None => (identity.ship_id.ok_or_else(invalid)?, FLAG_CARGO),
    };
    let endpoint = Endpoint { location: item.location_id, flag: item.flag_id };
    let reach = Reachability::capture(&identity, &c.db, &c.world, c.session.int("solarsystemid"), &[endpoint]).await?;
    reach.check(&c.db, &c.sde, endpoint).await?;
    let recipient = identity.char_id;
    let ship_id = identity.ship_id;
    let sde = c.sde.clone();
    let db = c.db.clone();
    let source = item.clone();
    let destination = (destination_location, destination_flag);
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let rewards = rewards.clone();
            let source = source.clone();
            let sde = sde.clone();
            Box::pin(async move {
                let mut total_volume = 0.0_f64;
                let mut quantities = Vec::with_capacity(rewards.len());
                for (type_id, per_open) in rewards {
                    let quantity = per_open.checked_mul(i64::from(amount)).and_then(|q| i32::try_from(q).ok()).ok_or_else(invalid)?;
                    let volume = sde.types.get(type_id).and_then(|ty| ty.volume).unwrap_or(0.0);
                    total_volume += volume * f64::from(quantity);
                    quantities.push((type_id, quantity));
                }
                if destination_flag == FLAG_CARGO {
                    let Some((used, capacity)) = receipt::cargo_capacity(tx, ship_id.ok_or_else(invalid)?, destination.0, destination.1).await.map_err(db_err)? else { return Err(invalid()); };
                    if !used.is_finite() || !capacity.is_finite() || used + total_volume > capacity { return Err(invalid()); }
                }
                let remaining = source.quantity.checked_sub(amount).ok_or_else(invalid)?;
                let mut changes = std::collections::BTreeMap::new();
                changes.insert(source_id, QuantityChange::new(&source, (remaining > 0).then_some(remaining)));
                if !quantities::apply(tx, &changes).await.map_err(db_err)? { return Err(invalid()); }
                if remaining == 0 {
                    inventory::notifications::notify_item_removed(outbox, source.clone());
                } else {
                    inventory::notifications::notify_item_change(tx, &sde, outbox, source_id, vec![(IX_STACK_SIZE, Value::Int(i64::from(remaining))) ]).await?;
                }
                let now = crate::client::filetime_now();
                let mut rows = Vec::with_capacity(quantities.len());
                for (type_id, quantity) in quantities {
                    let id = transfers::insert(tx, type_id, recipient, destination_location, destination_flag, quantity).await.map_err(db_err)?;
                    receipt::insert(tx, receipt::RewardReceipt { item_id: id, character_id: recipient, crate_type_id: source.type_id, destination_location, destination_flag, opened_at: now }).await.map_err(db_err)?;
                    inventory::notifications::notify_item_change(tx, &sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(destination_location)), (crate::consts::IX_FLAG, Value::Int(i64::from(destination_flag))), (IX_STACK_SIZE, Value::Int(i64::from(quantity))) ]).await?;
                    rows.push(Value::tuple([Value::Int(id), Value::Int(i64::from(type_id)), Value::Int(i64::from(quantity))]));
                }
                if stack_response { Ok(Value::tuple([Value::Int(i64::from(amount)), Value::Int(i64::from(amount))])) } else { Ok(Value::list(rows)) }
            })
        },
        move |value, outbox| async move { dispatch_client(value, outbox, c) },
    ).await
}
