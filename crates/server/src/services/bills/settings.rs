//! billMgr automatic payment settings: `{ownerID: {billTypeID: bool, 'divisionID':
//! accountKey}}`. The readable owners are the caller's live corporation and the
//! alliance it is the current executor of; every submitted owner entry is decoded
//! and authorized before any write. A corporation's division lives on its own row,
//! an alliance's on the alliance row — never on the executor's.

use super::{is_accountant, SETTLED_BILL_TYPES};
use crate::dao::{bills as bills_dao, corporations::registry};
use crate::db::Db;
use crate::services::{corpwallet, db_err, notify_error, CallError, CallResult};
use infinity_core::consts::ACCOUNT_CASH;
use infinity_core::messages;
use marshal::Value;

/// The owners the caller may view or change: its live corporation plus the
/// alliance that corporation currently executes.
async fn owners(db: &Db, me: i64) -> Result<Vec<bills_dao::BillOwner>, CallError> {
    let mut result = Vec::new();
    let authority = crate::services::corps::permissions::load(db, me).await?
        .filter(|authority| authority.live)
        .ok_or_else(|| notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS))?;
    if !(crate::services::corps::permissions::allows(&authority, authority.corporation,
        infinity_core::consts::ROLE_ACCOUNTANT | infinity_core::consts::ROLE_JUNIOR_ACCOUNTANT | infinity_core::consts::ROLE_DIRECTOR)
        || crate::services::corps::permissions::active_ceo(&authority, me)) {
        return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS));
    }
    if crate::services::corps::is_player_corp(authority.corporation) {
        result.push(bills_dao::BillOwner::Corporation(authority.corporation));
    }
    if let Some(alliance) = crate::dao::alliances::corporation_alliance(db, authority.corporation).await.map_err(db_err)? {
        if crate::dao::alliances::executor_corp(db, alliance).await.map_err(db_err)? == Some(authority.corporation) {
            result.push(bills_dao::BillOwner::Alliance(alliance));
        }
    }
    Ok(result)
}

/// `GetAutomaticPaySettings`: the caller's corporation (and its executor alliance)
/// settings rows plus each owner's own division.
pub(super) async fn read(db: &Db, me: i64) -> CallResult {
    let mut result = Vec::new();
    for owner in owners(db, me).await? {
        let rows = bills_dao::settings(db, owner.id()).await.map_err(db_err)?;
        let division = bills_dao::division(db, owner).await.map_err(db_err)?.unwrap_or(ACCOUNT_CASH);
        let mut settings: Vec<(Value, Value)> = rows.iter().map(|r| (Value::Int(r.bill_type as i64), Value::Bool(r.enabled))).collect();
        settings.push((Value::str("divisionID"), Value::Int(division as i64)));
        result.push((Value::Int(owner.id()), Value::Dict(settings)));
    }
    Ok(Value::Dict(result))
}

/// `SendAutomaticPaySettings`: validate every owner entry first, then save
/// corporation and alliance settings to their own rows in one transaction.
pub(super) async fn save(db: &Db, me: i64, arg: Option<&Value>) -> Result<(), CallError> {
    let allowed: std::collections::HashMap<i64, bills_dao::BillOwner> = owners(db, me).await?
        .into_iter().map(|owner| (owner.id(), owner)).collect();
    let mut changes: Vec<(bills_dao::BillOwner, Vec<(i32, bool)>, Option<i32>)> = Vec::new();
    match arg {
        Some(Value::Dict(owners)) => {
            for (owner, settings) in owners {
                let Some(id) = owner.as_int() else {
                    return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS));
                };
                let Some(owner) = allowed.get(&id).copied() else {
                    return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS));
                };
                let Value::Dict(settings) = settings else {
                    return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS));
                };
                let mut toggles = Vec::new();
                let mut division = None;
                for (key, value) in settings {
                    if let Some(kind) = key.as_int() {
                        let kind = i32::try_from(kind).map_err(|_| notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS))?;
                        if !SETTLED_BILL_TYPES.contains(&kind) {
                            return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS));
                        }
                        let enabled = match value {
                            Value::Bool(enabled) => *enabled,
                            _ => return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS)),
                        };
                        toggles.push((kind, enabled));
                    } else if key.as_text().as_deref() == Some("divisionID") {
                        let key = value.as_int().and_then(|v| i32::try_from(v).ok())
                            .filter(|key| corpwallet::valid_key(*key))
                            .ok_or_else(|| notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS))?;
                        division = Some(key);
                    } else {
                        return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS));
                    }
                }
                if division.is_none() {
                    return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS));
                }
                changes.push((owner, toggles, division));
            }
        }
        Some(_) => return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS)),
        None => return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS)),
    }
    if changes.is_empty() {
        return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS));
    }
    let _trading = crate::services::market::lock().await;
    let mut locked_alliances = Vec::new();
    let mut locked_corps = Vec::new();
    let mut executors = std::collections::HashMap::new();
    for (owner, _, _) in &changes {
        match *owner {
            bills_dao::BillOwner::Corporation(corp) => locked_corps.push(corp),
            bills_dao::BillOwner::Alliance(alliance) => {
                locked_alliances.push(alliance);
                let executor = crate::dao::alliances::executor_corp(db, alliance).await.map_err(db_err)?
                    .ok_or_else(|| notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS))?;
                executors.insert(alliance, executor);
                locked_corps.push(executor);
            }
        }
    }
    locked_alliances.sort_unstable();
    locked_alliances.dedup();
    locked_corps.sort_unstable();
    locked_corps.dedup();
    db.transaction(
        |tx, _: &mut ()| {
            let changes = changes.clone();
            let locked_alliances = locked_alliances.clone();
            let locked_corps = locked_corps.clone();
            let executors = executors.clone();
            Box::pin(async move {
                if !registry::lock_admission(tx, &locked_alliances, &locked_corps, &[me]).await.map_err(db_err)? {
                    return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS));
                }
                let authority = crate::services::corps::permissions::load(tx, me).await?
                    .ok_or_else(|| notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS))?;
                for (owner, _, division) in &changes {
                    let corp = match *owner {
                        bills_dao::BillOwner::Corporation(corp) => corp,
                        bills_dao::BillOwner::Alliance(alliance) => crate::dao::alliances::executor_corp(tx, alliance).await.map_err(db_err)?
                            .filter(|corp| executors.get(&alliance) == Some(corp))
                            .ok_or_else(|| notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS))?,
                    };
                    let key = division.ok_or_else(|| notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS))?;
                    if !is_accountant(tx, me, corp).await? || !corpwallet::can_take(&authority, corp, key) {
                        return Err(notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS));
                    }
                }
                for (owner, toggles, division) in changes {
                    for (kind, enabled) in toggles {
                        bills_dao::set_setting(tx, owner.id(), kind, enabled).await.map_err(db_err)?;
                    }
                    let key = division.ok_or_else(|| notify_error(messages::ACCOUNTANT_REQUIRED_FOR_AUTOPAY_SETTINGS))?;
                    bills_dao::set_division(tx, owner, key).await.map_err(db_err)?;
                }
                Ok(())
            })
        },
        |(), ()| async move { Ok::<_, CallError>(()) },
    ).await
}

