//! Capability-closed Lua adaptation planning using captured immutable catalogue values.
//! Plans are rechecked against the request and checked integer LP arithmetic before
//! any transaction; Lua never receives an executor or mutable game state.

use std::{sync::Arc, time::Instant};
use mlua::{Table, UserData, UserDataFields, UserDataMethods, Value};
use infinity_core::{consts::{GROUP_ADAPTION_CHIPS, dogma::{ATTR_REFIT_TICKET_QUANTITY, ATTR_REFIT_TICKET_TYPE}}};
use crate::{lua::content::{adaption::{self, Definitions}, OwnedAdmission, PinnedPolicy}, sde::Sde, services::{CallError, db_err}};

/// Resolve a physical injector using only immutable SDE attributes.
pub(crate) fn conversion(sde: &Sde, definitions: &Definitions, source: i32, quantity: u32) -> Option<(i32, u32)> {
    let ty = sde.types.get(source)?;
    if ty.group_id != Some(GROUP_ADAPTION_CHIPS) || quantity == 0 { return None; }
    let exact = |value: f64, max: f64| {
        (value.is_finite() && value > 0.0 && value <= max && value.fract() == 0.0).then_some(value)
    };
    let target = exact(ty.attribute(ATTR_REFIT_TICKET_TYPE)?, f64::from(i32::MAX))? as i32;
    let units = exact(ty.attribute(ATTR_REFIT_TICKET_QUANTITY)?, f64::from(u32::MAX))? as u32;
    if definitions.chip_types().binary_search(&target).is_err() { return None; }
    Some((target, quantity.checked_mul(units)?))
}

#[derive(Clone)]
pub(crate) struct Input {
    pub definitions: Arc<Definitions>,
    pub item_type: Option<i32>,
    pub quantity: u32,
    pub credit: Option<(i32, u32)>,
    pub issuer: i64,
    pub chips: Arc<Vec<(i32, u32)>>,
}
impl UserData for Input {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("item_type", |_, this| Ok(this.item_type));
        fields.add_field_method_get("quantity", |_, this| Ok(this.quantity));
        fields.add_field_method_get("injectable", |_, this| Ok(this.credit.is_some()));
        fields.add_field_method_get("credit_type", |_, this| Ok(this.credit.map(|value| value.0)));
        fields.add_field_method_get("credit_quantity", |_, this| Ok(this.credit.map(|value| value.1)));
        fields.add_field_method_get("issuer_allowed", |_, this| Ok(this.definitions.issuer(this.issuer)));
        fields.add_field_method_get("count", |_, this| Ok(this.chips.len()));
    }
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("entry", |lua, this, index: usize| {
            let Some(&(type_id, quantity)) = index.checked_sub(1).and_then(|i| this.chips.get(i)) else { return Ok(None) };
            let row = lua.create_table()?;
            row.set("type_id", type_id)?;
            row.set("quantity", quantity)?;
            row.set("quote", this.definitions.quote(type_id, this.issuer))?;
            Ok(Some(row))
        });
    }
}

enum PreparedAction {
    Inject { type_id: i32, quantity: u32, credit_type: i32, credit_quantity: u32 },
    Exchange { issuer: i64, chips: Arc<Vec<(i32, u32)>> },
}

pub(crate) struct PreparedPlan {
    pin: PinnedPolicy,
    definitions: Arc<Definitions>,
    action: PreparedAction,
}

pub(super) enum Action {
    Inject { type_id: i32, quantity: u32, credit_type: i32, credit_quantity: u32 },
    Exchange { issuer: i64, chips: Arc<Vec<(i32, u32)>>, total: i64 },
}

pub(crate) struct AdmittedPlan {
    admission: Arc<OwnedAdmission>,
    action: Action,
}

impl PreparedPlan {
    pub(crate) fn admit(self, admission: Arc<OwnedAdmission>) -> Result<Option<AdmittedPlan>, CallError> {
        let policy = admission.policy();
        let definitions = policy.adaption().ok_or_else(invalid)?;
        if !self.pin.same_package(policy) || !Arc::ptr_eq(&self.definitions, definitions) {
            return Err(super::operations::refusal(&self.definitions, adaption::CONFLICT));
        }
        let action = match self.action {
            PreparedAction::Inject { type_id, quantity, credit_type, credit_quantity } => Action::Inject { type_id, quantity, credit_type, credit_quantity },
            PreparedAction::Exchange { issuer, chips } => {
                let mut total = 0i64;
                for &(type_id, quantity) in chips.iter() {
                    let quote = definitions.quote(type_id, issuer).ok_or_else(invalid)?;
                    total = quote.checked_mul(i64::from(quantity)).and_then(|amount| total.checked_add(amount))
                        .ok_or_else(|| super::operations::refusal(definitions, adaption::OVERFLOW))?;
                }
                if total <= 0 { return Ok(None); }
                Action::Exchange { issuer, chips, total }
            }
        };
        Ok(Some(AdmittedPlan { admission, action }))
    }
}

