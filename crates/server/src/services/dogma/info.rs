//! Dogma item, character and ship information for
//! `eve/client/script/environment/godma.py` and `eve/client/script/dogma/`.
//! Uses SDE type and dogma data, game items, attributes and charges, and actor state.
//! Unsupported effects and attributes are not synthesized beyond the available data.
//! Wire stack quantities are projected to the client display limit; stored
//! quantities stay exact (see `cap_client_quantity`).
use crate::services::{db_err, item_descriptor, keyval, CallError};
use crate::services::{combat, fit, skills};
use crate::client::{filetime_now, Client};
use crate::dao::items::loading::{Item, ItemQuery};
use crate::services::items::load_items;
use crate::dao::items::{charges, dogma as dogma_dao};
use crate::sde::Sde;
use infinity_core::consts::{dogma::ATTR_ARMOR_DAMAGE, dogma::ATTR_CAPACITOR_CAPACITY, dogma::ATTR_CAPACITY, dogma::ATTR_CHARGE, dogma::ATTR_HULL_DAMAGE, dogma::ATTR_IS_ONLINE, dogma::ATTR_MASS, dogma::ATTR_QUANTITY, dogma::ATTR_RADIUS, dogma::ATTR_SHIELD_CHARGE, dogma::ATTR_VOLUME, dogma::ATTR_HEAT_LOW, dogma::ATTR_HEAT_MED, dogma::ATTR_HEAT_HI, BLUEPRINT_COPY_QUANTITY, CATEGORY_OWNER, CLIENT_MAX_SHORT_AMOUNT, dogma::EFFECT_ONLINE, FLAG_HANGAR, FLAG_PILOT, GROUP_CHARACTER, SINGLETON_QUANTITY, TYPE_CHARACTER, TYPE_PLASTIC_WRAP};
use marshal::Value;
use std::collections::BTreeMap;

/// Server online-effect duration wire choice for godma.py RefreshItemEffects' tuple.
pub(super) const ONLINE_EFFECT_DURATION: i64 = -1;
/// Server online-effect repeat wire choice for godma.py RefreshItemEffects' tuple.
pub(super) const ONLINE_EFFECT_REPEAT: i64 = -1;

/// Largest stack quantity the client displays correctly: its short amount
/// formats reject absolute amounts above the cap, so wire quantities project
/// it; stored quantities remain unchanged. Upper bound only: singleton and
/// plastic-wrap negative encodings are handled by `wire_quantity`.
pub(in crate::services) fn cap_client_quantity(quantity: i64) -> i64 {
    quantity.min(CLIENT_MAX_SHORT_AMOUNT)
}

