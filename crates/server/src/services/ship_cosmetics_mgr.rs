//! First-party shipCosmeticsMgr calls used by `cosmeticsSvc.py`.
//! Activation, application, GML actions and queries converge timed expiry.
//! Licenses and applications are persisted in `game.ship_skin_licenses` and
//! `game.ship_skin_applications`, items in `game.items`, and deadlines in `game.jobs`;
//! static eligibility comes from the SDE. Structure and single-use activation are refused.
mod application;
pub(crate) mod expiry;
pub(crate) mod state;
mod licenses;
#[cfg(test)] mod tests;

use std::{collections::BTreeSet, sync::Arc};
use marshal::Value;
use crate::{client::{Client, filetime_now}, dao::{characters::selection::lock_character, items::loading::ItemQuery, ship_skins}, db::outbox::{dispatch_client, Outbox}, messages::ship_skins as text, services::{db_err, identity::Identity, inventory::reachability::{Endpoint, Reachability}, items::load_items, keyval, market, notify_error, Call, CallError, CallResult}};

fn positive(value: &Value) -> Result<i64, CallError> {
    match value { Value::Int(n) if *n > 0 => Ok(*n), _ => Err(notify_error(text::INVALID_ARGUMENT)) }
}
fn small(value: &Value) -> Result<i32, CallError> {
    i32::try_from(positive(value)?).map_err(|_| notify_error(text::INVALID_ARGUMENT))
}
fn activation_ids(args: &[Value]) -> Result<Vec<i64>, CallError> {
    let [Value::List(values)] = args else { return Err(notify_error(text::INVALID_ARGUMENT)); };
    let ids: BTreeSet<_> = values.iter().map(positive).collect::<Result<_, _>>()?;
    if ids.is_empty() || ids.len() != values.len() { return Err(notify_error(text::INVALID_ARGUMENT)); }
    Ok(ids.into_iter().collect())
}
fn visible(sde: &crate::sde::Sde, region: &str, skin: i32) -> bool {
    sde.ship_skins.skin(skin).is_some_and(|s| !s.is_structure_skin && s.visible(region))
}
fn record(row: ship_skins::License, include_id: bool) -> Value {
    let mut fields = vec![("expires", row.expires_at.map_or(Value::None, Value::Int)), ("isSingleUse", Value::Bool(false))];
    if include_id { fields.push(("skinID", Value::Int(i64::from(row.skin_id)))); }
    keyval(fields)
}
async fn activate(c: &mut Client, ids: Vec<i64>) -> CallResult {
    let _guard = market::lock().await;
    let identity = Arc::new(Identity::of_client(c));
    let rows = load_items(&c.db, &c.sde, ItemQuery::Ids(&ids)).await.map_err(db_err)?;
    if rows.len() != ids.len() { return Err(notify_error(text::INVALID_ITEM)); }
    let endpoints: Vec<_> = rows.iter().map(|row| Endpoint { location: row.location_id, flag: row.flag_id }).collect();
    let reach = Reachability::capture(&identity, &c.db, &c.world, c.session.int("solarsystemid"), &endpoints).await?;
    let targets = expiry::prepare(&c.db, &c.world, identity.char_id).await?;
    let now = filetime_now();
    let db = c.db.clone(); let sde = c.sde.clone(); let region = c.cfg.server.boot_region.clone();
    db.transaction(move |tx, outbox: &mut Outbox| {
        let identity = identity.clone(); let sde = sde.clone(); let region = region.clone(); let reach = reach.clone(); let ids = ids.clone(); let targets = targets.clone();
        Box::pin(async move {
            lock_character(tx, identity.char_id).await.map_err(db_err)?;
            expiry::expire_tx(tx, identity.char_id, now, &targets, outbox).await?;
            licenses::activate_tx(tx, &identity, &sde, &region, &reach, &ids, now, outbox).await?;
            expiry::schedule(tx, identity.char_id).await?;
            Ok(Value::None)
        })
    }, |value, outbox| async move { let result = dispatch_client(value, outbox, c); crate::scheduler::wake(); result }).await
}
async fn apply(c: &mut Client, ship: i64, skin: Option<i32>) -> CallResult {
    let _guard = market::lock().await;
    let identity = Arc::new(Identity::of_client(c));
    let row = load_items(&c.db, &c.sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(text::INVALID_ITEM))?;
    let endpoint = Endpoint { location: row.item_id, flag: row.flag_id };
    let reach = Reachability::capture(&identity, &c.db, &c.world, c.session.int("solarsystemid"), &[endpoint]).await?;
    let mut recipients = if let Some(system) = identity.system_id.filter(|_| identity.station_id.is_none()) { crate::space::ship_cosmetics::recipients(&c.world, system, ship).await } else { Vec::new() };
    recipients.push(identity.char_id);
    let fact = if let Some(system) = identity.system_id.filter(|_| identity.station_id.is_none()) {
        crate::space::ship_cosmetics::current(&c.world, system, ship).await
    } else { None }.unwrap_or(crate::space::ship_cosmetics::Ship { ship_id: ship, type_id: row.type_id, character_id: (identity.ship_id == Some(ship)).then_some(identity.char_id), is_npc: false });
    let facts = vec![fact];
    let targets = expiry::prepare(&c.db, &c.world, identity.char_id).await?;
    let db = c.db.clone(); let sde = c.sde.clone(); let region = c.cfg.server.boot_region.clone(); let now = filetime_now();
    db.transaction(move |tx, outbox: &mut Outbox| {
        let identity = identity.clone(); let sde = sde.clone(); let region = region.clone(); let reach = reach.clone(); let recipients = recipients.clone(); let facts = facts.clone(); let targets = targets.clone();
        Box::pin(async move {
            lock_character(tx, identity.char_id).await.map_err(db_err)?;
            expiry::expire_tx(tx, identity.char_id, now, &targets, outbox).await?;
            if application::apply_tx(tx, &identity, &sde, &region, &reach, ship, skin, now).await?.is_some() {
                let states = state::project(tx, &sde, &facts, now).await?;
                for notice in crate::gateway::ship_cosmetics::notice(&states[0], &recipients)? { outbox.notify_gateway(notice); }
            }
            Ok(Value::None)
        })
    }, |value, outbox| async move { let result = dispatch_client(value, outbox, c); crate::scheduler::wake(); result }).await
}
async fn query(c: &Client, method: &str, args: &[Value]) -> CallResult {
    let me = Identity::of_client(c).char_id;
    if me <= 0 { return Err(notify_error(text::INVALID_ARGUMENT)); }
    let region = &c.cfg.server.boot_region; let now = filetime_now();
    match (method, args) {
        ("GetLicencedSkins", []) => {
            expiry::converge(&c.db, &c.world, me, now).await?;
            let rows = ship_skins::licensed(&c.db, me, now).await.map_err(db_err)?;
            Ok(Value::list(rows.into_iter().filter(|r| visible(&c.sde, region, r.skin_id)).map(|r| record(r, true))))
        }
        ("GetLicencedSkinsForShipType", [character, ty]) => {
            if positive(character)? != me { return Err(notify_error(text::INVALID_ARGUMENT)); }
            let ty = small(ty)?;
            expiry::converge(&c.db, &c.world, me, now).await?;
            let rows = ship_skins::licensed(&c.db, me, now).await.map_err(db_err)?;
            Ok(Value::list(rows.into_iter().filter(|r| visible(&c.sde, region, r.skin_id) && c.sde.ship_skins.skin(r.skin_id).is_some_and(|s| s.supports(ty))).map(|r| record(r, true))))
        }
        ("GetFirstPartySkinData", [character, skin]) => {
            let character = positive(character)?; let skin = small(skin)?;
            expiry::converge(&c.db, &c.world, character, now).await?;
            if !visible(&c.sde, region, skin) { return Ok(Value::None); }
            Ok(ship_skins::get_license(&c.db, character, skin, now).await.map_err(db_err)?.map_or(Value::None, |r| record(r, false)))
        }
        _ => Err(notify_error(text::INVALID_ARGUMENT)),
    }
}
#[derive(Clone, Copy)]
enum GmAction { Give(Option<i64>), Expire, Remove }

