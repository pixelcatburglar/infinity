//! `dynamicItemService` information and creation RPCs used by
//! `eve/client/script/dogma/dynamicItemSvc.py`. Reads game provenance, typed factor
//! rows from `game.dynamic_item_modifiers`, and item overrides; uses SDE ranges for
//! creation only. Renewal, erase and roguelike variants are unsupported; deployed
//! inputs require World removal and are refused.
use infinity_core::{consts::dogma::ATTR_HULL_DAMAGE, messages::dynamic_items as msg};
use crate::{client::Client, dao::{dynamic_items, items::{dogma, loading::ItemQuery}}, db::{executor::Executor, outbox::{dispatch_client, Outbox}}, sde::Sde, services::{db_err, items::load_items, keyval, market, notify_error, rand01, Call, CallError, CallResult}};
use marshal::Value;
use super::mutation::{self, Context};

pub(super) async fn info(ex: &(impl Executor + ?Sized), sde: &Sde, item_id: i64) -> CallResult {
    let metadata = dynamic_items::get(ex, item_id).await.map_err(db_err)?.ok_or_else(|| notify_error(msg::NOT_FOUND))?;
    let item = load_items(ex, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(msg::NOT_FOUND))?;
    if item.type_id != metadata.result_type_id { return Err(notify_error(msg::NOT_FOUND)); }
    let overrides = dogma::overrides(ex, item_id).await.map_err(db_err)?;
    let damage = overrides.iter().find(|row| row.attribute_id == ATTR_HULL_DAMAGE).map_or(0.0, |row| row.value);
    let modifiers = Value::Dict(metadata.modifiers.into_iter().map(|(id, value)| (Value::Int(i64::from(id)), Value::Float(value))).collect());
    let attributes = Value::Dict(overrides.into_iter().map(|row| (Value::Int(i64::from(row.attribute_id)), Value::Float(row.value))).collect());
    Ok(keyval([("itemID", Value::Int(item_id)), ("typeID", Value::Int(i64::from(item.type_id))), ("sourceTypeID", Value::Int(i64::from(metadata.source_type_id))), ("mutatorTypeID", Value::Int(i64::from(metadata.mutator_type_id))), ("modifiers", modifiers), ("attributes", attributes), ("damaged", Value::Float(damage))]))
}

fn ids<const N: usize>(args: &[Value]) -> Result<[i64; N], CallError> {
    if args.len() != N { return Err(notify_error(msg::INVALID_ARGUMENT)); }
    let mut ids = [0; N];
    for (id, value) in ids.iter_mut().zip(args) {
        *id = match value { Value::Int(value) if *value > 0 => *value, _ => return Err(notify_error(msg::INVALID_ARGUMENT)) };
    }
    Ok(ids)
}

async fn create(c: &mut Client, mutator_id: i64, source_id: i64) -> CallResult {
    let _lock = market::lock().await;
    let mutator = load_items(&c.db, &c.sde, ItemQuery::Id(mutator_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(msg::INVALID_ITEMS))?;
    let draws = mutation::draws(&c.sde, mutator.type_id, rand01)?;
    let source = load_items(&c.db, &c.sde, ItemQuery::Id(source_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(msg::INVALID_ITEMS))?;
    let context = Context { character: c.session.int("charid").unwrap_or(0), station: c.session.int("stationid"), ship: c.session.int("shipid"), system: c.session.int("solarsystemid") };
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(move |tx, outbox: &mut Outbox| {
        let sde = sde.clone();
        let context = context.clone();
        let draws = draws.clone();
        let mutator = mutator.clone();
        let source = source.clone();
        Box::pin(async move {
            mutation::create(tx, &sde, outbox, &context, &mutator, &source, &draws).await.map(Value::Int)
        })
    }, |value, outbox| async move { dispatch_client(value, outbox, c) }).await
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "GetDynamicItemInfo" => Some(match ids::<1>(call.args) { Ok(ids) => info(&c.db, &c.sde, ids[0]).await, Err(e) => Err(e) }),
        "CreateDynamicItem" => Some(match ids::<2>(call.args) { Ok(ids) => create(c, ids[0], ids[1]).await, Err(e) => Err(e) }),
        _ => None,
    }
}
