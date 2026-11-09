//! Ordinary crate destination queries and transactional opening for the client crate controller.
//! Authorized shared sources preserve guest ownership until consumption; rewards go
//! to the opener's station hangar or active ship cargo. Uses game.items and crate
//! receipts; special and custom crate definitions remain unavailable.
//! The open-reply reward rows are projected to the client display limit;
//! inserted item quantities stay exact.

use infinity_core::{consts::{FLAG_CARGO, FLAG_HANGAR, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD}, messages::crates as messages};
use crate::{client::Client, dao::items::{crates as receipt, quantities::{self, QuantityChange}, transfers, loading::{Item, ItemQuery}}, db::outbox::{dispatch_client, Outbox}, services::{db_err, notify_error, CallError, CallResult}, services::identity::Identity, services::inventory::{self, access::Container, notifications::Audience, reachability::{Endpoint, Reachability}}, services::items::load_items};
use marshal::Value;

/// Client crate controller's multi-open limit.
const MAX_OPEN: i32 = 30;

fn invalid() -> CallError { notify_error(messages::INVALID_SOURCE) }

pub(super) fn reward_quantity(per_open: i64, amount: i32) -> Option<i64> {
    per_open.checked_mul(i64::from(amount))
}

struct Preparation {
    identity: Identity,
    item: Item,
    rewards: Vec<(i32, i64)>,
    destination_location: i64,
    destination_flag: i32,
    reach: Reachability,
    container: Container,
}

