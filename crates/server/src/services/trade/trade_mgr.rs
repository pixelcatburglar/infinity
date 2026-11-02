//! `trademgr` initiation and bound trade-session calls, mirroring
//! `pvptradesvc.py`, `pvptradewnd.py` and the PlayerTrade inventory controller.
//! Uses `game.trades`, `game.trade_items`, character wallets and station hangars.
//! Fitted items, corporation hangars and PLEX vault transfers are unsupported.
use std::collections::{BTreeMap, BTreeSet};
use marshal::Value;
use crate::{client::Client, consts::{FLAG_HANGAR, IX_LOCATION_ID, IX_STACK_SIZE}, dao::{items::{loading::{Item, ItemQuery}, quantities::{self, QuantityChange}, transfers}, trades::{self, Trade, TradeItem}}, db::{executor::Executor, outbox::Outbox}, messages, sde::Sde, services::{db_err, items::load_items, keyval, market, notify_error, user_error, wallets, Call, CallError, CallResult}};
use super::{dispatch as dispatch_outbox, session, settle, timeout, TradeEffect};

/// `eveexceptions.const.UE_TYPEIDL`: list of type IDs in a UserError argument.
const UE_TYPEIDL: i64 = 29;

fn moniker(node: i64, id: i64) -> Value {
    Value::Instance(Box::new(Value::str("util.Moniker")), Box::new(Value::tuple([Value::str("trademgr"), Value::Int(node), Value::Int(id), Value::None])))
}
fn pair(trade: &Trade, caller: i64, target: i64) -> bool { (trade.a == caller && trade.b == target) || (trade.b == caller && trade.a == target) }
fn acceptance(trade: &Trade) -> Value { Value::list([Value::Bool(trade.accepted_a), Value::Bool(trade.accepted_b)]) }
fn amounts(trade: &Trade) -> Value { Value::list([Value::Float(trade.money_a), Value::Float(trade.money_b)]) }
pub(super) fn reset(outbox: &mut Outbox<TradeEffect>, trade: &mut Trade) {
    trade.accepted_a = false;
    trade.accepted_b = false;
    settle::tell(outbox, trade, "OnTradeOffer", vec![Value::Int(trade.id), acceptance(trade)]);
}
fn item_notice(outbox: &mut Outbox<TradeEffect>, trade: &Trade, item: &Item, changes: Vec<(i64, Value)>) {
    settle::tell(outbox, trade, "OnItemChange", vec![item.row(), Value::Dict(changes.into_iter().map(|(i, v)| (Value::Int(i), v)).collect()), Value::None]);
}

async fn initiate(c: &Client, target: i64) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    if me == target { return Err(notify_error(messages::trade::TRADE_SELF)); }
    let station = c.session.int("stationid").ok_or_else(|| notify_error(messages::trade::TRADE_NOT_DOCKED))?;
    if !c.world.is_online(target) { return Err(notify_error(messages::trade::TRADE_TARGET_NOT_HERE)); }
    let _lock = market::lock().await;
    if !c.world.is_online(me) || !c.world.is_online(target) { return Err(notify_error(messages::trade::TRADE_TARGET_NOT_HERE)); }
    let node = c.cfg.server.node_id;
    let world = &*c.world;
    c.db.transaction(|tx, outbox: &mut Outbox<TradeEffect>| Box::pin(async move {
        if transfers::docked(tx, me).await.map_err(db_err)? != Some(station) || transfers::docked(tx, target).await.map_err(db_err)? != Some(station) {
            return Err(notify_error(messages::trade::TRADE_TARGET_NOT_HERE));
        }
        if let Some(existing) = trades::open_for(tx, me).await.map_err(db_err)? {
            return if pair(&existing, me, target) { Ok(moniker(node, existing.id)) } else { Err(notify_error(messages::trade::TRADE_BUSY)) };
        }
        if trades::open_for(tx, target).await.map_err(db_err)?.is_some() { return Err(notify_error(messages::trade::TRADE_BUSY)); }
        let trade = trades::create(tx, station, me, target, crate::client::filetime_now()).await.map_err(db_err)?;
        timeout::refresh(tx, outbox, &trade).await?;
        let result = moniker(node, trade.id);
        outbox.operation(TradeEffect::Put(trade));
        outbox.notify_pilot(target, "OnTradeInitiate", vec![Value::Int(me), result.clone()]);
        Ok(result)
    }), |value, outbox| async move { dispatch_outbox(value, outbox, world) }).await
}

