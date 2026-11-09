//! Persistent corporation application and governance notices, using the existing
//! mail notification producer and notificationUtil.py/notificationconst.py types.
//! Rows are written in the caller's transaction and roll back with its business change.
use super::*;

/// notificationconst notificationGroupCorp.
const GROUP_CORP: i32 = 3;
/// notificationconst notificationTypeCorpAppNew.
const NEW: i32 = 16;
/// notificationconst notificationTypeCorpAppReject.
const REJECT: i32 = 17;
/// notificationconst notificationTypeCorpAppAccept.
const ACCEPT: i32 = 18;
/// notificationconst notificationTypeCharAppAccept.
const CHAR_ACCEPT: i32 = 128;
/// notificationconst notificationTypeCharAppReject.
const CHAR_REJECT: i32 = 129;
/// notificationconst notificationTypeCharAppWithdraw.
const CHAR_WITHDRAW: i32 = 130;
/// notificationconst notificationTypeCorpAppInvited.
const INVITED: i32 = 139;
/// notificationconst notificationTypeCorpAppCustomReject.
const CUSTOM_REJECT: i32 = 142;
/// notificationconst CharLeftCorpMsg, CorpNewCEOMsg, CorpLiquidationMsg and CorpKicked.
const LEFT: i32 = 21;
const NEW_CEO: i32 = 22;
const LIQUIDATION: i32 = 85;
const KICKED: i32 = 92;

pub(super) async fn kicked<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, receiver: i64, corporation: i64) -> Result<(), CallError> {
    let payload = serde_json::json!({"corpID": corporation}).to_string();
    super::super::mail::notify(db, outbox, receiver, KICKED, GROUP_CORP, corporation, &payload).await
}

pub(super) async fn voluntary<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, character: i64, corporation: i64) -> Result<(), CallError> {
    let payload = serde_json::json!({"corpID": corporation}).to_string();
    for receiver in registry::application_managers(db, corporation, ROLE_PERSONNEL_MANAGER | ROLE_DIRECTOR).await.map_err(db_err)? {
        super::super::mail::notify(db, outbox, receiver, LEFT, GROUP_CORP, character, &payload).await?;
    }
    Ok(())
}

pub(super) async fn ceo<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, receivers: &[i64], corporation: i64) -> Result<(), CallError> {
    let payload = serde_json::json!({"corpID": corporation}).to_string();
    for &receiver in receivers {
        super::super::mail::notify(db, outbox, receiver, NEW_CEO, GROUP_CORP, corporation, &payload).await?;
    }
    Ok(())
}

pub(super) async fn liquidation<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, receivers: &[i64], corporation: i64) -> Result<(), CallError> {
    let payload = serde_json::json!({"corpID": corporation}).to_string();
    for &receiver in receivers {
        super::super::mail::notify(db, outbox, receiver, LIQUIDATION, GROUP_CORP, corporation, &payload).await?;
    }
    Ok(())
}

pub(super) async fn application<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, row: &registry::Application) -> Result<(), CallError> {
    let (kind, managers) = match row.status {
        APP_APPLIED => (NEW, true),
        APP_INVITED_BY_CORPORATION => (INVITED, false),
        APP_ACCEPTED_BY_CORPORATION => (ACCEPT, false),
        APP_REJECTED_BY_CORPORATION if !row.custom_message.is_empty() => (CUSTOM_REJECT, false),
        APP_REJECTED_BY_CORPORATION => (REJECT, false),
        APP_ACCEPTED_BY_CHARACTER => (CHAR_ACCEPT, true),
        APP_REJECTED_BY_CHARACTER => (CHAR_REJECT, true),
        APP_WITHDRAWN_BY_CHARACTER => (CHAR_WITHDRAW, true),
        _ => return Ok(()),
    };
    let payload = if kind == CUSTOM_REJECT {
        serde_json::json!({"corpID": row.corporation_id, "customMessage": row.custom_message}).to_string()
    } else {
        serde_json::json!({"corpID": row.corporation_id}).to_string()
    };
    let recipients = if managers {
        registry::application_managers(db, row.corporation_id, ROLE_PERSONNEL_MANAGER | ROLE_DIRECTOR).await.map_err(db_err)?
    } else {
        vec![row.character_id]
    };
    let sender = if managers { row.character_id } else { row.corporation_id };
    for receiver in recipients {
        super::super::mail::notify(db, outbox, receiver, kind, GROUP_CORP, sender, &payload).await?;
    }
    Ok(())
}
