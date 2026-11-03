//! LPStoreMgr mirrors `eve/client/script/ui/station/loyaltyPointStore/lpStoreSvc.py`.
//! Imported `game.lp_offers` are authoritative; a globally empty catalogue uses
//! retail-derived faction offers. Purchases use `game.lp_balances`, wallets and the
//! character's station hangar even when a corporation pays. Analysis Kredits cannot
//! be spent, so offers requiring them remain visible but cannot be purchased.
use super::change_lp;
use crate::services::{db_err, keyval, notify_error, Call, CallError, CallResult};
use crate::client::Client;
use crate::dao::lp_offers::Offer;
use crate::dao::items::{loading::Item, quantities::{self, QuantityChange}};
use crate::db::outbox::dispatch_client;
use crate::messages;
use crate::consts::IX_STACK_SIZE;
use marshal::Value;
use std::collections::BTreeMap;
use tracing::info;

/// Server fallback policy: retail ISK value per LP.
const ISK_VALUE_PER_LP: f64 = 2000.0;
/// Server fallback policy: ISK charged per LP.
const ISK_PER_LP: i64 = 1000;
/// SDE metaGroups: faction.
const META_GROUP_FACTION: f64 = 4.0;
/// Server fallback policy: LP price increment.
const LP_PRICE_INCREMENT: i64 = 25;
/// Server fallback policy: minimum LP price.
const MIN_LP_PRICE: i64 = 100;
/// Server policy: maximum quantity per purchase.
const MAX_OFFER_COUNT: i64 = 1000;
/// Largest integer ISK amount represented exactly by the floating-point wallet.
const MAX_EXACT_WALLET_ISK: i64 = 1_i64 << 53;

async fn fallback(ex: &(impl crate::db::executor::Executor + ?Sized), sde: &crate::sde::Sde, corp: i64) -> Result<Vec<Offer>, CallError> {
    let Some(race) = sde.npc_corporations.get(corp).and_then(|c| c.race_id) else { return Ok(Vec::new()) };
    let prices = crate::dao::store::catalogue::prices(ex).await.map_err(db_err)?;
    let mut offers = Vec::new();
    for t in sde.types.iter() {
        if t.race_id.map(i64::from) != Some(race) || t.published != Some(true) || t.market_group_id.is_none() { continue; }
        let Some(average) = prices.get(&t.type_id).copied().filter(|p| crate::dao::store::catalogue::positive_price(*p)) else { continue };
        for attr in sde.dogma.attributes_by_name("metaGroupID") {
            if t.attribute(attr.attribute_id) != Some(META_GROUP_FACTION) { continue; }
            let units = ((average / ISK_VALUE_PER_LP) / LP_PRICE_INCREMENT as f64).round() as i64;
            let Some(lp) = units.checked_mul(LP_PRICE_INCREMENT).map(|lp| lp.max(MIN_LP_PRICE)) else { continue };
            let Some(isk) = lp.checked_mul(ISK_PER_LP) else { continue };
            offers.push(Offer { corporation_id: corp, offer_id: i64::from(t.type_id), type_id: t.type_id, quantity: 1, lp_cost: lp, isk_cost: isk, ak_cost: 0, required_items: Vec::new() });
        }
    }
    Ok(offers)
}

async fn offers(c: &Client, corp: i64) -> Result<Vec<Offer>, CallError> {
    if c.sde.npc_corporations.get(corp).is_none() { return Ok(Vec::new()); }
    if crate::dao::lp_offers::is_empty(&c.db).await.map_err(db_err)? { fallback(&c.db, &c.sde, corp).await }
    else { crate::dao::lp_offers::list(&c.db, corp).await.map_err(db_err) }
}

fn wire(offer: Offer) -> Value {
    keyval([
        ("offerID", Value::Int(offer.offer_id)), ("corpID", Value::Int(offer.corporation_id)),
        ("typeID", Value::Int(i64::from(offer.type_id))), ("qty", Value::Int(i64::from(offer.quantity))),
        ("lpCost", Value::Int(offer.lp_cost)), ("iskCost", Value::Int(offer.isk_cost)),
        ("akCost", Value::Int(offer.ak_cost)),
        ("reqItems", Value::list(offer.required_items.into_iter().map(|r| Value::tuple([Value::Int(i64::from(r.type_id)), Value::Int(i64::from(r.quantity))])))),
        ("requiredStandings", Value::None),
    ])
}

struct Cost { lp: i64, isk: i64, output: i32, items: BTreeMap<i32, i64> }
fn scale(offer: &Offer, count: i64) -> Option<Cost> {
    if !(1..=MAX_OFFER_COUNT).contains(&count) || offer.quantity <= 0 || offer.type_id <= 0 || offer.lp_cost < 0 || offer.isk_cost < 0 || offer.ak_cost < 0 { return None; }
    offer.ak_cost.checked_mul(count)?;
    let output = i32::try_from(i64::from(offer.quantity).checked_mul(count)?).ok()?;
    let mut items = BTreeMap::<i32, i64>::new();
    for r in &offer.required_items {
        if r.type_id <= 0 || r.quantity <= 0 { return None; }
        let n = i64::from(r.quantity).checked_mul(count)?;
        let total = items.entry(r.type_id).or_default();
        *total = total.checked_add(n)?;
    }
    let isk = offer.isk_cost.checked_mul(count)?;
    if isk > MAX_EXACT_WALLET_ISK { return None; }
    Some(Cost { lp: offer.lp_cost.checked_mul(count)?, isk, output, items })
}