async fn prepare(c: &Client, source_id: i64, amount: i32) -> Result<Preparation, CallError> {
    let identity = Identity { system_id: c.session.int("solarsystemid"), ..Identity::of_client(c) };
    if identity.char_id == 0 || !(1..=MAX_OPEN).contains(&amount) { return Err(invalid()); }
    let item = load_items(&c.db, &c.sde, ItemQuery::Id(source_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(invalid)?;
    if item.singleton || item.quantity < i64::from(amount) { return Err(invalid()); }
    let definition = c.sde.crates.get(item.type_id).ok_or_else(invalid)?;
    let rewards = definition.preview(false, None).map_err(|_| notify_error(messages::METADATA_UNAVAILABLE))?;
    if rewards.is_empty() { return Err(notify_error(messages::OPENING_UNAVAILABLE)); }
    let (destination_location, destination_flag) = match identity.station_id {
        Some(station) => (station, FLAG_HANGAR),
        None => (identity.ship_id.ok_or_else(invalid)?, FLAG_CARGO),
    };
    let endpoint = Endpoint { location: item.location_id, flag: item.flag_id };
    let destination_endpoint = Endpoint { location: destination_location, flag: destination_flag };
    let endpoints = [endpoint, destination_endpoint];
    let reach = Reachability::capture(&identity, &c.db, &c.world, identity.system_id, &endpoints).await?;
    let container = if identity.station_id == Some(destination_location) {
        Container::Hangar { station_id: destination_location, owner_id: identity.char_id }
    } else { Container::Item(destination_location) };
    let (allowed, _) = inventory::movement::authorize_move(&c.db, &c.sde, &identity, &reach, &item, Some(item.location_id), &container, Some(destination_flag)).await?.ok_or_else(invalid)?;
    if allowed.location != destination_location || allowed.flag != destination_flag || allowed.item_owner != identity.char_id { return Err(invalid()); }
    Ok(Preparation { identity, item, rewards, destination_location, destination_flag, reach, container })
}

pub(super) async fn location(c: &Client, source_id: i64) -> CallResult {
    let prepared = prepare(c, source_id, 1).await?;
    Ok(Value::tuple([Value::Int(prepared.destination_location), Value::Int(i64::from(prepared.destination_flag))]))
}

pub(super) async fn open(c: &mut Client, source_id: i64, amount: i32, stack_response: bool) -> CallResult {
    let _market = crate::services::market::lock().await;
    let Preparation { identity, item, rewards, destination_location, destination_flag, reach, container } = prepare(c, source_id, amount).await?;
    let endpoints = [Endpoint { location: item.location_id, flag: item.flag_id }, Endpoint { location: destination_location, flag: destination_flag }];
    let audience = Audience::capture(&identity, &c.db, &c.world, identity.system_id, &endpoints).await?;
    let recipient = identity.char_id;
    let sde = c.sde.clone();
    let db = c.db.clone();
    let source = item.clone();
    let destination = (destination_location, destination_flag);
    db.transaction(
        move |tx, outbox: &mut Outbox| {
            let rewards = rewards.clone();
            let source = source.clone();
            let sde = sde.clone();
            let reach = reach.clone();
            let audience = audience.clone();
            let identity = identity.clone();
            let container = container.clone();
            Box::pin(async move {
                use crate::dao::items::parent_locks::{LockRequest, LockStrength};
                let source_lineage = if crate::services::items::is_static_location(&sde, source.location_id) {
                    Vec::new()
                } else {
                    crate::dao::items::deliveries::ancestry(tx, source.location_id).await.map_err(db_err)?
                };
                let destination_lineage = if destination_location == source.location_id
                    || crate::services::items::is_static_location(&sde, destination_location) {
                    Vec::new()
                } else {
                    crate::dao::items::deliveries::ancestry(tx, destination_location).await.map_err(db_err)?
                };
                let mut requests = vec![LockRequest { item_id: source_id, strength: LockStrength::Update }];
                requests.extend(inventory::destination::parent_requests(&sde,
                    source_lineage.iter().chain(destination_lineage.iter()).map(|row| (row.item_id, row.type_id)),
                    LockStrength::Update, LockStrength::Share)?);
                if let Some(parent) = source_lineage.iter().find(|row| row.item_id == source.location_id)
                    .filter(|row| inventory::hold_storage::is_ordinary_container(&sde, row.type_id)) {
                    requests.push(LockRequest { item_id: parent.item_id, strength: LockStrength::Update });
                }
                let _locks = inventory::destination::lock_items(tx, &sde, &requests, &[]).await?;
                if (!crate::services::items::is_static_location(&sde, source.location_id)
                    && crate::dao::items::deliveries::ancestry(tx, source.location_id).await.map_err(db_err)? != source_lineage)
                    || (destination_location != source.location_id && !crate::services::items::is_static_location(&sde, destination_location)
                        && crate::dao::items::deliveries::ancestry(tx, destination_location).await.map_err(db_err)? != destination_lineage) {
                    return Err(invalid());
                }
                let current = load_items(tx, &sde, ItemQuery::Id(source_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(invalid)?;
                if current.owner_id != source.owner_id || current.location_id != source.location_id || current.flag_id != source.flag_id
                    || current.type_id != source.type_id || current.quantity != source.quantity || current.singleton != source.singleton {
                    return Err(invalid());
                }
                let (allowed, _) = inventory::movement::authorize_move(tx, &sde, &identity, &reach, &current, Some(current.location_id), &container, Some(destination_flag)).await?.ok_or_else(invalid)?;
                if allowed.location != destination_location || allowed.flag != destination_flag || allowed.item_owner != recipient { return Err(invalid()); }
                if !crate::dao::trades::reserved(tx, &[source_id, source.location_id]).await.map_err(db_err)?.is_empty()
                    || crate::dao::industry::has_active_blueprint_job(tx, &[source_id]).await.map_err(db_err)? { return Err(invalid()); }
                let viewers = audience.resolve(tx, &sde).await?;
                let mut quantities = Vec::with_capacity(rewards.len());
                for (type_id, per_open) in rewards {
                    let quantity = reward_quantity(per_open, amount).ok_or_else(invalid)?;
                    quantities.push((type_id, quantity));
                }
                let same_hold = source.location_id == destination.0 && source.flag_id == destination.1;
                let mut additions: Vec<_> = quantities.iter().map(|(type_id, quantity)| inventory::hold_storage::Addition {
                    type_id: *type_id, singleton: false, quantity: *quantity,
                }).collect();
                if same_hold && source.quantity > i64::from(amount) {
                    additions.push(inventory::hold_storage::Addition {
                        type_id: source.type_id, singleton: false, quantity: source.quantity - i64::from(amount),
                    });
                }
                let exclude = if same_hold { std::slice::from_ref(&source_id) } else { &[] };
                inventory::hold_storage::check_destination(tx, &sde, allowed.capacity_owner, destination.0, destination.1, &additions, inventory::hold_storage::Replaced { items: exclude, reloads: &[] }, None).await?;
                let remaining = source.quantity.checked_sub(i64::from(amount)).ok_or_else(invalid)?;
                let mut changes = std::collections::BTreeMap::new();
                changes.insert(source_id, QuantityChange::new(&source, (remaining > 0).then_some(remaining)));
                if !quantities::apply(tx, &changes).await.map_err(db_err)? { return Err(invalid()); }
                let new_source = if remaining == 0 {
                    crate::dao::items::loading::Item { location_id: LOCATION_JUNKYARD, ..source.clone() }
                } else {
                    crate::dao::items::loading::Item { quantity: remaining, ..source.clone() }
                };
                viewers.item_change(outbox, &source, &new_source, vec![(IX_STACK_SIZE, Value::Int(source.quantity))], recipient);
                if remaining == 0 {
                    inventory::notifications::notify_item_removed(outbox, source.clone());
                } else {
                    inventory::notifications::notify_item_change(tx, &sde, outbox, source_id, vec![(IX_STACK_SIZE, Value::Int(source.quantity)) ]).await?;
                }
                let now = crate::client::filetime_now();
                let mut rows = Vec::with_capacity(quantities.len());
                for (type_id, quantity) in quantities {
                    let id = transfers::insert(tx, type_id, recipient, destination_location, destination_flag, quantity).await.map_err(db_err)?;
                    receipt::insert(tx, receipt::RewardReceipt { item_id: id, character_id: recipient, crate_type_id: source.type_id, destination_location, destination_flag, opened_at: now }).await.map_err(db_err)?;
                    inventory::notifications::notify_item_change(tx, &sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(destination_location)), (infinity_core::consts::IX_FLAG, Value::Int(i64::from(destination_flag))), (IX_STACK_SIZE, Value::Int(quantity)) ]).await?;
                    rows.push(Value::tuple([Value::Int(id), Value::Int(i64::from(type_id)), Value::Int(crate::services::dogma::info::cap_client_quantity(quantity))]));
                }
                if stack_response { Ok(Value::tuple([Value::Int(i64::from(amount)), Value::Int(i64::from(amount))])) } else { Ok(Value::list(rows)) }
            })
        },
        move |value, outbox| async move { dispatch_client(value, outbox, c) },
    ).await
}
