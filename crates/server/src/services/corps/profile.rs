//! Corporation profile mutations, validated and serialized with current authority.
use super::*;
use crate::dao::corporations::dispatch as dao;
/// `appConst.py:314` corpLogoChangeCost; `corp_dlg_editcorpdetails.py:356` confirms it.
const LOGO_CHANGE_COST: i128 = 100;


pub(super) fn tax(value: Option<&Value>) -> Result<f64, CallError> {
    let rate = value.and_then(Value::as_float).ok_or_else(|| notify_error(messages::DIRECTOR_REQUIRED_TO_EDIT_CORP_INFO))?;
    if !rate.is_finite() || !(0.0..=1.0).contains(&rate) { return Err(notify_error(messages::DIRECTOR_REQUIRED_TO_EDIT_CORP_INFO)); }
    Ok(rate)
}
pub(super) fn logo(values: &[Value]) -> Result<[Option<i32>; 7], CallError> {
    let invalid = || notify_error(messages::DIRECTOR_REQUIRED_TO_EDIT_CORP_INFO);
    if values.len() != 7 { return Err(invalid()); }
    let mut result = [None; 7];
    for (i, value) in values.iter().enumerate() {
        result[i] = match value { Value::None => None, Value::Int(n) => Some(i32::try_from(*n).map_err(|_| invalid())?), _ => return Err(invalid()) };
    }
    Ok(result)
}


fn change(old: &registry::Corporation, new: &registry::Corporation) -> Value {
    let mut fields: Vec<(Value, Value)> = Vec::new();
    let mut add = |name: &str, before: Value, after: Value| {
        if before != after { fields.push((Value::str(name), Value::tuple([before, after]))); }
    };
    add("description", Value::unicode(&old.description), Value::unicode(&new.description));
    add("url", Value::unicode(&old.url), Value::unicode(&new.url));
    add("taxRate", Value::Float(old.tax_rate), Value::Float(new.tax_rate));
    add("loyaltyPointTaxRate", Value::Float(old.lp_tax_rate), Value::Float(new.lp_tax_rate));
    add("isRecruiting", Value::Bool(old.is_recruiting), Value::Bool(new.is_recruiting));
    for (i, name) in ["shape1", "shape2", "shape3", "color1", "color2", "color3", "typeface"].iter().enumerate() {
        add(name, old.logo[i].map(Value::Int).unwrap_or(Value::None), new.logo[i].map(Value::Int).unwrap_or(Value::None));
    }
    let before = old.division_names.as_deref().unwrap_or(&[]);
    let after = new.division_names.as_deref().unwrap_or(&[]);
    for (i, name) in ["division1", "division2", "division3", "division4", "division5", "division6", "division7", "walletDivision1", "walletDivision2", "walletDivision3", "walletDivision4", "walletDivision5", "walletDivision6", "walletDivision7"].iter().enumerate() {
        let value = |rows: &[Option<String>]| rows.get(i).and_then(Option::as_ref).filter(|s| !s.is_empty()).map(Value::unicode).unwrap_or(Value::None);
        add(name, value(before), value(after));
    }
    Value::Dict(fields)
}

#[derive(Clone)]
enum Update { Profile { description: String, url: String, tax: f64, recruiting: bool, lp_tax: f64 }, Divisions(Vec<Option<String>>), Logo([Option<i32>; 7]) }

pub(super) async fn update(c: &Client, call: &Call<'_>, corp: i64, me: i64) -> CallResult {
    let invalid = || notify_error(messages::DIRECTOR_REQUIRED_TO_EDIT_CORP_INFO);
    let update = match call.method {
        "UpdateCorporation" => {
            if call.args.len() != 5 { return Err(invalid()); }
            let description = call.args[0].as_text().ok_or_else(invalid)?.into_owned();
            let url = call.args[1].as_text().ok_or_else(invalid)?.into_owned();
            let recruiting = match call.args[3] { Value::Bool(value) => value, Value::Int(0) => false, Value::Int(1) => true, _ => return Err(invalid()) };
            Update::Profile { description, url, tax: tax(call.args.get(2))?, recruiting, lp_tax: tax(call.args.get(4))? }
        }
        "UpdateDivisionNames" => {
            if call.args.len() != CORP_DIVISION_NAME_COUNT { return Err(invalid()); }
            let mut names = Vec::with_capacity(CORP_DIVISION_NAME_COUNT);
            for value in call.args { names.push(match value { Value::None => None, _ => Some(value.as_text().ok_or_else(invalid)?.into_owned()) }); }
            Update::Divisions(names)
        }
        "UpdateLogo" => Update::Logo(logo(call.args)?),
        _ => return Err(invalid()),
    };
    let _economy = if matches!(&update, Update::Logo(_)) { Some(super::super::market::lock().await) } else { None };
    c.db.transaction(
        |tx, outbox: &mut Outbox| {
            let update = update.clone();
            Box::pin(async move {
                if !registry::lock_admission(tx, &[], &[corp], &[me]).await.map_err(db_err)? { return Err(invalid()); }
                let old = registry::corporation(tx, corp).await.map_err(db_err)?.filter(|row| !row.deleted).ok_or_else(invalid)?;
                let permitted = if matches!(&update, Update::Divisions(_)) { permissions::is_active_ceo(tx, me, corp).await? }
                    else { permissions::has(tx, me, corp, ROLE_DIRECTOR).await? };
                if !permitted {
                    return Err(if matches!(&update, Update::Divisions(_)) { notify_error(messages::corporations::DIVISION_CEO_REQUIRED) } else { invalid() });
                }
                if let Update::Logo(values) = &update {
                    if old.logo == values.map(|value| value.map(i64::from)) { return Ok(Value::None); }
                    super::super::wallets::corp_debit(tx, outbox, corp, ACCOUNT_CASH, crate::isk::IskAmount::from_whole_isk(LOGO_CHANGE_COST).ok_or(crate::isk::AmountError)?).await?;
                }
                let changed = match update {
                    Update::Profile { description, url, tax, recruiting, lp_tax } => dao::update_corporation(tx, corp, &description, &url, tax, recruiting, lp_tax).await,
                    Update::Divisions(names) => dao::set_division_names(tx, corp, &names).await,
                    Update::Logo(values) => dao::update_logo(tx, corp, [values[0], values[1], values[2]], [values[3], values[4], values[5]], values[6]).await,
                }.map_err(db_err)?;
                if changed != 1 { return Err(invalid()); }
                let new = registry::corporation(tx, corp).await.map_err(db_err)?.ok_or_else(invalid)?;
                let delta = change(&old, &new);
                if matches!(&delta, Value::Dict(fields) if !fields.is_empty()) {
                    for member in crate::dao::characters::members::active_ids(tx, corp).await.map_err(db_err)? {
                        outbox.notify_pilot(member, "OnCorporationChanged", vec![Value::Int(corp), delta.clone()]);
                    }
                }
                Ok(Value::None)
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, &c.world) },
    ).await
}
