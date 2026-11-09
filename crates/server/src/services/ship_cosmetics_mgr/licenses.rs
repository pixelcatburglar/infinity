//! First-party permanent and timed license activation backed by game items.
//! `cosmeticsSvc.py` consumes item changes followed by license events.
//! Static eligibility and duration come from the SDE; licenses and items are
//! persisted in `game.ship_skin_licenses` and `game.items`. The server's checked
//! renewal policy extends from the later of the existing expiry and current time.
//! Structure and single-use license activation are unsupported.
use std::collections::{BTreeMap, BTreeSet};
use marshal::Value;
use infinity_core::{consts::IX_STACK_SIZE, messages::{self, ship_skins as text}};
use crate::{dao::{items::{loading::ItemQuery, quantities::{self, QuantityChange}}, ship_skins, trades}, db::{executor::Executor, outbox::Outbox}, sde::Sde, services::{db_err, identity::Identity, inventory::{self, reachability::{Endpoint, Reachability}}, items::load_items, notify_error, user_error, CallError}};

pub(super) async fn grant_tx(ex: &(impl Executor + ?Sized), sde: &Sde, region: &str, character: i64, skin: i32, duration: Option<i64>, now: i64) -> Result<(), CallError> {
    if !sde.ship_skins.skin(skin).is_some_and(|s| !s.is_structure_skin && s.visible(region)) {
        return Err(notify_error(text::INVALID_SKIN));
    }
    let stored = ship_skins::get_stored_license(ex, character, skin).await.map_err(db_err)?;
    if stored.as_ref().is_some_and(|row| row.expires_at.is_none()) { return Err(user_error(messages::MSG_SKIN_ALREADY_LICENSED, vec![])); }
    let expires = match duration {
        None => None,
        Some(days) => {
            let ticks = days.checked_mul(infinity_core::consts::FILETIME_DAY).filter(|_| days > 0).ok_or_else(|| notify_error(text::INVALID_ARGUMENT))?;
            let base = stored.as_ref().and_then(|row| row.expires_at).unwrap_or(now).max(now);
            let expiry = base.checked_add(ticks).ok_or_else(|| notify_error(text::INVALID_ARGUMENT))?;
            expiry.checked_add(1).ok_or_else(|| notify_error(text::INVALID_ARGUMENT))?;
            Some(expiry)
        }
    };
    let changed = match stored {
        None => ship_skins::insert_license(ex, character, skin, expires).await.map_err(db_err)?,
        Some(row) => ship_skins::update_expiry(ex, character, skin, row.expires_at, expires).await.map_err(db_err)?,
    };
    if !changed { return Err(notify_error(text::CHANGED)); }
    Ok(())
}

pub(super) async fn activate_tx<X: Send>(ex: &(impl Executor + ?Sized), identity: &Identity, sde: &Sde, region: &str, reachability: &Reachability, item_ids: &[i64], now: i64, outbox: &mut Outbox<X>) -> Result<(), CallError> {
    let items = load_items(ex, sde, ItemQuery::Ids(item_ids)).await.map_err(db_err)?;
    if identity.char_id <= 0 || items.len() != item_ids.len() { return Err(notify_error(text::INVALID_ITEM)); }
    if !trades::reserved(ex, item_ids).await.map_err(db_err)?.is_empty() { return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED)); }
    let mut skins = BTreeSet::new();
    let mut grants = Vec::new();
    let mut permanent = BTreeSet::new();
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
        if !skins.insert(skin.skin_id) && (license.is_permanent() || permanent.contains(&skin.skin_id)) {
            return Err(user_error(messages::MSG_SKIN_ALREADY_LICENSED, vec![]));
        }
        if license.is_permanent() { permanent.insert(skin.skin_id); }
        grants.push((skin.skin_id, if license.is_permanent() { None } else { Some(i64::from(license.duration_days)) }));
        changes.insert(item.item_id, QuantityChange::new(item, (item.quantity > 1).then_some(item.quantity - 1)));
    }
    for (skin, duration) in grants { grant_tx(ex, sde, region, identity.char_id, skin, duration, now).await?; }
    if !quantities::apply(ex, &changes).await.map_err(db_err)? { return Err(notify_error(text::CHANGED)); }
    for item in items {
        if item.quantity > 1 {
            inventory::notifications::notify_item_change(ex, sde, outbox, item.item_id, vec![(IX_STACK_SIZE, Value::Int(i64::from(item.quantity)))]).await?;
        } else { inventory::notifications::notify_item_removed(outbox, item); }
    }
    for skin in skins { outbox.notify_pilot(identity.char_id, "OnSkinLicenseActivated", vec![Value::Int(i64::from(skin)), Value::Int(identity.char_id)]); }
    Ok(())
}
