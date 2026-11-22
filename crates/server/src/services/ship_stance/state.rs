//! Reads and repairs the persisted tactical stance of one hull.
//! Mirrors `shipmode/shipstance.py`: the current stance is derived only from
//! the real hidden mode item (`inventorycommon/const.py` flagHiddenModifers);
//! `game.ship_stances` bounds the previous switch and its cooldown deadline.
//! The caller holds the hull lock and transaction; this module opens no nested
//! transaction, touches no World and sends no immediate notification.
//! Non-stance hulls create nothing; unrelated hidden items are preserved,
//! while partial or malformed stance state is refused as an integrity error.
use anyhow::anyhow;
use infinity_core::consts::{FLAG_HIDDEN_MODIFIERS, IX_LOCATION_ID};
use marshal::Value;
use crate::dao::items::loading::{GameItem, Item, ItemQuery};
use crate::dao::items::{creation, ship_stance as timing};
use crate::db::{executor::Executor, outbox::Outbox};
use crate::sde::{ship_stances, Sde};
use crate::services::{db_err, CallError};
use super::StanceSnapshot;

fn integrity(reason: &str) -> CallError {
    CallError::Internal(anyhow!("ship stance integrity violation: {reason}"))
}

/// The stored stance of a hull, `None` for unsupported or non-assembled hulls
/// and for supported hulls that hold no stance state yet. Half state is an
/// integrity error.
pub(crate) async fn read(ex: &(impl Executor + ?Sized), sde: &Sde, hull: &Item) -> Result<Option<StanceSnapshot>, CallError> {
    if !hull.singleton || ship_stances::for_hull(hull.type_id).is_none() { return Ok(None); }
    snapshot(ex, sde, hull).await
}

/// Idempotent initialization inside the caller's transaction: a supported hull
/// that has neither a mode item nor a timing row gains the client's default
/// mode and a never-switched row. Existing valid state is returned unchanged,
/// so an old cooldown deadline is never reset.
pub(crate) async fn ensure_initialized_tx<E: Send>(
    ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<E>, hull: &Item,
) -> Result<Option<StanceSnapshot>, CallError> {
    if !hull.singleton || ship_stances::for_hull(hull.type_id).is_none() { return Ok(None); }
    if let Some(snapshot) = snapshot(ex, sde, hull).await? { return Ok(Some(snapshot)); }
    let mode_type = ship_stances::mode_type(hull.type_id, ship_stances::DEFAULT_STANCE)
        .ok_or_else(|| integrity("default stance unresolved for supported hull"))?;
    let item = create_mode_item(ex, sde, outbox, hull, mode_type).await?;
    if !timing::insert_timing(ex, hull.item_id).await.map_err(db_err)? {
        return Err(integrity("timing row appeared during initialization"));
    }
    Ok(Some(StanceSnapshot { item, previous_stance_id: None, switched_at: 0, ready_at: 0, current: ship_stances::DEFAULT_STANCE }))
}

/// The only mode-item creation path: the singleton insert, SDE enrichment and
/// the owner's item-change notice, shared by initialization and switching.
pub(super) async fn create_mode_item<E: Send>(
    ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<E>, hull: &Item, mode_type: i32,
) -> Result<Item, CallError> {
    let item_id = creation::rookie_ship(ex, mode_type, hull.owner_id, hull.item_id, FLAG_HIDDEN_MODIFIERS).await.map_err(db_err)?;
    let item = crate::services::items::enrich_item(sde, GameItem {
        item_id, type_id: mode_type, owner_id: hull.owner_id, location_id: hull.item_id,
        flag_id: FLAG_HIDDEN_MODIFIERS, quantity: 1, singleton: true, custom_info: None,
    }).ok_or_else(|| integrity("created mode type missing from SDE"))?;
    crate::services::inventory::notifications::notify_owner_item_change(
        outbox, &item, hull.owner_id, [(IX_LOCATION_ID, Value::Int(0))]);
    Ok(item)
}

/// `None` only when no recognized mode item and no timing row exist; anything
/// else malformed is an integrity error and never silently repaired or removed.
async fn snapshot(ex: &(impl Executor + ?Sized), sde: &Sde, hull: &Item) -> Result<Option<StanceSnapshot>, CallError> {
    let items = crate::services::items::load_items(ex, sde, ItemQuery::LocationFlags(hull.item_id, &[FLAG_HIDDEN_MODIFIERS])).await.map_err(db_err)?;
    let timing = timing::timing(ex, hull.item_id).await.map_err(db_err)?;
    let mut mode: Option<Item> = None;
    for item in items {
        if ship_stances::stance_for_mode(hull.type_id, item.type_id).is_some() {
            if item.owner_id != hull.owner_id || item.quantity != 1 || !item.singleton {
                return Err(integrity("mode item owner, quantity or singleton mismatch"));
            }
            if mode.replace(item).is_some() {
                return Err(integrity("multiple recognized mode items"));
            }
        } else if ship_stances::HULL_STANCES.iter().any(|stances| stances.modes.iter().any(|&(_, mode)| mode == item.type_id)) {
            return Err(integrity("mode item of another hull present"));
        }
    }
    match (mode, timing) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(integrity("timing row without a mode item")),
        (Some(_), None) => Err(integrity("mode item without a timing row")),
        (Some(item), Some(timing)) => {
            let current = ship_stances::stance_for_mode(hull.type_id, item.type_id).unwrap();
            Ok(Some(StanceSnapshot { item, previous_stance_id: timing.previous_stance_id, switched_at: timing.switched_at, ready_at: timing.ready_at, current }))
        }
    }
}

/// The `shipStance` slim field for one ball being prepared: the persisted
/// stance of a supported hull, `None` for every other ship. Ball preparation
/// opens no transaction and creates nothing, so a supported hull without
/// complete state is an integrity error, never a silent default.
pub(crate) async fn slim_field(ex: &(impl Executor + ?Sized), sde: &Sde, ship: i64, type_id: i32) -> Result<Option<Value>, CallError> {
    if ship_stances::for_hull(type_id).is_none() { return Ok(None); }
    let hull = crate::services::items::load_items(ex, sde, ItemQuery::Id(ship)).await.map_err(db_err)?
        .into_iter().next().ok_or_else(|| integrity("supported hull item missing"))?;
    let Some(snapshot) = read(ex, sde, &hull).await? else {
        return Err(integrity("supported hull without stance state"));
    };
    Ok(Some(stance_slim_value(snapshot.previous_stance_id, snapshot.switched_at, snapshot.current)))
}

/// `shipmode/notifier.py` `get_slim_item_field` payload:
/// `(oldStanceID, time, newStanceID)`; time is the server's FILETIME stamp.
pub(crate) fn stance_slim_value(previous: Option<i64>, switched_at: i64, current: i64) -> Value {
    Value::tuple([previous.map(Value::Int).unwrap_or(Value::None), Value::Int(switched_at), Value::Int(current)])
}

/// `shipmode/notifier.py` `OnStanceActive` arguments: `(shipID, newStanceID)`.
pub(crate) fn stance_active_args(ship: i64, current: i64) -> Vec<Value> {
    vec![Value::Int(ship), Value::Int(current)]
}
