//! GM item, ship and fitting commands; the slash service owns the role gate.
//! Uses inventory, fitting, combat and dogma service mechanisms and the startup SDE.
//! Ship creation targets a docked character; `/spawn` creates an assembled hull
//! in the caller's hangar or nearby in space, without boarding it.
//! Ammunition uses the ordinary reload path.
use super::{args::{self, Args}, slash_error, CallError, Client};
use infinity_core::consts::{FLAG_CARGO, FLAG_HANGAR};
use crate::dao::items::loading::ItemQuery;
use infinity_core::messages;
use crate::services::{combat::targeting_charges, dogma, fitting::ship, inventory, items::load_items, db_err};

/// dogma/const.py: chargeGroup1 through chargeGroup5.
const CHARGE_GROUPS: [i32; 5] = [604, 605, 606, 609, 610];
/// dogma/const.py: attributeChargeSize.
const CHARGE_SIZE: i32 = 128;

fn invalid() -> CallError { slash_error(messages::gm::GM_ITEM_USAGE.into()) }

fn positive(args: &Args<'_>, index: usize) -> Result<i32, CallError> {
    args.i64(index).ok().and_then(|n| i32::try_from(n).ok()).filter(|n| *n > 0).ok_or_else(invalid)
}

async fn self_ship(c: &Client, target: &str) -> Result<(i64, i64), CallError> {
    let owner = args::target(c, target).await?;
    if c.session.int("charid") != Some(owner) { return Err(slash_error(messages::gm::GM_ITEM_TARGET_SELF.into())); }
    let ship = c.session.int("shipid").ok_or_else(invalid)?;
    Ok((owner, ship))
}

fn accepts(c: &Client, module: i32, charge: i32) -> bool {
    let Some(data) = c.sde.types.get(charge) else { return false };
    let Some(group) = data.group_id else { return false };
    charge_compatible(group, c.sde.types.attribute(charge, CHARGE_SIZE), |attribute| c.sde.types.attribute(module, attribute))
}

fn charge_compatible(group: i32, charge_size: Option<f64>, module_attribute: impl Fn(i32) -> Option<f64>) -> bool {
    let group_matches = CHARGE_GROUPS.iter().any(|id| module_attribute(*id) == Some(group as f64));
    let size_matches = module_attribute(CHARGE_SIZE).map_or(true, |size| charge_size == Some(size));
    group_matches && size_matches
}


pub(super) async fn spawn(c: &mut Client, args: &[String]) -> Result<i64, CallError> {
    let parsed = Args::new(args);
    parsed.required(0).map_err(|_| slash_error(messages::gm::GM_ITEM_SPAWN_USAGE.into()))?;
    let (type_id, _) = parsed.type_id(c, 0).await?;
    ship::gm_spawn_ship(c, type_id).await
}