fn gm_args(call: &Call<'_>, me: i64) -> Result<(i32, GmAction), CallError> {
    let invalid = || notify_error(text::INVALID_ARGUMENT);
    let pairs = match call.kwargs { Value::None => &[][..], Value::Dict(pairs) => pairs.as_slice(), _ => return Err(invalid()) };
    match call.method {
        "GiveSkin" => {
            let [skin] = call.args else { return Err(invalid()) };
            let mut duration = None;
            let mut single = None;
            for (key, value) in pairs {
                match key.as_text().as_deref() {
                    Some("duration") if duration.is_none() => duration = Some(positive(value)?),
                    Some("isSingleUse") if single.is_none() => match value { Value::Bool(v) => single = Some(*v), _ => return Err(invalid()) },
                    _ => return Err(invalid()),
                }
            }
            if single == Some(true) { return Err(notify_error(text::SINGLE_USE)); }
            Ok((small(skin)?, GmAction::Give(duration)))
        }
        "GMExpireSkinLicense" if pairs.is_empty() => {
            let [skin] = call.args else { return Err(invalid()) };
            Ok((small(skin)?, GmAction::Expire))
        }
        "RemoveSkin" if pairs.is_empty() => {
            let [skin, character] = call.args else { return Err(invalid()) };
            if positive(character)? != me { return Err(invalid()); }
            Ok((small(skin)?, GmAction::Remove))
        }
        _ => Err(invalid()),
    }
}
fn notify_empty_removal<X>(outbox: &mut Outbox<X>, character: i64) {
    outbox.notify_pilot(character, "OnRemoteMessage", vec![Value::str(crate::messages::MSG_CUSTOM_NOTIFY), Value::dict([("notify", Value::unicode(text::REMOVED_NEXT_LOGIN))])]);
}

