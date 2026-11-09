//! In-space compression for `itemcompression/client/itemCompression.py` and
//! `itemcompression/client/inSpaceCompression.py`. The one-to-one preview in
//! `itemcompression/client/ui/compression_window.py:150-157` uses the native SDE
//! mapping; inventory is persisted in game.items. Explicit compression uses fresh
//! inventory access checks, preserving shared-bay stack ownership. Wire stack
//! quantities project to the client display limit; stored amounts stay exact.
//! Only live own or fleet ship compressors are supported, not automatic
//! compression or structures.
use std::collections::BTreeMap;
use infinity_core::{consts::{CATEGORY_SHIP, FLAG_CARGO, FLAG_FLEET_HANGAR, FLAG_GENERAL_MINING_HOLD, FLAG_SPECIAL_ASTEROID_HOLD, FLAG_SPECIALIZED_ICE_HOLD, FLAG_SPECIALIZED_GAS_HOLD, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD}, messages::mining as msg};
use crate::{client::Client, dao::{self, items::{loading::{Item, ItemQuery}, quantities::{self, QuantityChange}, transfers}}, db::{executor::Executor, outbox::{dispatch_client, Outbox}}, sde::Sde, services::{db_err, fleets, inventory, items::load_items, market, notify_error, Call, CallError, CallResult}, space::dist};
use marshal::Value;
use std::sync::Arc;

#[derive(Clone)]
struct Context { character: i64, ship: i64, system: i64 }

#[derive(Clone, Copy, PartialEq, Eq)]
struct SourceGuard { item_id: i64, type_id: i32, owner_id: i64, location_id: i64, flag_id: i32, quantity: i64, singleton: bool }

impl From<&Item> for SourceGuard {
    fn from(item: &Item) -> Self {
        Self { item_id: item.item_id, type_id: item.type_id, owner_id: item.owner_id, location_id: item.location_id, flag_id: item.flag_id, quantity: item.quantity, singleton: item.singleton }
    }
}

fn arguments(call: &Call<'_>) -> Result<(i64, i64), CallError> {
    if !matches!(call.kwargs, Value::None) && !matches!(call.kwargs, Value::Dict(v) if v.is_empty()) {
        return Err(notify_error(msg::COMPRESSION_ARGUMENT));
    }
    match call.args {
        [Value::Int(source), Value::Int(facility)] if *source > 0 && *facility > 0 => Ok((*source, *facility)),
        _ => Err(notify_error(msg::COMPRESSION_ARGUMENT)),
    }
}

fn eligible(context: &Context, item: &Item) -> bool {
    (item.owner_id == context.character || item.flag_id == FLAG_FLEET_HANGAR) && item.location_id == context.ship && item.quantity > 0 && !item.singleton
        && [FLAG_CARGO, FLAG_FLEET_HANGAR, FLAG_GENERAL_MINING_HOLD, FLAG_SPECIAL_ASTEROID_HOLD, FLAG_SPECIALIZED_ICE_HOLD, FLAG_SPECIALIZED_GAS_HOLD].contains(&item.flag_id)
}

fn output_type(sde: &Sde, source: i32) -> Result<i32, CallError> {
    sde.compression.get(source).filter(|&output| output > 0 && output != source && sde.types.get(output).is_some())
        .ok_or_else(|| notify_error(msg::COMPRESSION_TYPE))
}

