//! dogmaIM, bound per location, supplies item attribute and effect state.
//! `eve/client/script/environment/godma.py:2440-2454` consumes all-info state,
//! and `eve/client/script/environment/godma.py:1652-1676` reads active effects.
//! Type attributes: `eve/common/script/dogma/baseDogmaLocation.py:1960-1967`.
//! Static dogma and type data come from the shared SDE; items, base overrides, charges and online
//! state come from the runtime game database. For absent dogma rows, mass, capacity,
//! volume and radius fall back to typed SDE type columns: this is server schema
//! behavior, not a validated client fallback. Effective fitting is the subset in
//! `services/fit.rs`, not a complete implementation of client dogma.
//! Module online changes and resource reconciliation use the shared effects policy.
//! Character brains compile passive skills and active implant and booster modifiers;
//! structure effects remain unsupported.
//! Brain versions are in-memory and reset when the server restarts.
//! Ship priming reports settled runtime capacitor, rack heat and overload state;
//! ships outside space use fitted values and no transient heat.
//! Ship state can also be assembled after reconciliation under the lifecycle lock.
pub(crate) mod brain;


use super::{db_err, item_descriptor, keyval, Call, CallError, CallResult};
use super::{combat, exploration, fit, skills};
use super::identity::Identity;
use crate::client::{filetime_now, short, Client};
use crate::dao::items::loading::{Item, ItemQuery};
use super::items::load_items;
use crate::dao::items::{charges, dogma as dogma_dao};
use crate::consts::{dogma::ATTR_ARMOR_DAMAGE, dogma::ATTR_CAPACITOR_CAPACITY, dogma::ATTR_CAPACITY, dogma::ATTR_CHARGE, dogma::ATTR_HULL_DAMAGE, dogma::ATTR_IS_ONLINE, dogma::ATTR_MASS, dogma::ATTR_QUANTITY, dogma::ATTR_RADIUS, dogma::ATTR_SHIELD_CAPACITY, dogma::ATTR_SHIELD_CHARGE, dogma::ATTR_VOLUME, dogma::ATTR_HEAT_LOW, dogma::ATTR_HEAT_MED, dogma::ATTR_HEAT_HI, BLUEPRINT_COPY_QUANTITY, CATEGORY_OWNER, dogma::EFFECT_ONLINE, FLAG_HANGAR, FLAG_PILOT, SINGLETON_QUANTITY, TYPE_CHARACTER, TYPE_PLASTIC_WRAP};
use marshal::Value;
use std::collections::BTreeMap;
use tracing::warn;

/// SDE invGroups: Character.
const GROUP_CHARACTER: i64 = 1;

/// Server online-effect duration wire choice for godma.py RefreshItemEffects' tuple.
const ONLINE_EFFECT_DURATION: i64 = -1;
/// Server online-effect repeat wire choice for godma.py RefreshItemEffects' tuple.
const ONLINE_EFFECT_REPEAT: i64 = -1;

/// Inventory wire quantity preserves encoded plastic wrap volumes; other singleton
/// originals are -1 and blueprint copies are -2.
pub(super) fn wire_quantity(type_id: i32, singleton: bool, quantity: i64) -> i64 {
    if !singleton {
        quantity
    } else if type_id == TYPE_PLASTIC_WRAP && quantity < 0 {
        quantity
    } else if quantity == BLUEPRINT_COPY_QUANTITY {
        BLUEPRINT_COPY_QUANTITY
    } else {
        SINGLETON_QUANTITY
    }
}


impl Item {
    pub fn row(&self) -> Value {
        let quantity = wire_quantity(self.type_id, self.singleton, self.quantity as i64);
        item_descriptor().row(&[
            Value::Int(self.item_id),
            Value::Int(self.type_id as i64),
            Value::Int(self.owner_id),
            Value::Int(self.location_id),
            Value::Int(self.flag_id as i64),
            Value::Int(quantity),
            Value::Int(self.group_id),
            Value::Int(self.category_id),
            self.custom_info.as_ref().map(Value::str).unwrap_or(Value::None),
        ])
    }
}


fn type_attributes(c: &Client, type_id: i32) -> BTreeMap<i64, f64> {
    let mut attrs: BTreeMap<i64, f64> = c.sde.types.attributes(type_id)
        .map(|(id, value)| (id as i64, value)).collect();
    // Server typed SDE fallback when these physical attributes lack dogma rows.
    if let Some(t) = c.sde.types.get(type_id) {
        for (attr, value) in [(ATTR_MASS, t.mass), (ATTR_CAPACITY, t.capacity), (ATTR_VOLUME, t.volume), (ATTR_RADIUS, t.radius)] {
            if let Some(v) = value {
                attrs.entry(attr as i64).or_insert(v);
            }
        }
    }
    attrs
}