async fn gm(c: &mut Client, call: &Call<'_>) -> CallResult {
    if c.login.role & crate::consts::ROLE_GML == 0 { return Err(notify_error(crate::messages::INSUFFICIENT_ROLE_FOR_OPERATION)); }
    let character = Identity::of_client(c).char_id;
    if character <= 0 { return Err(notify_error(text::INVALID_ARGUMENT)); }
    let (skin, action) = gm_args(call, character)?;
    if !c.sde.ship_skins.skin(skin).is_some_and(|row| !row.is_structure_skin) { return Err(notify_error(text::INVALID_SKIN)); }
    let _guard = market::lock().await;
    let targets = expiry::prepare(&c.db, &c.world, character).await?;
    let db = c.db.clone(); let sde = c.sde.clone(); let region = c.cfg.server.boot_region.clone(); let now = filetime_now();
    db.transaction(|tx, outbox: &mut Outbox| {
        let targets = targets.clone(); let sde = sde.clone(); let region = region.clone();
        let action = action;
        Box::pin(async move {
            lock_character(tx, character).await.map_err(db_err)?;
            match action {
                GmAction::Give(duration) => {
                    expiry::expire_tx(tx, character, now, &targets, outbox).await?;
                    licenses::grant_tx(tx, &sde, &region, character, skin, duration, now).await?;
                    outbox.notify_pilot(character, "OnSkinLicenseActivated", vec![Value::Int(i64::from(skin)), Value::Int(character)]);
                }
                GmAction::Expire => {
                    let row = ship_skins::get_stored_license(tx, character, skin).await.map_err(db_err)?.ok_or_else(|| notify_error(text::INVALID_SKIN))?;
                    let past = now.checked_sub(1).ok_or_else(|| notify_error(text::INVALID_ARGUMENT))?;
                    if !ship_skins::update_expiry(tx, character, skin, row.expires_at, Some(past)).await.map_err(db_err)? { return Err(notify_error(text::CHANGED)); }
                    let count = expiry::remove_tx(tx, character, skin, Some(now), &targets, outbox).await?.ok_or_else(|| notify_error(text::CHANGED))?;
                    if count == 0 { notify_empty_removal(outbox, character); }
                    expiry::expire_tx(tx, character, now, &targets, outbox).await?;
                }
                GmAction::Remove => {
                    let count = expiry::remove_tx(tx, character, skin, None, &targets, outbox).await?.ok_or_else(|| notify_error(text::INVALID_SKIN))?;
                    if count == 0 { notify_empty_removal(outbox, character); }
                }
            }
            expiry::schedule(tx, character).await?;
            Ok(Value::None)
        })
    }, |value, outbox| async move { let result = dispatch_client(value, outbox, c); crate::scheduler::wake(); result }).await
}

pub(crate) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "shipCosmeticsMgr" { return None; }
    Some(match call.method {
        "GiveSkin" | "GMExpireSkinLicense" | "RemoveSkin" => gm(c, call).await,
        "ActivateSkinLicense" => match activation_ids(call.args) { Ok(ids) => activate(c, ids).await, Err(e) => Err(e) },
        "ApplySkinToShip" => match call.args {
            [ship, skin] => match (positive(ship), if matches!(skin, Value::None) { Ok(None) } else { small(skin).map(Some) }) {
                (Ok(ship), Ok(skin)) => apply(c, ship, skin).await,
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            _ => Err(notify_error(text::INVALID_ARGUMENT)),
        },
        "ActivateStructureSkinR" => match call.args {
            [item] => match positive(item) { Ok(_) => Err(notify_error(text::STRUCTURE)), Err(error) => Err(error) },
            _ => Err(notify_error(text::INVALID_ARGUMENT)),
        },
        "GetLicencedSkins" | "GetLicencedSkinsForShipType" | "GetFirstPartySkinData" => query(c, call.method, call.args).await,
        _ => return None,
    })
}
