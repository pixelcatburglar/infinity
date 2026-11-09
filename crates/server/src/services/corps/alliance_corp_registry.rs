//! Alliance creation and applications through `corpRegistry`, mirroring
//! `eve/client/script/ui/services/alliances` and `eve/client/script/ui/services/corporation`.
//! Shared alliance mechanics in `services::alliances` use `game.alliances`,
//! `game.corporations` and `game.alliance_applications` through the DAO.
//! Wars, bills, ordinary capital changes and prime time are not supported.
use super::*;
use crate::dao::alliances as dao;
use crate::services::alliances::{self, admission, queue_application_changed, application_header, application_line, application_line_of, indexed_rowset, set_alliance, APP_NEW, APP_REJECTED};
use crate::services::{db_err, keyval, notify_error, trimmed_arg_text, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use infinity_core::consts::{ACCOUNT_CASH, ORG_NAME_MAX_LENGTH, ORG_NAME_MIN_LENGTH, ORG_SHORT_NAME_MAX_LENGTH, ORG_SHORT_NAME_MIN_LENGTH};
use crate::db::Db;
use crate::db::outbox::dispatch_world;
use infinity_core::messages;
use marshal::Value;
use crate::services::identity::Identity;
use crate::dao::corporations::registry;
use tracing::info;
/// appConst.allianceCreationCost
const CREATION_COST: i128 = 1_000_000_000;
/// The CEO of a player corporation that isn't in an alliance yet.
async fn unallied_ceo_corp(db: &Db, me: i64) -> Result<i64, CallError> {
    let (corp, _) = membership(db, me).await?;
    if !is_player_corp(corp) || !permissions::is_active_ceo(db, me, corp).await? {
        return Err(notify_error(messages::PLAYER_CORP_CEO_REQUIRED));
    }
    let allied = dao::corporation_alliance(db, corp).await.map_err(db_err)?;
    if allied.is_some() {
        return Err(notify_error(messages::CORP_ALREADY_IN_ALLIANCE));
    }
    Ok(corp)
}
/// `CreateAlliance(allianceName, shortName, description, url)` on the founder's corpRegistry.
async fn create(c: &Client, call: &Call<'_>) -> CallResult {
    let [Value::Unicode(name), Value::Unicode(short_name), Value::Unicode(description), Value::Unicode(url)] = call.args else {
        return Err(notify_error(messages::ALLIANCE_NAME_LENGTH_INVALID));
    };
    if [name, short_name, description, url].iter().any(|value| value.contains('\0')) {
        return Err(notify_error(messages::ALLIANCE_NAME_LENGTH_INVALID));
    }
    let (name, short_name, description, url) = (name.trim().to_owned(), short_name.trim().to_uppercase(), description.trim().to_owned(), url.trim().to_owned());
    let identity = Identity::of_client(c);
    let corp = bound_corp(call).ok_or_else(|| notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION))?;
    if admission::actor_corp(&c.db, &identity).await? != corp { return Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION)); }
    create_core(c, name, short_name, description, url, false).await.map(Value::Int)
}

