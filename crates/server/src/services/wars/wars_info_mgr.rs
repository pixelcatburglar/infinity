//! Public war history, paging and structure queries for `warsInfoMgr`.
//! Mirrors `eve/client/script/ui/shared/neocom/corporation/war/warHistoryCont.py` and structure queries.
//! Reads `game.wars`; mutual wars have no allies or headquarters.

use infinity_core::messages;
use crate::{client::Client, dao::wars};
use crate::services::{Call, CallError, CallResult, db_err, notify_error};
use marshal::Value;
use super::wire;

/// `warsInfoMgr.GetTop50`, client war leaderboard page size.
const TOP_PAGE_SIZE: i64 = 50;

fn invalid() -> CallError { notify_error(messages::wars::INVALID) }
fn id(value: &Value) -> Result<i64, CallError> {
    match value { Value::Int(id) if *id > 0 => Ok(*id), _ => Err(invalid()) }
}
fn ids(value: &Value) -> Result<Vec<i64>, CallError> {
    let mut result: Vec<_> = value.items().ok_or_else(invalid)?.iter().map(id).collect::<Result<_, _>>()?;
    result.sort_unstable();
    result.dedup();
    Ok(result)
}

fn validate_arity(method: &str, count: usize) -> Option<Result<(), CallError>> {
    let expected = match method {
        "GetWarsByOwnerID" | "GetWarsByOwners" | "GetTop50" | "GetWarsForStructure" | "GetPublicWarInfo" => 1,
        "GetWarsRequiringAssistance" => 0,
        _ => return None,
    };
    Some(if count == expected { Ok(()) } else { Err(invalid()) })
}

fn wars_by_owners(owners: &[i64], rows: &[wars::War]) -> Value {
    let mut groups: Vec<Vec<(Value, Value)>> = (0..owners.len()).map(|_| Vec::new()).collect();
    for row in rows {
        for owner in [row.declared_by, row.against] {
            if let Ok(index) = owners.binary_search(&owner) {
                groups[index].push((Value::Int(row.war_id), wire::war(row)));
            }
        }
    }
    Value::Dict(owners.iter().zip(groups).map(|(&owner, group)| (Value::Int(owner), Value::Dict(group))).collect())
}

async fn query(c: &Client, call: &Call<'_>) -> CallResult {
    match call.method {
        "GetWarsByOwnerID" => {
            let owner = id(call.args.first().ok_or_else(invalid)?)?;
            Ok(wire::war_list(&wars::by_owner(&c.db, owner).await.map_err(db_err)?))
        }
        "GetWarsByOwners" => {
            let owners = ids(call.args.first().ok_or_else(invalid)?)?;
            let rows = if owners.is_empty() { Vec::new() } else { wars::by_owners(&c.db, &owners).await.map_err(db_err)? };
            Ok(wars_by_owners(&owners, &rows))
        }
        "GetTop50" => {
            let before = match call.args.first() {
                Some(Value::None) | Some(Value::Int(0)) => None,
                Some(v) => Some(id(v)?),
                None => return Err(invalid()),
            };
            Ok(wire::wars(&wars::top(&c.db, before, TOP_PAGE_SIZE).await.map_err(db_err)?))
        }
        "GetWarsRequiringAssistance" => Ok(Value::Dict(vec![])),
        "GetWarsForStructure" => {
            id(call.args.first().ok_or_else(invalid)?)?;
            Ok(Value::list([]))
        }
        "GetPublicWarInfo" => {
            let ids = ids(call.args.first().ok_or_else(invalid)?)?;
            let rows = if ids.is_empty() { Vec::new() } else { wars::by_ids(&c.db, &ids).await.map_err(db_err)? };
            Ok(wire::war_list(&rows))
        }
        _ => unreachable!(),
    }
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if let Err(error) = validate_arity(call.method, call.args.len())? { return Some(Err(error)); }
    Some(query(c, call).await)
}

#[cfg(test)]
mod tests;