impl AdmittedPlan {
    pub(super) fn policy(&self) -> &PinnedPolicy { self.admission.policy() }
    pub(super) fn action(&self) -> &Action { &self.action }
}

fn invalid() -> CallError { db_err(anyhow::anyhow!("invalid adaptation plan")) }
fn fields(table: &Table, keys: &[&str]) -> Result<(), CallError> {
    let mut count = 0;
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, _) = pair.map_err(db_err)?;
        let Value::String(key) = key else { return Err(invalid()) };
        let key = key.to_str().map_err(db_err)?;
        if !keys.iter().any(|expected| *expected == &*key) { return Err(invalid()); }
        count += 1;
    }
    if count != keys.len() { return Err(invalid()); }
    Ok(())
}
fn integer(table: &Table, key: &str) -> Result<i64, CallError> {
    match table.raw_get::<Value>(key).map_err(db_err)? { Value::Integer(value) => Ok(value), _ => Err(invalid()) }
}
fn row(table: &Table, quoted: bool) -> Result<(i32, u32, i64), CallError> {
    fields(table, if quoted { &["type_id", "quantity", "quote"] } else { &["type_id", "quantity"] })?;
    let type_id = i32::try_from(integer(table, "type_id")?).ok().filter(|id| *id > 0).ok_or_else(invalid)?;
    let quantity = u32::try_from(integer(table, "quantity")?).ok().filter(|quantity| *quantity > 0).ok_or_else(invalid)?;
    let quote = if quoted { integer(table, "quote")? } else { 0 };
    if quote < 0 { return Err(invalid()); }
    Ok((type_id, quantity, quote))
}

pub(crate) fn plan(pin: &PinnedPolicy, input: Input, deadline: Instant) -> Result<Option<PreparedPlan>, CallError> {
    if !pin.adaption().is_some_and(|definitions| Arc::ptr_eq(definitions, &input.definitions)) { return Err(invalid()); }
    if input.item_type.is_some_and(|id| id <= 0) || (input.item_type.is_some() && input.quantity == 0) { return Err(invalid()); }
    if input.item_type.is_none() && (input.chips.is_empty() || input.chips.iter().any(|&(id, quantity)| id <= 0 || quantity == 0)
        || input.chips.windows(2).any(|pair| pair[0].0 >= pair[1].0)) { return Err(invalid()); }
    let operation = if input.item_type.is_some() { "InjectChips" } else { "ChipExchange" };
    let result = pin.run_adaption(operation, input.clone(), deadline)?;
    if matches!(result, Value::Nil | Value::Boolean(false)) { return Ok(None); }
    let Value::Table(table) = result else { return Err(invalid()) };
    if let Some(type_id) = input.item_type {
        let (credit_type, credit_quantity, _) = row(&table, false)?;
        if input.credit != Some((credit_type, credit_quantity)) { return Err(invalid()); }
        return Ok(Some(PreparedPlan { pin: pin.clone(), definitions: input.definitions, action: PreparedAction::Inject { type_id, quantity: input.quantity, credit_type, credit_quantity } }));
    }
    if !input.definitions.issuer(input.issuer) { return Err(invalid()); }
    let mut count = 0;
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, value) = pair.map_err(db_err)?;
        let Value::Integer(index) = key else { return Err(invalid()) };
        if index < 1 || usize::try_from(index).ok().is_none_or(|index| index > input.chips.len()) || !matches!(value, Value::Table(_)) { return Err(invalid()); }
        count += 1;
    }
    if count != input.chips.len() { return Err(invalid()); }
    for (index, &(type_id, quantity)) in input.chips.iter().enumerate() {
        let Value::Table(entry) = table.raw_get::<Value>(index + 1).map_err(db_err)? else { return Err(invalid()) };
        let (actual_type, actual_quantity, quote) = row(&entry, true)?;
        if actual_type != type_id || actual_quantity != quantity || input.definitions.quote(type_id, input.issuer) != Some(quote) { return Err(invalid()); }
    }
    Ok(Some(PreparedPlan { pin: pin.clone(), definitions: input.definitions, action: PreparedAction::Exchange { issuer: input.issuer, chips: input.chips } }))
}