async fn item(ex: &(impl Executor + ?Sized), sde: &Sde, id: i64) -> Result<Option<Item>, CallError> {
    Ok(load_items(ex, sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next())
}

async fn compress_tx(
    ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox,
    context: &Context, expected_source: SourceGuard,
    locks: &crate::dao::items::parent_locks::LockedItems<'_>,
) -> Result<(Value, Item, crate::services::items::DepositReceipt), CallError> {
    if context.character <= 0 || context.ship <= 0 || context.system <= 0 { return Err(notify_error(msg::COMPRESSION_CONTEXT)); }
    let output_type = output_type(sde, expected_source.type_id)?;
    let source = item(ex, sde, expected_source.item_id).await?.ok_or_else(|| notify_error(msg::COMPRESSION_CHANGED))?;
    if SourceGuard::from(&source) != expected_source || !eligible(context, &source) { return Err(notify_error(msg::COMPRESSION_CHANGED)); }
    let ship = item(ex, sde, context.ship).await?.ok_or_else(|| notify_error(msg::COMPRESSION_CONTEXT))?;
    if ship.owner_id != context.character || ship.location_id != context.system || !ship.singleton || ship.quantity != 1 || ship.category_id != i64::from(CATEGORY_SHIP) {
        return Err(notify_error(msg::COMPRESSION_CONTEXT));
    }
    if !dao::trades::reserved(ex, &[source.item_id, context.ship]).await.map_err(db_err)?.is_empty() { return Err(notify_error(msg::COMPRESSION_RESERVED)); }
    let mut changes = BTreeMap::new();
    changes.insert(source.item_id, QuantityChange::new(&source, None));
    if !quantities::apply(ex, &changes).await.map_err(db_err)? { return Err(notify_error(msg::COMPRESSION_CHANGED)); }
    let deposited = transfers::deposit_stack(
        ex, context.ship, source.flag_id, output_type, source.owner_id, source.quantity,
        transfers::DepositCandidate::InStatement, Some(locks),
    ).await.map_err(db_err)?;
    let receipt = crate::services::items::enrich_deposit(sde, deposited, source.quantity)?;
    let output_id = receipt.last().item_id;
    let display_quantity = crate::services::dogma::info::cap_client_quantity(source.quantity);
    let result = Value::tuple([
        Value::Int(source.item_id), Value::Int(i64::from(source.type_id)), Value::Int(display_quantity),
        Value::Int(output_id), Value::Int(i64::from(output_type)), Value::Int(display_quantity),
    ]);
    inventory::notifications::notify_item_removed(outbox, source.clone());
    for row in receipt.rows() {
        let change = row.old_quantity.map_or((IX_LOCATION_ID, Value::Int(0)), |old| (IX_STACK_SIZE, Value::Int(old)));
        outbox.notify_client("OnItemChange", vec![row.item.row(), inventory::notifications::item_change_dict([change]), Value::None]);
    }
    Ok((result, source, receipt))
}

async fn compress(c: &mut Client, source_id: i64, facility: i64) -> CallResult {
    let context = Context { character: c.session.int("charid").unwrap_or(0), ship: c.session.int("shipid").unwrap_or(0), system: c.session.int("solarsystemid").unwrap_or(0) };
    if context.character <= 0 || context.ship <= 0 || context.system <= 0 || c.session.int("stationid").is_some() { return Err(notify_error(msg::COMPRESSION_CONTEXT)); }
    let _lock = market::lock().await;
    let source = item(&c.db, &c.sde, source_id).await?.ok_or_else(|| notify_error(msg::COMPRESSION_ITEM))?;
    if !eligible(&context, &source) { return Err(notify_error(msg::COMPRESSION_ITEM)); }
    output_type(&c.sde, source.type_id)?;
    if !dao::trades::reserved(&c.db, &[source_id]).await.map_err(db_err)?.is_empty() { return Err(notify_error(msg::COMPRESSION_RESERVED)); }
    let actor_context = context.clone();
    let source_type = source.type_id;
    let sde = c.sde.clone();
    let admitted = c.world.with(context.system, move |s| {
        let Some(pilot) = s.pilot_ball(actor_context.character).filter(|ball| ball.id == actor_context.ship) else { return false };
        let Some(target) = s.ball(facility) else { return false };
        if facility != actor_context.ship && !target.char_id.is_some_and(|character| s.pilot_ball(character).is_some_and(|ball| ball.id == facility) && fleets::same_fleet(actor_context.character, character)) { return false; }
        let Some(range) = s.compressor_range(facility, source_type, &sde) else { return false };
        let Some((a, b)) = s.sample_position(pilot.id).zip(s.sample_position(target.id)) else { return false };
        let gap = (dist(a, b) - f64::from(pilot.radius) - f64::from(target.radius)).max(0.0);
        gap.is_finite() && gap <= range
    }).await.unwrap_or(false);
    if !admitted { return Err(notify_error(msg::COMPRESSION_FACILITY)); }
    let db = c.db.clone();
    let sde = c.sde.clone();
    let identity = crate::services::identity::Identity { system_id: Some(context.system), ..crate::services::identity::Identity::of_client(c) };
    let endpoint = inventory::reachability::Endpoint { location: source.location_id, flag: source.flag_id };
    let reach = inventory::reachability::Reachability::capture(&identity, &db, &c.world, Some(context.system), &[endpoint]).await?;
    let audience = inventory::notifications::Audience::capture(&identity, &db, &c.world, Some(context.system), &[endpoint]).await?;
    let expected_source = SourceGuard::from(&source);
    db.transaction(move |tx, outbox: &mut Outbox| {
        let sde = sde.clone();
        let context = context.clone();
        let reach = reach.clone();
        let audience = audience.clone();
        let identity = identity.clone();
        let source = source.clone();
        Box::pin(async move {
            let locks = inventory::hold_storage::check_destination(tx, &sde, context.character, context.ship, expected_source.flag_id,
                &[inventory::hold_storage::Addition { type_id: output_type(&sde, expected_source.type_id)?, singleton: false, quantity: expected_source.quantity }],
                inventory::hold_storage::Replaced { items: &[expected_source.item_id], reloads: &[] }, None).await?
                .ok_or_else(|| notify_error(msg::COMPRESSION_ITEM))?;
            let container = inventory::access::Container::Item(context.ship);
            let (destination, _) = inventory::movement::authorize_move(tx, &sde, &identity, &reach, &source, Some(source.location_id), &container, Some(source.flag_id)).await?
                .ok_or_else(|| notify_error(msg::COMPRESSION_ITEM))?;
            if destination.item_owner != source.owner_id || destination.capacity_owner != context.character {
                return Err(notify_error(msg::COMPRESSION_ITEM));
            }
            let viewers = audience.resolve(tx, &sde).await?;
            let (result, source, receipt) = compress_tx(tx, &sde, outbox, &context, expected_source, &locks).await?;
            let gone = Item { location_id: LOCATION_JUNKYARD, ..source.clone() };
            viewers.item_change(outbox, &source, &gone, vec![(IX_LOCATION_ID, Value::Int(source.location_id))], context.character);
            for row in receipt.rows() {
                let previous = row.old_quantity.map_or_else(
                    || Item { location_id: LOCATION_JUNKYARD, ..row.item.clone() },
                    |old| Item { quantity: old, ..row.item.clone() },
                );
                let change = row.old_quantity.map_or((IX_LOCATION_ID, Value::Int(0)), |old| (IX_STACK_SIZE, Value::Int(old)));
                viewers.item_change(outbox, &previous, &row.item, vec![change], context.character);
            }
            Ok(result)
        })
    }, |value, outbox| async move { dispatch_client(value, outbox, c) }).await
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "CompressItemInSpace" => Some(match arguments(call) { Ok((source, facility)) => compress(c, source, facility).await, Err(error) => Err(error) }),
        "SetAutoCompressionEnabled" => Some(Err(notify_error(msg::COMPRESSION_AUTO))),
        _ => None,
    }
}

#[cfg(test)] mod tests;