pub(super) async fn command(c: &mut Client, command: &str, args: &[String]) -> Result<String, CallError> {
    let usage = match command {
        "ship" if args.is_empty() => Some(messages::gm::GM_ITEM_SHIP_USAGE),
        "fit" if args.len() < 2 => Some(messages::gm::GM_ITEM_FIT_USAGE),
        "unload" if args.len() < 2 => Some(messages::gm::GM_ITEM_UNLOAD_USAGE),
        "load" if args.len() < 3 => Some(messages::gm::GM_ITEM_LOAD_USAGE),
        "online" if args.is_empty() => Some(messages::gm::GM_ITEM_ONLINE_USAGE),
        "iteminfo" if args.is_empty() => Some(messages::gm::GM_ITEM_INFO_USAGE),
        "typeinfo" if args.is_empty() => Some(messages::gm::GM_TYPE_INFO_USAGE),
        _ => None,
    };
    if let Some(usage) = usage { return Err(slash_error(usage.into())); }
    let parsed = Args::new(args);
    match command {
        "ship" => {
            parsed.required(0).map_err(|_| slash_error(messages::gm::GM_ITEM_SHIP_USAGE.into()))?;
            if args.len() > 1 { parsed.required(1).map_err(|_| slash_error(messages::gm::GM_ITEM_SHIP_USAGE.into()))?; }
            let (type_id, name) = parsed.type_id(c, 0).await?;
            let owner = if args.len() > 1 { parsed.target(c, 1).await? } else { c.session.int("charid").ok_or_else(invalid)? };
            let id = ship::gm_create_ship(c, owner, type_id).await?;
            Ok(messages::gm::gm_item_created(id, &name))
        }
        "fit" => {
            parsed.required(0).map_err(|_| slash_error(messages::gm::GM_ITEM_FIT_USAGE.into()))?;
            let target = parsed.target(c, 0).await?;
            parsed.required(1).map_err(|_| slash_error(messages::gm::GM_ITEM_FIT_USAGE.into()))?;
            let item = if let Ok(id) = parsed.i64(1) {
                load_items(&c.db, &c.sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next()
            } else { None };
            let (type_id, item_id) = match item {
                Some(item) => (item.type_id, Some(item.item_id)),
                None => (parsed.type_id(c, 1).await?.0, None),
            };
            let quantity = if args.len() > 2 { positive(&parsed, 2).map_err(|_| slash_error(messages::gm::GM_ITEM_FIT_USAGE.into()))? } else { 1 };
            let count = inventory::gm::gm_fit(c, target, type_id, item_id, quantity).await?;
            Ok(messages::gm::gm_item_fitted(count))
        }
        "unload" => {
            parsed.required(0).map_err(|_| slash_error(messages::gm::GM_ITEM_UNLOAD_USAGE.into()))?;
            let (owner, ship) = self_ship(c, args.first().ok_or_else(invalid)?).await?;
            let requested = parsed.i64(1).map_err(|_| slash_error(messages::gm::GM_ITEM_UNLOAD_USAGE.into()))?;
            let items = load_items(&c.db, &c.sde, ItemQuery::LocationOwner(ship, owner)).await.map_err(db_err)?;
            let selected: Vec<_> = items.into_iter().filter(|item| inventory::is_fitting_flag(item.flag_id) && (item.item_id == requested || i64::from(item.type_id) == requested)).collect();
            if selected.is_empty() { return Err(slash_error(messages::gm::GM_ITEM_NOT_FOUND.into())); }
            let (location, flag) = match c.session.int("stationid") {
                Some(station) => (station, FLAG_HANGAR),
                None => (ship, FLAG_CARGO),
            };
            let count = inventory::gm::gm_unload(c, ship, owner, selected.into_iter().map(|item| item.item_id).collect(), location, flag).await?;
            Ok(messages::gm::gm_item_unloaded(count))
        }
        "load" => {
            parsed.required(0).map_err(|_| slash_error(messages::gm::GM_ITEM_LOAD_USAGE.into()))?;
            parsed.required(1).map_err(|_| slash_error(messages::gm::GM_ITEM_LOAD_USAGE.into()))?;
            let (owner, ship) = self_ship(c, args.first().ok_or_else(invalid)?).await?;
            let (type_id, _) = parsed.type_id(c, 1).await?;
            let quantity = positive(&parsed, 2).map_err(|_| slash_error(messages::gm::GM_ITEM_LOAD_USAGE.into()))?;
            let items = load_items(&c.db, &c.sde, ItemQuery::LocationOwner(ship, owner)).await.map_err(db_err)?;
            let modules: Vec<_> = items.into_iter().filter(|item| inventory::is_fitting_flag(item.flag_id) && accepts(c, item.type_id, type_id)).map(|item| item.item_id).collect();
            if modules.is_empty() { return Err(slash_error(messages::gm::GM_ITEM_NOT_FOUND.into())); }
            let location = c.session.int("stationid").unwrap_or(ship);
            let count = targeting_charges::gm_load_ammo(c, ship, modules, location, type_id, i64::from(quantity)).await?;
            Ok(messages::gm::gm_item_loaded(count))
        }
        "online" => {
            let text = args.first().ok_or_else(invalid)?;
            let ship = if text.eq_ignore_ascii_case("me") { c.session.int("shipid").ok_or_else(invalid)? } else { parsed.i64(0).map_err(|_| slash_error(messages::gm::GM_ITEM_ONLINE_USAGE.into()))? };
            let count = dogma::online::gm_online(c, ship).await?;
            Ok(messages::gm::gm_item_online(count))
        }
        "iteminfo" => {
            let id = parsed.i64(0).map_err(|_| slash_error(messages::gm::GM_ITEM_INFO_USAGE.into()))?;
            let item = load_items(&c.db, &c.sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| slash_error(messages::gm::GM_ITEM_NOT_FOUND.into()))?;
            Ok(messages::gm::gm_item_info(item.item_id, item.type_id, item.owner_id, item.location_id, item.flag_id, item.quantity, item.singleton))
        }
        "typeinfo" => {
            parsed.required(0).map_err(|_| slash_error(messages::gm::GM_TYPE_INFO_USAGE.into()))?;
            let (id, name) = parsed.type_id(c, 0).await?;
            let data = c.sde.types.get(id).ok_or_else(invalid)?;
            let mut lines = vec![messages::gm::gm_type_info(id, &name, data.group_id.unwrap_or(0), data.category_id.unwrap_or(0))];
            lines.extend(c.sde.types.attributes(id).map(|(id, value)| messages::gm::gm_type_attribute(id, value)));
            Ok(lines.join("\n"))
        }
        _ => Err(invalid()),
    }
}

#[cfg(test)] mod tests;
