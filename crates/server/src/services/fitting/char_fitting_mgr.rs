//! Saved personal fittings, mirroring `charFittingMgr` and `fittingSvc.py`.
//! Fitting triples use strict signed `int4` type and flag fields and positive
//! signed `int8` quantities; a save batch is preflighted and committed in one transaction.
//! Wire `fitData` quantities are projected to the client display limit;
//! saved `fit_qty` stays exact.
//! Historical rows are not repaired, and fittings owned by other characters are unsupported.
use std::sync::Arc;
use crate::services::{db_err, keyval, notify_error, value_field, value_id_list, Call, CallError, CallResult};
use infinity_core::messages;
use crate::dao::items::fitting as fitting_dao;
use crate::client::Client;
use marshal::Value;

fn fitting_value(r: &fitting_dao::Fitting) -> (Value, Value) {
    let id = r.fitting_id;
    let (types, flags, qty) = (&r.fit_types, &r.fit_flags, &r.fit_qty);
    let fit_data = types.iter().zip(flags).zip(qty).map(|((t, f), q)| Value::tuple([Value::Int(*t as i64), Value::Int(*f as i64), Value::Int(crate::services::dogma::info::cap_client_quantity(*q))]));
    let fitting = keyval([
        ("fittingID", Value::Int(id)),
        ("ownerID", Value::Int(r.owner_id)),
        ("name", Value::unicode(r.name.clone())),
        ("description", Value::unicode(r.description.clone())),
        ("shipTypeID", Value::Int(r.ship_type_id as i64)),
        ("fitData", Value::list(fit_data)),
    ]);
    (Value::Int(id), fitting)
}

struct ParsedFitData {
    types: Vec<i32>,
    flags: Vec<i32>,
    qty: Vec<i64>,
}

struct PreparedFitting {
    name: String,
    description: String,
    ship_type: i32,
    data: ParsedFitData,
}

fn invalid_saved_data() -> CallError {
    notify_error(messages::fitting::INVALID_SAVED_DATA)
}

fn parse_fit_data(value: Option<&Value>) -> Result<ParsedFitData, CallError> {
    let rows = match value {
        Some(Value::List(rows)) | Some(Value::Tuple(rows)) => rows,
        _ => return Err(invalid_saved_data()),
    };
    let mut parsed = ParsedFitData {
        types: Vec::with_capacity(rows.len()),
        flags: Vec::with_capacity(rows.len()),
        qty: Vec::with_capacity(rows.len()),
    };
    for row in rows {
        let Some(values) = (match row {
            Value::List(values) | Value::Tuple(values) => Some(values),
            _ => None,
        }) else {
            return Err(invalid_saved_data());
        };
        if values.len() != 3 {
            return Err(invalid_saved_data());
        }
        let [Value::Int(t), Value::Int(f), Value::Int(q)] = values.as_slice() else {
            return Err(invalid_saved_data());
        };
        let (Ok(t), Ok(f)) = (i32::try_from(*t), i32::try_from(*f)) else {
            return Err(invalid_saved_data());
        };
        if *q <= 0 {
            return Err(invalid_saved_data());
        }
        parsed.types.push(t);
        parsed.flags.push(f);
        parsed.qty.push(*q);
    }
    Ok(parsed)
}

fn prepare_fittings(fits: &[(Value, Value)]) -> Result<Vec<PreparedFitting>, CallError> {
    fits.iter().map(|(_, fit)| {
        let name = value_field(fit, "name").and_then(|v| v.as_text()).map(|text| text.into_owned()).unwrap_or_default();
        let description = value_field(fit, "description").and_then(|v| v.as_text()).map(|text| text.into_owned()).unwrap_or_default();
        let ship_type = value_field(fit, "shipTypeID").and_then(Value::as_int).unwrap_or(0) as i32;
        Ok(PreparedFitting { name, description, ship_type, data: parse_fit_data(value_field(fit, "fitData"))? })
    }).collect()
}

async fn save_prepared(db: &crate::db::Db, owner: i64, inputs: Vec<PreparedFitting>) -> Result<Vec<i64>, CallError> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let inputs = Arc::new(inputs);
    db.transaction(move |tx, _: &mut ()| {
        let inputs = Arc::clone(&inputs);
        Box::pin(async move {
            let mut ids = Vec::with_capacity(inputs.len());
            for input in inputs.iter() {
                ids.push(fitting_dao::save(tx, owner, &input.name, &input.description, input.ship_type, &input.data.types, &input.data.flags, &input.data.qty).await.map_err(db_err)?);
            }
            Ok(ids)
        })
    }, |ids, ()| async move { Ok(ids) }).await
}

async fn fittings(c: &mut Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let owner = call.args.first().and_then(|v| v.as_int()).unwrap_or(char_id);
    if owner != char_id {
        return Ok(match call.method {
            "GetFittings" => Value::Dict(vec![]),
            _ => Value::None,
        });
    }
    let db = &c.db;
    Ok(match call.method {
        // {fittingID: KeyVal(fittingID, ownerID, name, description, shipTypeID, fitData)}
        "GetFittings" => {
            let rows = fitting_dao::fittings(db, owner).await.map_err(db_err)?;
            Value::Dict(rows.iter().map(fitting_value).collect())
        }
        // SaveManyFittings(ownerID, {tempID: fitting}) -> [KeyVal(tempFittingID, realFittingID)]
        "SaveManyFittings" => {
            let fits = match call.args.get(1) {
                Some(Value::Dict(fits)) => fits,
                _ => return Err(invalid_saved_data()),
            };
            let prepared = prepare_fittings(fits)?;
            let ids = save_prepared(db, owner, prepared).await?;
            Value::List(fits.iter().zip(ids).map(|((temp, _), id)| keyval([
                ("tempFittingID", temp.clone()),
                ("realFittingID", Value::Int(id)),
            ])).collect())
        }
        "DeleteFitting" => {
            let id = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            fitting_dao::delete(db, owner, id).await.map_err(db_err)?;
            Value::None
        }
        "DeleteManyFittings" => {
            let ids = value_id_list(call.args.get(1));
            fitting_dao::delete_many(db, owner, &ids).await.map_err(db_err)?;
            Value::None
        }
        _ => return Err(CallError::Internal(anyhow::anyhow!("charFittingMgr::{} not handled", call.method))),
    })
}

/// UpdateNameAndDescription(fittingID, ownerID, name, description)
async fn rename_fitting(c: &Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let text = |i: usize| call.args.get(i).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    fitting_dao::rename(&c.db, id, char_id, &text(2), &text(3)).await.map_err(db_err)?;
    Ok(Value::None)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match call.method { "UpdateNameAndDescription" => rename_fitting(c, call).await, _ => fittings(c, call).await })
}
#[cfg(test)]
mod tests;
