//! Alliance creation and applications through `corpRegistry`, mirroring
//! `eve/client/script/ui/services/alliances` and `eve/client/script/ui/services/corporation`.
//! Shared alliance mechanics in `services::alliances` use `game.alliances`,
//! `game.corporations` and `game.alliance_applications` through the DAO.
//! Wars, bills, sovereignty, capital systems and prime time are not supported.
use super::*;
use crate::dao::alliances as dao;
use crate::services::alliances::{self, application_changed, application_header, application_line, application_line_of, indexed_rowset, set_alliance, APP_NEW};
use crate::services::{db_err, keyval, notify_error, trimmed_arg_text, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{ACCOUNT_CASH, ORG_NAME_MAX_LENGTH, ORG_NAME_MIN_LENGTH, ORG_SHORT_NAME_MAX_LENGTH, ORG_SHORT_NAME_MIN_LENGTH};
use crate::db::Db;
use crate::db::outbox::dispatch_world;
use crate::messages;
use marshal::Value;
use tracing::info;
/// appConst.allianceCreationCost
const CREATION_COST: f64 = 1_000_000_000.0;
/// The CEO of a player corporation that isn't in an alliance yet.
async fn unallied_ceo_corp(db: &Db, me: i64) -> Result<i64, CallError> {
    let (corp, _) = membership(db, me).await?;
    if !is_player_corp(corp) || ceo_of(db, corp).await? != Some(me) {
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
    let (name, short_name, description, url) = (trimmed_arg_text(call, 0), trimmed_arg_text(call, 1).to_uppercase(), trimmed_arg_text(call, 2), trimmed_arg_text(call, 3));
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
    let _trading = super::super::market::lock().await;
    let now = filetime_now();
    let id = db.transaction(
        |tx, outbox| {
            let (name, short_name, description, url) = (name.clone(), short_name.clone(), description.clone(), url.clone());
            Box::pin(async move {
                if !gm {
                    super::super::wallets::corp_debit(tx, outbox, corp, ACCOUNT_CASH, CREATION_COST).await?;
                }
                let id = dao::create(tx, &name, &short_name, &description, &url, corp, me, now).await.map_err(db_err)?;
                dao::delete_corp_applications(tx, corp).await.map_err(db_err)?;
                set_alliance(tx, outbox, corp, Some(id), now).await?;
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
    let db = &c.db;
    let me = c.session.int("charid").unwrap_or(0);
    let corp = unallied_ceo_corp(db, me).await?;
    alliances::alliance_registry::admit(db, &c.world, alliance, corp, false).await
}
/// GetSuggestedAllianceShortNames(name) -> [KeyVal(shortName)]
fn suggested_short_names(name: &str) -> Value {
    Value::list(alliances::suggested_names(name).into_iter().map(|t| keyval([("shortName", Value::unicode(t))])))
}
/// The alliance side of corpRegistry.
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let (db, world) = (&c.db, &*c.world);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    Some(match call.method {
        "CreateAlliance" => create(c, call).await,
        "GetSuggestedAllianceShortNames" => Ok(suggested_short_names(&trimmed_arg_text(call, 0))),
        // ApplyToJoinAlliance(allianceID, applicationText)
        "ApplyToJoinAlliance" => {
            let (alliance, text) = (int(0), trimmed_arg_text(call, 1));
            let corp = match unallied_ceo_corp(db, me).await {
                Ok(corp) => corp,
                Err(e) => return Some(Err(e)),
            };
            match dao::exists(db, alliance).await {
                Ok(true) => {}
                Ok(false) => return Some(Err(notify_error(messages::ALLIANCE_NOT_FOUND))),
                Err(e) => return Some(Err(db_err(e))),
            }
            let old = match application_line_of(db, alliance, corp).await {
                Ok(old) => old,
                Err(e) => return Some(Err(e)),
            };
            if old.as_ref().is_some_and(|l| l[4].as_int() == Some(APP_NEW as i64)) {
                return Some(Err(CallError::User { msg: messages::MSG_CANT_APPLY_TWICE_TO_ALLIANCE.into(), dict: vec![] }));
            }
            let stored = dao::store_application(db, alliance, corp, &text, APP_NEW, filetime_now()).await.map_err(db_err);
            match stored {
                Ok(_) => application_changed(db, world, alliance, corp, old).await.map(|_| Value::None),
                Err(e) => Err(e),
            }
        }
        // {allianceID: application} of the session's corporation.
        "GetAllianceApplications" => {
            let corp = c.session.int("corpid").unwrap_or(0);
            match dao::applications(db, dao::ApplicationFilter::Corporation(corp)).await {
                Ok(rows) => {
                    let header = application_header();
                    let rows = rows.into_iter().map(|r| (r.alliance, header.row(&application_line(r)))).collect();
                    Ok(indexed_rowset(&header, "allianceID", rows))
                }
                Err(e) => Err(db_err(e)),
            }
        }
        "DeleteAllianceApplication" => {
            let alliance = int(0);
            let corp = c.session.int("corpid").unwrap_or(0);
            if ceo_of(db, corp).await.ok().flatten() != Some(me) {
                return Some(Err(notify_error(messages::CORP_CEO_REQUIRED_TO_WITHDRAW_ALLIANCE_APPLICATION)));
            }
            let old = match application_line_of(db, alliance, corp).await {
                Ok(old) => old,
                Err(e) => return Some(Err(e)),
            };
            match dao::delete_application(db, alliance, corp).await {
                Ok(_) if old.is_some() => application_changed(db, world, alliance, corp, old).await.map(|_| Value::None),
                Ok(_) => Ok(Value::None),
                Err(e) => Err(db_err(e)),
            }
        }
        _ => return None,
    })
}
