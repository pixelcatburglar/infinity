//! Permanent first-party license activation, backed by game items and licenses.
//! `cosmeticsSvc.py` consumes item changes followed by license events.
//! Timed, structure and single-use licenses are not activated here.
use std::collections::{BTreeMap, BTreeSet};
use marshal::Value;
use crate::{consts::IX_STACK_SIZE, dao::{items::{loading::ItemQuery, quantities::{self, QuantityChange}}, ship_skins, trades}, db::{executor::Executor, outbox::Outbox}, messages::{self, ship_skins as text}, sde::Sde, services::{db_err, identity::Identity, inventory::{self, reachability::{Endpoint, Reachability}}, items::load_items, notify_error, user_error, CallError}};

pub(super) async fn grant_permanent(ex: &(impl Executor + ?Sized), sde: &Sde, region: &str, character: i64, skin: i32) -> Result<(), CallError> {
    if !sde.ship_skins.skin(skin).is_some_and(|s| !s.is_structure_skin && s.visible(region)) {
        return Err(notify_error(text::INVALID_SKIN));
    }
    let stored = ship_skins::get_stored_license(ex, character, skin).await.map_err(db_err)?;
    let changed = match stored {
        None => ship_skins::insert_license(ex, character, skin, None).await.map_err(db_err)?,
        Some(row) => match row.expires_at {
            None => return Err(user_error(messages::MSG_SKIN_ALREADY_LICENSED, vec![])),
            Some(expiry) => ship_skins::update_expiry(ex, character, skin, Some(expiry), None).await.map_err(db_err)?,
        },
    };
    if !changed { return Err(notify_error(text::CHANGED)); }
    Ok(())
}

pub(super) async fn activate_tx<X: Send>(ex: &(impl Executor + ?Sized), identity: &Identity, sde: &Sde, region: &str, reachability: &Reachability, item_ids: &[i64], outbox: &mut Outbox<X>) -> Result<(), CallError> {
    let items = load_items(ex, sde, ItemQuery::Ids(item_ids)).await.map_err(db_err)?;
    if identity.char_id <= 0 || items.len() != item_ids.len() { return Err(notify_error(text::INVALID_ITEM)); }
    if !trades::reserved(ex, item_ids).await.map_err(db_err)?.is_empty() { return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED)); }
    let mut skins = BTreeSet::new();
    let mut changes = BTreeMap::new();
    for item in &items {
        if item.owner_id != identity.char_id || item.quantity <= 0 { return Err(notify_error(text::INVALID_ITEM)); }
        reachability.check(ex, sde, Endpoint { location: item.location_id, flag: item.flag_id }).await?;
        if inventory::access::custody_allowed(ex, sde, item.item_id, identity.char_id, identity.station_id, true).await? == Some(false) {
            return Err(notify_error(messages::NO_CORPORATION_HANGAR_TAKE_ROLE));
        }
        let license = sde.ship_skins.license(item.type_id).ok_or_else(|| notify_error(text::INVALID_ITEM))?;
        let skin = sde.ship_skins.skin(license.skin_id).filter(|s| !s.is_structure_skin && s.visible(region)).ok_or_else(|| notify_error(text::INVALID_SKIN))?;
        if license.is_single_use { return Err(notify_error(text::SINGLE_USE)); }
        if !license.is_permanent() { return Err(notify_error(text::TIMED)); }
        if !skins.insert(skin.skin_id) { return Err(user_error(messages::MSG_SKIN_ALREADY_LICENSED, vec![])); }
        changes.insert(item.item_id, QuantityChange::new(item, (item.quantity > 1).then_some(item.quantity - 1)));
    }
    for &skin in &skins { grant_permanent(ex, sde, region, identity.char_id, skin).await?; }
    if !quantities::apply(ex, &changes).await.map_err(db_err)? { return Err(notify_error(text::CHANGED)); }
    for item in items {
        if item.quantity > 1 {
            inventory::notifications::notify_item_change(ex, sde, outbox, item.item_id, vec![(IX_STACK_SIZE, Value::Int(i64::from(item.quantity)))]).await?;
        } else { inventory::notifications::notify_item_removed(outbox, item); }
    }
    for skin in skins { outbox.notify_pilot(identity.char_id, "OnSkinLicenseActivated", vec![Value::Int(i64::from(skin)), Value::Int(identity.char_id)]); }
    Ok(())
}