async fn stage(ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<TradeEffect>, trade: &mut Trade, me: i64, ship: Option<i64>, ids: &[i64], source: i64, quantity: Option<i64>) -> Result<(), CallError> {
    if source != trade.station { return Err(notify_error(messages::trade::TRADE_SOURCE_NOT_HANGAR)); }
    let unique: BTreeSet<_> = ids.iter().copied().collect();
    if unique.len() != ids.len() || ids.is_empty() { return Err(notify_error(messages::trade::TRADE_SOURCE_NOT_HANGAR)); }
    let items = load_items(ex, sde, ItemQuery::Ids(ids)).await.map_err(db_err)?;
    if items.len() != ids.len() { return Err(notify_error(messages::trade::TRADE_SOURCE_NOT_HANGAR)); }
    for item in &items {
        if item.owner_id != me || item.location_id != source || item.flag_id != FLAG_HANGAR { return Err(notify_error(messages::trade::TRADE_SOURCE_NOT_HANGAR)); }
        if Some(item.item_id) == ship { return Err(user_error(messages::trade::MSG_PEOPLE_ABOARD_SHIP, vec![])); }
        if let Some(q) = quantity { if q < 1 || q > crate::services::item_volume::stack_size(item.singleton, i64::from(item.quantity)) || (item.singleton && q != 1) { return Err(notify_error(messages::trade::TRADE_SOURCE_NOT_HANGAR)); } }
    }
    let tree_ids = trades::descendants(ex, ids).await.map_err(db_err)?;
    let tree = load_items(ex, sde, ItemQuery::Ids(&tree_ids)).await.map_err(db_err)?;
    if tree.len() != tree_ids.len() { return Err(notify_error(messages::trade::TRADE_SOURCE_NOT_HANGAR)); }
    if tree.iter().any(|item| item.owner_id != me) { return Err(notify_error(messages::trade::TRADE_SOURCE_NOT_HANGAR)); }
    let invalid: BTreeSet<_> = tree.iter().filter(|item| !sde.is_tradable(item.type_id)).map(|item| i64::from(item.type_id)).collect();
    if !invalid.is_empty() {
        return Err(user_error(messages::trade::MSG_CANNOT_TRADE_NON_TRADABLE, vec![("itemList".into(), Value::tuple([Value::Int(UE_TYPEIDL), Value::list(invalid.into_iter().map(Value::Int))]))]));
    }
    if crate::dao::industry::has_active_blueprint_job(ex, &tree_ids).await.map_err(db_err)? {
        return Err(notify_error(messages::industry::BLUEPRINT_ACTIVE));
    }
    let mut staged = Vec::new();
    for item in &items {
        let q = quantity.unwrap_or(crate::services::item_volume::stack_size(item.singleton, i64::from(item.quantity)));
        let id = if !item.singleton && q < i64::from(item.quantity) {
            let mut changes = BTreeMap::new();
            changes.insert(item.item_id, QuantityChange::new(item, Some(item.quantity - q as i32)));
            if !quantities::apply(ex, &changes).await.map_err(db_err)? { return Err(notify_error(messages::STACK_CANNOT_BE_SPLIT)); }
            let id = transfers::insert(ex, item.type_id, me, trade.id, item.flag_id, q as i32).await.map_err(db_err)?;
            if let Some(left) = load_items(ex, sde, ItemQuery::Id(item.item_id)).await.map_err(db_err)?.into_iter().next() { item_notice(outbox, trade, &left, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]); }
            id
        } else {
            if !trades::relocate(ex, item.item_id, me, source, trade.id, item.flag_id).await.map_err(db_err)? { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
            item.item_id
        };
        trades::add_item(ex, trade.id, &TradeItem { id, owner: me, origin: source, flag: item.flag_id }).await.map_err(db_err)?;
        if let Some(moved) = load_items(ex, sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next() { item_notice(outbox, trade, &moved, vec![(IX_LOCATION_ID, Value::Int(source))]); }
        staged.push(id);
    }
    let contents = trades::descendants(ex, &staged).await.map_err(db_err)?;
    let other = if me == trade.a { trade.b } else { trade.a };
    outbox.notify_pilot(other, "OnPrimingNeededForTradeItems", vec![Value::list(contents.into_iter().map(Value::Int))]);
    reset(outbox, trade);
    Ok(())
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> CallResult {
    if !call.bound {
        return if call.method == "InitiateTrade" { initiate(c, call.args.first().and_then(Value::as_int).unwrap_or(0)).await } else { Ok(Value::None) };
    }
    let id = call.bind_params.and_then(Value::as_int).unwrap_or(0);
    let me = c.session.int("charid").unwrap_or(0);
    let ship = c.session.int("shipid");
    let method = call.method.to_owned();
    let args = call.args.to_vec();
    let kwargs = call.kwargs.clone();
    let sde = c.sde.clone();
    let world = c.world.clone();
    let _lock = market::lock().await;
    c.db.transaction(move |tx, outbox: &mut Outbox<TradeEffect>| {
        let (method, args, kwargs, sde) = (method.clone(), args.clone(), kwargs.clone(), sde.clone());
        Box::pin(async move {
            let Some(mut trade) = trades::get(tx, id).await.map_err(db_err)? else {
                return if method == "Abort" { Ok(Value::None) } else { Err(notify_error(messages::trade::TRADE_BUSY)) };
            };
            if trade.state != "open" { return if method == "Abort" { Ok(Value::None) } else { Err(notify_error(messages::trade::TRADE_BUSY)) }; }
            if me != trade.a && me != trade.b { return Err(notify_error(messages::trade::TRADE_BUSY)); }
            if transfers::docked(tx, trade.a).await.map_err(db_err)? != Some(trade.station) || transfers::docked(tx, trade.b).await.map_err(db_err)? != Some(trade.station) {
                settle::abort(tx, &sde, outbox, &mut trade, true).await?;
                return Ok(Value::None);
            }
            let mut changed = false;
            let value = match method.as_str() {
                "List" => {
                    let rows = load_items(tx, &sde, ItemQuery::Location(id)).await.map_err(db_err)?;
                    keyval([("tradeContainerID", Value::Int(id)), ("traders", Value::list([Value::Int(trade.a), Value::Int(trade.b)])), ("items", Value::list(rows.iter().map(Item::row)))])
                }
                "GetItem" | "GetSelfInvItem" => {
                    let celestial = sde.map.celestial(trade.station).ok_or_else(|| notify_error(messages::trade::TRADE_NOT_DOCKED))?;
                    let type_id = celestial.type_id.and_then(|id| i32::try_from(id).ok()).ok_or_else(|| notify_error(messages::trade::TRADE_NOT_DOCKED))?;
                    let category = sde.types.get(type_id).and_then(|data| data.category_id).unwrap_or(0);
                    Item { item_id: id, type_id, owner_id: me, location_id: trade.station, flag_id: FLAG_HANGAR, quantity: 1, singleton: true, group_id: celestial.group_id.unwrap_or(0), category_id: i64::from(category), custom_info: None }.row()
                }
                "Add" | "MultiAdd" => {
                    let ids = if method == "Add" { vec![args.first().and_then(Value::as_int).ok_or_else(|| notify_error(messages::trade::TRADE_SOURCE_NOT_HANGAR))?] } else {
                        args.first().and_then(Value::items).ok_or_else(|| notify_error(messages::trade::TRADE_SOURCE_NOT_HANGAR))?.iter().map(Value::as_int).collect::<Option<Vec<_>>>().ok_or_else(|| notify_error(messages::trade::TRADE_SOURCE_NOT_HANGAR))?
                    };
                    let source = args.get(1).and_then(Value::as_int).ok_or_else(|| notify_error(messages::trade::TRADE_SOURCE_NOT_HANGAR))?;
                    let qty = match kwargs.get("qty") { Some(Value::None) | None => None, Some(v) => Some(v.as_int().ok_or_else(|| notify_error(messages::trade::TRADE_SOURCE_NOT_HANGAR))?) };
                    stage(tx, &sde, outbox, &mut trade, me, ship, &ids, source, qty).await?;
                    changed = true;
                    Value::Bool(true)
                }
                "OfferMoney" => {
                    let amount = args.first().and_then(session::money).ok_or_else(|| notify_error(messages::trade::TRADE_SOURCE_NOT_HANGAR))?;
                    if !crate::dao::wallets::characters::covers(tx, me, amount).await.map_err(db_err)? { return Err(wallets::not_enough_money(wallets::balance(tx, me).await?, amount)); }
                    if me == trade.a { trade.money_a = amount; } else { trade.money_b = amount; }
                    reset(outbox, &mut trade);
                    settle::tell(outbox, &trade, "OnTradeMoneyOffer", vec![Value::Int(id), amounts(&trade)]);
                    changed = true;
                    Value::None
                }
                "MakeOffer" => {
                    let roots = trades::items(tx, id).await.map_err(db_err)?;
                    if !args.first().is_some_and(|value| session::matches_manifest(value, &trade, &roots)) {
                        reset(outbox, &mut trade);
                        outbox.notify_pilot(me, "OnTradeOfferReset", vec![Value::Int(id), Value::Bool(true)]);
                        changed = true;
                        Value::Bool(false)
                    } else {
                        if me == trade.a { trade.accepted_a = true; } else { trade.accepted_b = true; }
                        if trade.accepted_a && trade.accepted_b {
                            return Ok(Value::Bool(matches!(settle::complete(tx, &sde, outbox, &mut trade).await?, settle::Settled::Done)));
                        } else {
                            settle::tell(outbox, &trade, "OnTradeOffer", vec![Value::Int(id), acceptance(&trade)]);
                            changed = true;
                        }
                        Value::Bool(true)
                    }
                }
                "Abort" => { settle::abort(tx, &sde, outbox, &mut trade, true).await?; Value::None }
                "MultiMerge" | "StackAll" => Value::None,
                _ => Value::None,
            };
            if changed {
                trade.updated_at = crate::client::filetime_now();
                if !trades::update(tx, &trade, trade.updated_at).await.map_err(db_err)? { return Err(notify_error(messages::trade::TRADE_BUSY)); }
                timeout::refresh(tx, outbox, &trade).await?;
                outbox.operation(TradeEffect::Put(trade));
            }
            Ok(value)
        })
    }, |value, outbox| async move { dispatch_outbox(value, outbox, &world) }).await
}