/// Inventory wire quantity preserves encoded plastic wrap volumes; other singleton
/// originals are -1 and blueprint copies are -2.
pub(in crate::services) fn wire_quantity(type_id: i32, singleton: bool, quantity: i64) -> i64 {
    if !singleton {
        cap_client_quantity(quantity)
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
        let quantity = wire_quantity(self.type_id, self.singleton, self.quantity);
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


pub(super) fn type_attributes(sde: &Sde, type_id: i32) -> BTreeMap<i64, f64> {
    let mut attrs: BTreeMap<i64, f64> = sde.types.attributes(type_id)
        .map(|(id, value)| (id as i64, value)).collect();
    // Server typed SDE fallback when these physical attributes lack dogma rows.
    if let Some(t) = sde.types.get(type_id) {
        for (attr, value) in [(ATTR_MASS, t.mass), (ATTR_CAPACITY, t.capacity), (ATTR_VOLUME, t.volume), (ATTR_RADIUS, t.radius)] {
            if let Some(v) = value {
                attrs.entry(attr as i64).or_insert(v);
            }
        }
    }
    attrs
}

pub(super) fn overlay_overrides(attrs: &mut BTreeMap<i64, f64>, overrides: impl IntoIterator<Item = (i32, f64)>) {
    attrs.extend(overrides.into_iter().map(|(id, value)| (id as i64, value)));
}

/// godma's activeEffects for an online module: {effectID: (itemID, charID, shipID, targetID,
/// otherID, area, effectID, startTime, duration, repeat)}. godma counts an effect as running
/// only when it is listed here (RefreshItemEffects), so an empty dict shows every module offline.
pub(super) fn active_effect(item_id: i64, char_id: i64, ship_id: i64, effect_id: i64) -> (Value, Value) {
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

pub(super) fn online_effect(item_id: i64, char_id: i64, ship_id: i64) -> Vec<(Value, Value)> {
    vec![active_effect(item_id, char_id, ship_id, EFFECT_ONLINE)]
}

async fn heat_snapshot(c: &Client, ship: i64, fit: &fit::Fit) -> Vec<crate::services::effects::heat::RackSnapshot> {
    if let Some(system) = c.session.int("solarsystemid") {
        if let Some(snapshot) = crate::services::effects::heat::ship_state(&c.world, system, ship).await { return snapshot; }
    }
    crate::services::effects::heat::empty_ship_state(fit, filetime_now())
}

pub(super) fn info(item_row: Value, item_id: i64, attributes: BTreeMap<i64, f64>, active: Vec<(Value, Value)>) -> Value {
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

/// Projects the authoritative integer quantity into a non-authoritative floating-point dogma attribute.
pub(in crate::services) fn charge_info(sde: &Sde, ship: i64, flag: i32, type_id: i32, quantity: i64) -> Value {
    let mut attrs = type_attributes(sde, type_id);
    attrs.insert(ATTR_QUANTITY as i64, cap_client_quantity(quantity) as f64);
    info_keyed(Value::None, combat::targeting_charges::charge_key(ship, flag, type_id), attrs, vec![])
}

/// The character as a dogma item, located in its ship.
pub(super) async fn character_info(c: &Client) -> Result<Value, CallError> {
    let char_id = c.session.int("charid").ok_or_else(|| CallError::Internal(anyhow::anyhow!("no character selected")))?;
    let ship_id = c.session.int("shipid").unwrap_or(0);
    // The client sets these base values before applying the character brain's modifiers.
    let character_attrs = skills::attributes::base_character_attributes(&c.db, &c.sde, char_id).await?;
    let mut attrs = type_attributes(&c.sde, TYPE_CHARACTER);
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
pub(super) async fn online_modules(c: &Client, ship_id: i64) -> Result<Vec<i64>, CallError> {
    Ok(dogma_dao::online(&c.db, ship_id).await.map_err(db_err)?
        .into_iter().map(|r| r.item_id).collect())
}

/// `(instanceCache, instanceFlagQuantityCache, weaponBanks, heatStates)` for a ship being made
/// active (clientDogmaLocation.MakeShipActive). instanceCache rows follow dgmAttributesByIdx:
/// [itemID, isOnline, damage, charge, skillPoints, armorDamage, shieldCharge, isIncapacitated].
pub(super) async fn ship_state(c: &Client, ship_id: i64) -> Result<Value, CallError> {
    let owner = c.session.int("charid").ok_or_else(|| crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    crate::services::effects::online::reconcile_ship(&c.db, &c.world, &c.sde, owner, ship_id, |n| crate::services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await?;
    ship_state_core(c, ship_id).await
}

/// Assemble reconciled ship state without acquiring the lifecycle lock.
pub(crate) async fn ship_state_core(c: &Client, ship_id: i64) -> Result<Value, CallError> {
    let owner = c.session.int("charid").ok_or_else(|| crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
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
pub(super) async fn ship_info(c: &Client) -> Result<Vec<(Value, Value)>, CallError> {
    let Some(ship_id) = c.session.int("shipid") else { return Ok(vec![]) };
    let owner = c.session.int("charid").ok_or_else(|| crate::services::notify_error(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    crate::services::effects::online::reconcile_ship(&c.db, &c.world, &c.sde, owner, ship_id, |n| crate::services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await?;
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
    let overloaded = crate::services::effects::heat::overloaded_modules();
    let cap_max = fit.value(fit::Key::Ship, ATTR_CAPACITOR_CAPACITY);
    let cap = if let Some(system) = system {
        combat::groups_health::capacitor_snapshot(&c.db, &c.world, system, ship_id).await?.map_or(cap_max, |snapshot| snapshot.level)
    } else {
        cap_max
    };
    let (shield, armor_damage, hull_damage) = if let Some(system) = system {
        c.world.with(system, move |s| {
            if !s.pilot_ball(char_id).is_some_and(|ball| ball.id == ship_id) { return None; }
            s.ship_condition(ship_id)
        }).await.flatten().ok_or_else(|| CallError::Internal(anyhow::anyhow!("active ship condition is unavailable")))?
    } else {
        combat::prepare_condition_with_fit(&c.db, ship_id, &fit).await?.scalars()
    };
    for item in items {
        let mut attrs = type_attributes(&c.sde, item.type_id);
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
                if c.sde.dogma.effect(effect.effect_id).is_some_and(|row| row.effect_category == Some(crate::services::effects::heat::rpc::OVERLOAD_CATEGORY)) {
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
        let key = combat::targeting_charges::charge_key(ship_id, flag, type_id);
        out.push((key, charge_info(&c.sde, ship_id, flag, type_id, qty)));
    }
    Ok(out)
}
