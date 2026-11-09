//! Ship SKIN application using game items and effective licenses.
//! `shipSkinApplicationSvc.py` applies and cancels first-party SKINs.
//! Structure and third-party applications are not supported.
use infinity_core::{consts::CATEGORY_SHIP, messages::{self, ship_skins as text}};
use crate::{dao::{items::loading::ItemQuery, ship_skins::{self, ShipState}, trades}, db::executor::Executor, sde::Sde, services::{db_err, identity::Identity, inventory::reachability::{Endpoint, Reachability}, items::load_items, notify_error, user_error, CallError}};

pub(super) async fn apply_tx(ex: &(impl Executor + ?Sized), identity: &Identity, sde: &Sde, region: &str, reachability: &Reachability, ship_id: i64, skin_id: Option<i32>, now: i64) -> Result<Option<ShipState>, CallError> {
    let ship = load_items(ex, sde, ItemQuery::Id(ship_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(text::INVALID_ITEM))?;
    if identity.char_id <= 0 || ship.owner_id != identity.char_id || ship.category_id != i64::from(CATEGORY_SHIP) || !ship.singleton || ship.quantity != 1 { return Err(notify_error(text::INVALID_ITEM)); }
    reachability.check(ex, sde, Endpoint { location: ship.item_id, flag: ship.flag_id }).await?;
    if crate::services::inventory::access::custody_allowed(ex, sde, ship.item_id, identity.char_id, identity.station_id, true).await? == Some(false) {
        return Err(notify_error(messages::NO_CORPORATION_HANGAR_TAKE_ROLE));
    }
    if !trades::reserved(ex, &[ship_id]).await.map_err(db_err)?.is_empty() { return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED)); }
    if let Some(skin_id) = skin_id {
        if !sde.ship_skins.skin(skin_id).is_some_and(|skin| !skin.is_structure_skin && skin.visible(region) && skin.supports(ship.type_id)) || ship_skins::get_license(ex, identity.char_id, skin_id, now).await.map_err(db_err)?.is_none() {
            return Err(notify_error(text::INVALID_SKIN));
        }
    }
    let state = ship_skins::states_for_ships(ex, &[ship_id], now).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(text::CHANGED))?;
    if skin_id.is_some() && state.skin_id == skin_id { return Err(user_error(messages::MSG_SKIN_ALREADY_APPLIED, vec![])); }
    let changed = match skin_id {
        Some(id) => ship_skins::set_application(ex, identity.char_id, &ship, id, now).await.map_err(db_err)?,
        None => ship_skins::clear_application(ex, identity.char_id, &ship).await.map_err(db_err)?,
    };
    if !changed { return Err(notify_error(text::CHANGED)); }
    if skin_id.is_none() && state.skin_id.is_none() { return Ok(None); }
    Ok(Some(ship_skins::states_for_ships(ex, &[ship_id], now).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(text::CHANGED))?))
}