fn overlay_overrides(attrs: &mut BTreeMap<i64, f64>, overrides: impl IntoIterator<Item = (i32, f64)>) {
    attrs.extend(overrides.into_iter().map(|(id, value)| (id as i64, value)));
}

/// godma's activeEffects for an online module: {effectID: (itemID, charID, shipID, targetID,
/// otherID, area, effectID, startTime, duration, repeat)}. godma counts an effect as running
/// only when it is listed here (RefreshItemEffects), so an empty dict shows every module offline.
fn active_effect(item_id: i64, char_id: i64, ship_id: i64, effect_id: i64) -> (Value, Value) {
    let env = [
        Value::Int(item_id),
        Value::Int(char_id),
        Value::Int(ship_id),
        Value::None,
        Value::None,
        Value::list([]),
        Value::Int(effect_id),
        Value::Int(filetime_now()),
        Value::Int(ONLINE_EFFECT_DURATION),
        Value::Int(ONLINE_EFFECT_REPEAT),
    ];
    (Value::Int(effect_id), Value::tuple(env))
}

fn online_effect(item_id: i64, char_id: i64, ship_id: i64) -> Vec<(Value, Value)> {
    vec![active_effect(item_id, char_id, ship_id, EFFECT_ONLINE)]
}

async fn heat_snapshot(c: &Client, ship: i64, fit: &fit::Fit) -> Vec<super::effects::heat::RackSnapshot> {
    if let Some(system) = c.session.int("solarsystemid") {
        if let Some(snapshot) = super::effects::heat::ship_state(&c.world, system, ship).await { return snapshot; }
    }
    super::effects::heat::empty_ship_state(fit, filetime_now())
}

fn info(item_row: Value, item_id: i64, attributes: BTreeMap<i64, f64>, active: Vec<(Value, Value)>) -> Value {
    info_keyed(item_row, Value::Int(item_id), attributes, active)
}

/// `info` for any item key: an itemID, or a charge sublocation's (shipID, flagID, typeID).
fn info_keyed(item_row: Value, key: Value, attributes: BTreeMap<i64, f64>, active: Vec<(Value, Value)>) -> Value {
    let now = filetime_now();
    keyval([
        ("itemID", key),
        ("invItem", item_row),
        ("attributes", Value::Dict(attributes.into_iter().map(|(k, v)| (Value::Int(k), Value::Float(v))).collect())),
        ("activeEffects", Value::Dict(active)),
        ("time", Value::Int(now)),
        ("wallclockTime", Value::Int(now)),
    ])
}

/// The character as a dogma item, located in its ship.
async fn character_info(c: &Client) -> Result<Value, CallError> {
    let char_id = c.session.int("charid").ok_or_else(|| CallError::Internal(anyhow::anyhow!("no character selected")))?;
    let ship_id = c.session.int("shipid").unwrap_or(0);
    // The client sets these base values before applying the character brain's modifiers.
    let character_attrs = skills::attributes::base_character_attributes(&c.db, &c.sde, char_id).await?;
    let mut attrs = type_attributes(c, TYPE_CHARACTER);
    attrs.extend(character_attrs);
    let item = Item {
        item_id: char_id,
        type_id: TYPE_CHARACTER,
        owner_id: char_id,
        location_id: ship_id,
        flag_id: FLAG_PILOT,
        quantity: 1,
        singleton: true,
        group_id: GROUP_CHARACTER,
        category_id: i64::from(CATEGORY_OWNER),
        custom_info: None,
    };
    Ok(info(item.row(), char_id, attrs, vec![]))
}

/// Fitted modules of `ship_id` that are online (game.items.online).
async fn online_modules(c: &Client, ship_id: i64) -> Result<Vec<i64>, CallError> {
    Ok(dogma_dao::online(&c.db, ship_id).await.map_err(db_err)?
        .into_iter().map(|r| r.item_id).collect())
}