async fn create_core(c: &Client, name: String, short_name: String, description: String, url: String, gm: bool) -> Result<i64, CallError> {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let corp = unallied_ceo_corp(db, me).await?;
    if !(ORG_NAME_MIN_LENGTH..=ORG_NAME_MAX_LENGTH).contains(&name.chars().count()) {
        return Err(notify_error(messages::ALLIANCE_NAME_LENGTH_INVALID));
    }
    if !(ORG_SHORT_NAME_MIN_LENGTH..=ORG_SHORT_NAME_MAX_LENGTH).contains(&short_name.chars().count()) || !short_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        return Err(notify_error(messages::ALLIANCE_SHORT_NAME_INVALID));
    }
    let taken = dao::name_taken(db, &name, &short_name).await.map_err(db_err)?;
    if taken {
        return Err(notify_error(messages::ALLIANCE_NAME_OR_SHORT_NAME_TAKEN));
    }
    let now = filetime_now();
    let prepared = crate::services::sovereignty::governance::prepare(db, corp, now).await?;
    if !matches!(prepared, crate::services::sovereignty::governance::PreparedAllianceChange::NoOwnershipChange) {
        return Err(notify_error(messages::CORP_ALREADY_IN_ALLIANCE));
    }
    let _trading = super::super::market::lock().await;
    let admitted = std::sync::Arc::new(crate::services::sovereignty::governance::admit(world, prepared, Some(me))?);
    let identity = Identity::of_client(c);
    let mut applications: Vec<i64> = dao::applications(db, dao::ApplicationFilter::Corporation(corp)).await.map_err(db_err)?.into_iter().map(|a| a.alliance).collect();
    applications.sort_unstable(); applications.dedup();
    let id = db.transaction(
        |tx, outbox| {
            let (name, short_name, description, url) = (name.clone(), short_name.clone(), description.clone(), url.clone());
            let applications = applications.clone();
            let identity = identity;
            let admitted = std::sync::Arc::clone(&admitted);
            Box::pin(async move {
                if !registry::lock_admission(tx, &applications, &[corp], &[me]).await.map_err(db_err)? { return Err(notify_error(messages::PLAYER_CORP_CEO_REQUIRED)); }
                admission::ceo(tx, &identity, corp).await?;
                if dao::live_corporation_alliance(tx, corp).await.map_err(db_err)? != Some(None) { return Err(notify_error(messages::CORP_ALREADY_IN_ALLIANCE)); }
                if dao::name_taken(tx, &name, &short_name).await.map_err(db_err)? { return Err(notify_error(messages::ALLIANCE_NAME_OR_SHORT_NAME_TAKEN)); }
                dao::lock_applications(tx, corp).await.map_err(db_err)?;
                if dao::applications(tx, dao::ApplicationFilter::Corporation(corp)).await.map_err(db_err)?.iter().any(|a| applications.binary_search(&a.alliance).is_err()) {
                    return Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION));
                }
                if !gm {
                    super::super::wallets::corp_debit(tx, outbox, corp, ACCOUNT_CASH, crate::isk::IskAmount::from_whole_isk(CREATION_COST).ok_or(crate::isk::AmountError)?).await?;
                }
                let id = dao::create(tx, &name, &short_name, &description, &url, corp, me, now).await.map_err(db_err)?;
                set_alliance(tx, outbox, corp, Some(id), now, &admitted).await?;
                for app in dao::delete_other_applications(tx, corp, id, APP_NEW).await.map_err(db_err)? {
                    queue_application_changed(tx, outbox, app.alliance, corp, Some(application_line(app))).await?;
                }
                Ok(id)
            })
        },
        |id, outbox| async move { dispatch_world(id, outbox, world) },
    ).await?;
    info!("alliances: {me} of {corp} founded {id} <{short_name}> {name}");
    Ok(id)
}

/// Found an alliance without charging the corporation creation fee.
pub(crate) async fn gm_create(c: &mut Client, name: &str, ticker: &str) -> Result<i64, CallError> {
    create_core(c, name.trim().to_owned(), ticker.trim().to_uppercase(), String::new(), String::new(), true).await
}

/// Admit the caller's corporation without an application or executor approval.
pub(crate) async fn gm_join(c: &mut Client, alliance: i64) -> Result<(), CallError> {
    let identity = Identity::of_client(c);
    let corp = admission::actor_corp(&c.db, &identity).await?;
    admission::decide(&c.db, &c.world, &identity, alliance, corp, admission::Decision::Join).await
}
/// GetSuggestedAllianceShortNames(name) -> [KeyVal(shortName)]
fn suggested_short_names(name: &str) -> Value {
    Value::list(alliances::suggested_names(name).into_iter().map(|t| keyval([("shortName", Value::unicode(t))])))
}
fn bound_corp(call: &Call<'_>) -> Option<i64> {
    if !call.bound { return None; }
    call.bind_params.and_then(|v| v.as_int().or_else(|| v.items().and_then(|v| v.first()?.as_int())))
}