fn plan(items: &[Item], required: &BTreeMap<i32, i64>) -> Option<BTreeMap<i64, QuantityChange>> {
    let mut left = required.clone();
    let mut changes = BTreeMap::new();
    for item in items {
        let Some(remaining) = left.get_mut(&item.type_id) else { continue };
        let take = (*remaining).min(i64::from(item.quantity));
        if take <= 0 { continue; }
        *remaining -= take;
        let quantity = item.quantity - take as i32;
        changes.insert(item.item_id, QuantityChange::new(item, (quantity > 0).then_some(quantity)));
    }
    left.values().all(|&n| n == 0).then_some(changes)
}

async fn take_offer(c: &mut Client, corp: i64, offer_id: i64, count: i64, corporation: bool) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let Some(station) = c.session.int("stationid").filter(|&id| c.sde.map.station(id).is_some()) else { return Err(notify_error(messages::STATION_DOCK_REQUIRED_FOR_LP_STORE)); };
    if c.sde.npc_corporations.get(corp).is_none() { return Err(notify_error(messages::LP_OFFER_UNAVAILABLE)); }
    let (owner, key) = if corporation {
        let (owner, _) = super::super::corps::membership(&c.db, me).await?;
        let key = c.session.int("corpAccountKey").and_then(|n| i32::try_from(n).ok()).filter(|&n| super::super::corpwallet::valid_key(n));
        let Some(key) = key else { return Err(notify_error(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED)); };
        if !super::super::corps::is_player_corp(owner) || !super::super::corpwallet::may_take(&c.db, me, owner, key).await? { return Err(notify_error(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED)); }
        (owner, Some(key))
    } else { (me, None) };
    let (db, sde) = (c.db.clone(), c.sde.clone());
    let _trading = crate::services::market::lock().await;
    db.transaction(|tx, outbox| {
        let sde = sde.clone();
        Box::pin(async move {
            let offer = if crate::dao::lp_offers::is_empty(tx).await.map_err(db_err)? {
                fallback(tx, &sde, corp).await?.into_iter().find(|o| o.offer_id == offer_id)
            } else {
                crate::dao::lp_offers::get(tx, corp, offer_id).await.map_err(db_err)?
            };
            let Some(offer) = offer else { return Err(notify_error(messages::LP_OFFER_UNAVAILABLE)); };
            let Some(cost) = scale(&offer, count) else { return Err(notify_error(messages::LP_OFFER_UNAVAILABLE)); };
            if offer.ak_cost != 0 { return Err(notify_error(messages::loyalty::ANALYSIS_KREDITS_UNAVAILABLE)); }
            let types: Vec<i32> = cost.items.keys().copied().collect();
            let items = crate::dao::items::loyalty::requirement_stacks(tx, &sde, me, station, &types).await.map_err(db_err)?;
            let Some(changes) = plan(&items, &cost.items) else { return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY)); };
            if !quantities::apply(tx, &changes).await.map_err(db_err)? { return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY)); }
            for item in items {
                if let Some(change) = changes.get(&item.item_id) {
                    if change.quantity.is_none() { super::super::inventory::notifications::notify_item_removed(outbox, item); }
                    else { super::super::inventory::notifications::notify_item_change(tx, &sde, outbox, item.item_id, vec![(IX_STACK_SIZE, Value::Int(i64::from(item.quantity)))]).await?; }
                }
            }
            if let Some(key) = key { super::super::wallets::corp_debit(tx, outbox, owner, key, cost.isk as f64).await?; }
            else { super::super::wallets::debit(tx, outbox, owner, cost.isk as f64).await?; }
            change_lp(tx, outbox, owner, corp, -cost.lp, corporation).await?;
            crate::services::items::deliver(tx, &sde, outbox, me, station, offer.type_id, cost.output).await?;
            Ok(())
        })
    }, |value, outbox| async move { dispatch_client(value, outbox, c) }).await?;
    info!("lp store: {me} bought {count} offers from {corp}");
    Ok(Value::Bool(true))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    Some(match call.method {
        "GetAvailableOffersFromCorp" => offers(c, int(0)).await.map(|list| Value::list(list.into_iter().map(wire))),
        "TakeOfferForCharacter" => take_offer(c, int(0), int(1), call.args.get(2).and_then(|v| v.as_int()).unwrap_or(1), false).await,
        "TakeOfferForCorporation" => take_offer(c, int(0), int(1), call.args.get(2).and_then(|v| v.as_int()).unwrap_or(1), true).await,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