/// Set fitted modules online and notify the controlling client after commit.
pub(super) async fn gm_online(c: &mut Client, ship: i64) -> Result<usize, CallError> {
    use super::effects::online::{dispatch_changed, set_online_core, OnlineChanged, OnlinePolicy};
    use crate::db::outbox::Outbox;
    let _lock = super::market::lock().await;
    let policy = OnlinePolicy::from_identity(&Identity::of_client(c), c.notify_tx.clone())?;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            let policy = policy.clone();
            Box::pin(async move {
                load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next()
                    .filter(|item| item.owner_id == policy.owner && item.category_id == i64::from(crate::consts::CATEGORY_SHIP))
                    .ok_or_else(|| super::notify_error(crate::messages::gm::GM_ITEM_NOT_FOUND))?;
                let mut fitted = load_items(tx, &sde, ItemQuery::LocationOwner(ship, policy.owner)).await.map_err(db_err)?;
                fitted.sort_unstable_by_key(|item| item.item_id);
                let mut count = 0;
                for item in fitted.iter().filter(|item| item.category_id == i64::from(crate::consts::CATEGORY_MODULE)
                    && item.singleton && super::inventory::is_fitting_flag(item.flag_id)
                    && super::effects::lifecycle::has_effect(sde.types.effects(item.type_id), EFFECT_ONLINE as i32)) {
                    count += usize::from(set_online_core(tx, &sde, &policy, outbox, ship, item.item_id, true).await?);
                }
                Ok(count)
            })
        },
        |count, outbox| async move { dispatch_changed(count, outbox, c) },
    ).await
}