async fn application_write(c: &Client, corp: i64, alliance: i64, text: Option<String>) -> CallResult {
    let identity = Identity::of_client(c);
    let db = &c.db;
    let now = filetime_now();
    db.transaction(|tx, outbox| {
        let text = text.clone();
        let identity = identity;
        Box::pin(async move {
            if !registry::lock_admission(tx, &[alliance], &[corp], &[identity.char_id]).await.map_err(db_err)? {
                return Err(notify_error(messages::PLAYER_CORP_CEO_REQUIRED));
            }
            dao::lock_applications(tx, corp).await.map_err(db_err)?;
            admission::ceo(tx, &identity, corp).await?;
            let old = application_line_of(tx, alliance, corp).await?;
            if let Some(text) = text {
                if dao::live_corporation_alliance(tx, corp).await.map_err(db_err)? != Some(None) {
                    return Err(notify_error(messages::CORP_ALREADY_IN_ALLIANCE));
                }
                if !dao::exists(tx, alliance).await.map_err(db_err)? { return Err(notify_error(messages::ALLIANCE_NOT_FOUND)); }
                if dao::store_application(tx, alliance, corp, &text, APP_NEW, now).await.map_err(db_err)? != 1 {
                    return Err(CallError::User { msg: messages::MSG_CANT_APPLY_TWICE_TO_ALLIANCE.into(), dict: vec![] });
                }
            } else if let Some(row) = &old {
                let state = row[4].as_int().unwrap_or(0) as i32;
                if state != APP_NEW && state != APP_REJECTED { return Err(notify_error(messages::ALLIANCE_APPLICATION_STATUS_INVALID)); }
                if dao::delete_application(tx, alliance, corp, state).await.map_err(db_err)? != 1 {
                    return Err(notify_error(messages::ALLIANCE_APPLICATION_NOT_FOUND));
                }
            } else { return Ok(Value::None); }
            queue_application_changed(tx, outbox, alliance, corp, old).await?;
            Ok(Value::None)
        })
    }, |value, outbox| async move { dispatch_world(value, outbox, &c.world) }).await
}
/// The alliance side of corpRegistry.
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let db = &c.db;
    Some(match call.method {
        "CreateAlliance" => create(c, call).await,
        "GetSuggestedAllianceShortNames" => Ok(suggested_short_names(&trimmed_arg_text(call, 0))),
        // ApplyToJoinAlliance(allianceID, applicationText)
        "ApplyToJoinAlliance" => {
            let [alliance, Value::Unicode(text)] = call.args else { return Some(Err(notify_error(messages::ALLIANCE_APPLICATION_STATUS_INVALID))); };
            let Some(alliance) = alliance.as_int() else { return Some(Err(notify_error(messages::ALLIANCE_APPLICATION_STATUS_INVALID))); };
            let Some(corp) = bound_corp(call) else { return Some(Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION))); };
            if text.contains('\0') { return Some(Err(notify_error(messages::ALLIANCE_APPLICATION_STATUS_INVALID))); }
            application_write(c, corp, alliance, Some(text.clone())).await
        }
        // {allianceID: application} of the session's corporation.
        "GetAllianceApplications" => {
            let Some(corp) = bound_corp(call) else { return Some(Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION))); };
            if !call.args.is_empty() { return Some(Err(notify_error(messages::ALLIANCE_APPLICATION_STATUS_INVALID))); }
            let identity = Identity::of_client(c);
            db.transaction(|tx, _: &mut ()| {
                let identity = identity;
                Box::pin(async move {
                    if !registry::lock_admission(tx, &[], &[corp], &[identity.char_id]).await.map_err(db_err)?
                        || admission::actor_corp(tx, &identity).await? != corp {
                        return Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION));
                    }
                    let rows = dao::applications(tx, dao::ApplicationFilter::Corporation(corp)).await.map_err(db_err)?;
                    let header = application_header();
                    let rows = rows.into_iter().map(|r| (r.alliance, header.row(&application_line(r)))).collect();
                    Ok(indexed_rowset(&header, "allianceID", rows))
                })
            }, |value, ()| async move { Ok(value) }).await
        }
        "DeleteAllianceApplication" => {
            let [alliance] = call.args else { return Some(Err(notify_error(messages::ALLIANCE_APPLICATION_STATUS_INVALID))); };
            let Some(alliance) = alliance.as_int() else { return Some(Err(notify_error(messages::ALLIANCE_APPLICATION_STATUS_INVALID))); };
            let Some(corp) = bound_corp(call) else { return Some(Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION))); };
            application_write(c, corp, alliance, None).await
        }
        _ => return None,
    })
}
