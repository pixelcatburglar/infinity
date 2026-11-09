//! PLEX conversion between an account vault and a personal NPC-station hangar.
//! Mirrors invCache and the PLEX vault listeners; uses game.items and the vault ledger.
//! Structures, corporate custody and direct-trade withdrawals are not supported.
use std::collections::BTreeMap;
use marshal::Value;
use infinity_core::{consts::{FLAG_HANGAR, IX_LOCATION_ID, IX_STACK_SIZE, TYPE_PLEX}, messages};
use crate::{client::Client, dao::{accounts::plex, characters::persistence, items::{loading::ItemQuery, quantities::{self, QuantityChange}, transfers}, trades}, db::outbox::Outbox, services::{db_err, items::load_items, notify_error, plex_vault::{self, VaultAudience, VaultChange, VaultContext, VaultKind}, Call, CallError, CallResult}};
use super::notifications::{notify_item_change, notify_item_removed};

fn invalid() -> CallError { notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION) }
fn integer(value: Option<&Value>) -> Option<i64> {
    match value { Some(Value::Int(n)) => Some(*n), _ => None }
}
fn quantity(value: Option<&Value>) -> Option<i64> {
    integer(value).filter(|n| *n > 0)
}

pub(super) async fn convert(c: &mut Client, call: &Call<'_>, withdrawal: bool) -> CallResult {
    let args = call.args;
    let (location, amount, item) = if withdrawal {
        if args.len() != 4 || !args.get(2).and_then(Value::as_text).is_some_and(|v| v == "VaultToMarket") || integer(args.get(3)) != Some(i64::from(FLAG_HANGAR)) { return Err(invalid()); }
        (integer(args.get(1)), quantity(args.first()), None)
    } else {
        if args.len() != 4 || !args.get(3).and_then(Value::as_text).is_some_and(|v| v == "InventoryToVault") { return Err(invalid()); }
        (integer(args.first()), quantity(args.get(2)), integer(args.get(1)))
    };
    let location = location.ok_or_else(invalid)?;
    let amount = amount.ok_or_else(|| notify_error(messages::INSUFFICIENT_ITEM_QUANTITY))?;
    if !withdrawal && item.is_none_or(|id| id <= 0) { return Err(invalid()); }
    let character = c.session.int("charid").filter(|id| *id > 0).ok_or_else(invalid)?;
    let user = c.login.user_id;
    let audience = if c.world.is_online(character) { VaultAudience::Account } else { VaultAudience::UnpublishedCaller { character_id: Some(character) } };
    if user <= 0 || c.session.int("stationid") != Some(location) || c.sde.map.station(location).is_none() { return Err(invalid()); }
    let _lock = super::super::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    let sender = c.notify_tx.clone();
    db.transaction(move |tx, outbox: &mut Outbox| {
        let sde = sde.clone();
        let sender = sender.clone();
        Box::pin(async move {
            if !persistence::owned(tx, user, character).await.map_err(db_err)? || transfers::docked(tx, character).await.map_err(db_err)? != Some(location) { return Err(invalid()); }
            let reference = plex::reference(tx).await.map_err(db_err)?;
            let item_id = if withdrawal {
                let item_id = transfers::insert(tx, TYPE_PLEX, character, location, FLAG_HANGAR, amount).await.map_err(db_err)?;
                if plex_vault::change_in(tx, outbox, user, Some(character), VaultChange::Add(-i128::from(amount)), VaultContext { kind: VaultKind::MarketWithdrawal, item_id: Some(item_id), reference: Some(reference) }, audience).await?.is_none() { return Err(notify_error(messages::plex_vault::INSUFFICIENT_BALANCE)); }
                notify_item_change(tx, &sde, outbox, item_id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                item_id
            } else {
                let item_id = item.ok_or_else(invalid)?;
                if !trades::reserved(tx, &[item_id]).await.map_err(db_err)?.is_empty() { return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED)); }
                let row = load_items(tx, &sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(invalid)?;
                if row.type_id != TYPE_PLEX || row.owner_id != character || row.location_id != location || row.flag_id != FLAG_HANGAR || row.singleton { return Err(invalid()); }
                if amount > row.quantity { return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY)); }
                let remaining = row.quantity - amount;
                let changes = BTreeMap::from([(item_id, QuantityChange::new(&row, (remaining > 0).then_some(remaining)))]);
                if !quantities::apply(tx, &changes).await.map_err(db_err)? { return Err(invalid()); }
                if remaining == 0 { notify_item_removed(outbox, row); } else { notify_item_change(tx, &sde, outbox, item_id, vec![(IX_STACK_SIZE, Value::Int(row.quantity))]).await?; }
                if plex_vault::change_in(tx, outbox, user, Some(character), VaultChange::Add(i128::from(amount)), VaultContext { kind: VaultKind::InventoryDeposit, item_id: Some(item_id), reference: Some(reference) }, audience).await?.is_none() { return Err(notify_error(messages::plex_vault::BALANCE_LIMIT_EXCEEDED)); }
                item_id
            };
            if withdrawal { outbox.notify_sender(sender, "OnPlexItemAddedToInventory", vec![Value::Int(item_id), Value::Int(reference)]); }
            else { outbox.notify_sender(sender, "OnPlexDeposited", vec![Value::Int(reference)]); }
            Ok(Value::Int(reference))
        })
    }, |value, outbox| async move { crate::db::outbox::dispatch_client(value, outbox, c) }).await
}

#[cfg(test)] mod tests;