/// `(instanceCache, instanceFlagQuantityCache, weaponBanks, heatStates)` for a ship being made
/// active (clientDogmaLocation.MakeShipActive). instanceCache rows follow dgmAttributesByIdx:
/// [itemID, isOnline, damage, charge, skillPoints, armorDamage, shieldCharge, isIncapacitated].
pub async fn ship_state(c: &Client, ship_id: i64) -> Result<Value, CallError> {
    let owner = c.session.int("charid").ok_or_else(|| crate::services::notify_error(crate::messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    super::effects::online::reconcile_ship(&c.db, &c.sde, owner, ship_id, |n| super::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await?;
    ship_state_core(c, ship_id).await
}

/// Assemble reconciled ship state without acquiring the lifecycle lock.
pub(crate) async fn ship_state_core(c: &Client, ship_id: i64) -> Result<Value, CallError> {
    let owner = c.session.int("charid").ok_or_else(|| crate::services::notify_error(crate::messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    let items = load_items(&c.db, &c.sde, ItemQuery::LocationExceptFlag(ship_id, FLAG_HANGAR)).await.map_err(db_err)?;
    let online = online_modules(c, ship_id).await?;
    let fit = if let Some(system) = c.session.int("solarsystemid") {
        fit::Fit::load_in_space(&c.db, &c.world, system, owner, ship_id).await?
    } else {
        fit::Fit::load(&c.db, c.sde.clone(), owner, ship_id).await?
    };
    let heat = heat_snapshot(c, ship_id, &fit).await;
    let instance = |key: Value, online: bool, damage: f64| {
        let row = [
            key.clone(),
            Value::Int(online as i64),
            Value::Float(damage),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Int(0),
        ];
        (key, Value::list(row))
    };
    let mut instances: Vec<(Value, Value)> = items.iter().map(|i| instance(Value::Int(i.item_id), online.contains(&i.item_id), fit.value(fit::Key::Module(i.item_id), ATTR_HULL_DAMAGE))).collect();
    // Loaded charges: sublocations (shipID, flagID, typeID) with their quantities.
    let (quantities, charges) = combat::flag_quantities(c, ship_id).await?;
    instances.extend(charges.into_iter().map(|k| instance(k, false, 0.0)));
    let banks = combat::banks(&c.db, ship_id).await?;
    let heat = heat.into_iter().map(|rack| (Value::Int(i64::from(rack.heat_id)), Value::tuple([
        Value::Float(rack.heat), Value::Float(rack.capacity), Value::Float(rack.incoming),
        Value::Float(rack.generation), Value::Float(rack.dissipation), Value::Int(rack.last_calc_time),
    ]))).collect();
    Ok(Value::tuple([Value::Dict(instances), quantities, banks, Value::Dict(heat)]))
}

/// The active ship and everything fitted in it.
async fn ship_info(c: &Client) -> Result<Vec<(Value, Value)>, CallError> {
    let Some(ship_id) = c.session.int("shipid") else { return Ok(vec![]) };
    let owner = c.session.int("charid").ok_or_else(|| crate::services::notify_error(crate::messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    super::effects::online::reconcile_ship(&c.db, &c.sde, owner, ship_id, |n| super::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await?;
    let items = load_items(&c.db, &c.sde, ItemQuery::IdOrLocationExceptFlag(ship_id, FLAG_HANGAR)).await.map_err(db_err)?;
    let online = online_modules(c, ship_id).await?;
    let mut out = Vec::new();
    // Runtime state the HUD reads, including the current recharged capacitor.
    let char_id = c.session.int("charid").unwrap_or(0);
    let system = c.session.int("solarsystemid");
    let fit = if let Some(system) = system {
        fit::Fit::load_in_space(&c.db, &c.world, system, char_id, ship_id).await?
    } else {
        fit::Fit::load(&c.db, c.sde.clone(), char_id, ship_id).await?
    };
    let heat = heat_snapshot(c, ship_id, &fit).await;
    let overloaded = super::effects::heat::overloaded_modules();
    let (cap_max, shield_max) = (fit.value(fit::Key::Ship, ATTR_CAPACITOR_CAPACITY), fit.value(fit::Key::Ship, ATTR_SHIELD_CAPACITY));
    let cap = if let Some(system) = system {
        combat::groups_health::capacitor_snapshot(&c.db, &c.world, system, ship_id).await?.map_or(cap_max, |snapshot| snapshot.level)
    } else {
        cap_max
    };
    let (shield, armor_damage, hull_damage) = combat::ship_condition(ship_id).unwrap_or((shield_max, 0.0, 0.0));
    for item in items {
        let mut attrs = type_attributes(c, item.type_id);
        // Supply fitted values for ship and module attributes, including HUD maxima.
        let key = if item.item_id == ship_id {
            Some(fit::Key::Ship)
        } else if item.location_id == ship_id {
            Some(fit::Key::Module(item.item_id))
        } else {
            None
        };
        if let Some(key) = key {
            overlay_overrides(&mut attrs, fit.overrides(key).iter().copied());
            for (attr, value) in attrs.iter_mut() {
                *value = fit.value(key, *attr as i32);
            }
        }
        let mut active = if online.contains(&item.item_id) {
            attrs.insert(ATTR_IS_ONLINE as i64, 1.0);
            online_effect(item.item_id, char_id, ship_id)
        } else {
            vec![]
        };
        if overloaded.contains(&item.item_id) {
            for effect in c.sde.types.effects(item.type_id) {
                if c.sde.dogma.effect(effect.effect_id).is_some_and(|row| row.effect_category == Some(super::effects::heat::rpc::OVERLOAD_CATEGORY)) {
                    active.push(active_effect(item.item_id, char_id, ship_id, i64::from(effect.effect_id)));
                }
            }
        }
        attrs.entry(ATTR_MASS as i64).or_insert(0.0); // mass is always present
        if item.item_id == ship_id {
            attrs.insert(ATTR_CHARGE as i64, cap);
            attrs.insert(ATTR_SHIELD_CHARGE as i64, shield);
            attrs.insert(ATTR_ARMOR_DAMAGE as i64, armor_damage);
            attrs.insert(ATTR_HULL_DAMAGE as i64, hull_damage);
            for rack in &heat { attrs.insert(i64::from(rack.heat_id), rack.heat); }
        }
        for k in [ATTR_HULL_DAMAGE as i64, ATTR_ARMOR_DAMAGE as i64, i64::from(ATTR_HEAT_LOW), i64::from(ATTR_HEAT_MED), i64::from(ATTR_HEAT_HI)] {
            attrs.entry(k).or_insert(0.0);
        }
        out.push((Value::Int(item.item_id), info(item.row(), item.item_id, attrs, active)));
    }
    // Loaded charges are sublocations, not items: godma (UpdateItem) files an entry keyed
    // (shipID, flagID, typeID) with no invItem under the ship, with its quantity attribute;
    // the HUD reads a launcher's charge from there.
    let charges = charges::positive(&c.db, ship_id).await.map_err(db_err)?;
    for r in charges {
        let (flag, type_id, qty) = (r.flag_id, r.type_id, r.quantity);
        let mut attrs = type_attributes(c, type_id);
        attrs.insert(ATTR_QUANTITY as i64, qty as f64);
        let key = Value::tuple([Value::Int(ship_id), Value::Int(flag as i64), Value::Int(type_id as i64)]);
        out.push((key.clone(), info_keyed(Value::None, key, attrs, vec![])));
    }
    Ok(out)
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> CallResult {
    // Targeting, ammo, weapon groups and module activation.
    if let Some(r) = combat::dispatch(c, call).await {
        return r;
    }
    if let Some(r) = super::effects::heat::rpc::dispatch(c, call).await { return r; }
    let flag = |i: usize| call.args.get(i).map(|v| !v.is_none() && v.as_int() != Some(0)).unwrap_or(false);
    match call.method {
        "ChangeDroneSettings" => {
            let owner = c.session.int("charid").ok_or_else(|| super::notify_error(crate::messages::drones::NOT_AVAILABLE))?;
            let patch = super::drones::settings::parse(call.args)?;
            let defaults = super::drones::settings::defaults(&c.sde)?;
            super::drones::settings::apply(owner, defaults, patch);
            Ok(Value::None)
        }
        // GetAllInfo(getCharInfo, getShipInfo, structureID)
        "GetAllInfo" => {
            let char_info = if flag(0) {
                let ch = character_info(c).await?;
                let char_id = c.session.int("charid").unwrap_or(0);
                // (charItems, brain); brain = (version, charEffects, shipEffects, structureEffects)
                // -- godma.Prime unpacks "_, charBrain = allInfo.charInfo".
                let brain = brain::bootstrap(c).await?;
                Value::tuple([Value::Dict(vec![(Value::Int(char_id), ch)]), brain])
            } else {
                Value::None
            };
            let ship_info = if flag(1) { Value::Dict(ship_info(c).await?) } else { Value::None };
            Ok(keyval([
                ("charInfo", char_info),
                ("shipInfo", ship_info),
                ("structureInfo", Value::None),
                ("shipModifiedCharAttribs", Value::None),
                ("systemWideEffectsOnShip", Value::None),
                // (instanceCache, instanceFlagQuantityCache, weaponBanks, heatStates) for MakeShipActive.
                ("shipState", match c.session.int("shipid") {
                    Some(ship) => ship_state(c, ship).await?,
                    None => Value::tuple([Value::Dict(vec![]), Value::Dict(vec![]), Value::Dict(vec![]), Value::Dict(vec![])]),
                }),
            ]))
        }
        "ShipGetInfo" => Ok(Value::Dict(ship_info(c).await?)),
        "CharGetInfo" => character_info(c).await,
        "ItemGetInfo" => {
            let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let items = load_items(&c.db, &c.sde, ItemQuery::Id(item_id)).await.map_err(db_err)?;
            match items.into_iter().next() {
                Some(item) => {
                    let mut attrs = type_attributes(c, item.type_id);
                    overlay_overrides(&mut attrs, dogma_dao::overrides(&c.db, item.item_id).await.map_err(db_err)?
                        .into_iter().map(|row| (row.attribute_id, row.value)));
                    let online = online_modules(c, item.location_id).await?.contains(&item.item_id);
                    let char_id = c.session.int("charid").unwrap_or(0);
                    let active = if online { online_effect(item.item_id, char_id, item.location_id) } else { vec![] };
                    Ok(info(item.row(), item.item_id, attrs, active))
                }
                None if Some(item_id) == c.session.int("charid") => character_info(c).await,
                None => Ok(Value::None),
            }
        }
        "GetCharacterBaseAttributes" => {
            let ch = character_info(c).await?;
            Ok(ch)
        }
        "CheckSendLocationInfo" | "LogAttribute" => Ok(Value::None),
        "InjectSkillIntoBrain" => skills::books::inject(c, call.args).await,
        "InjectImplant" => skills::brain_items::inject_implant(c, call.args).await,
        "DestroyImplant" => skills::brain_items::destroy_implant(c, call.args).await,
        "UseBooster" => skills::brain_items::use_booster(c, call.args).await,
        "LaunchProbes" => exploration::launch_probes(c, call).await,
        // Both RPCs use the lifecycle's fitted-subject authorization and notifications.
        "SetModuleOnline" | "TakeModuleOffline" => {
            let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let module = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            let online = call.method == "SetModuleOnline";
            super::effects::online::set_online(&Identity::of_client(c), &c.db, &c.sde, c.notify_tx.clone(), c.login.user_id, ship, module, online, |notification| super::effects::online::dispatch_notification(notification, &c.world, &c.notify_tx, c.session.int("charid"))).await
        }
        _ => {
            warn!(
                user = c.login.user_id,
                "UNHANDLED dogmaIM::{} args={} kwargs={}",
                call.method,
                short(&marshal::Value::Tuple(call.args.to_vec())),
                short(call.kwargs)
            );
            Ok(Value::None)
        }
    }
}

#[cfg(test)] mod tests;
