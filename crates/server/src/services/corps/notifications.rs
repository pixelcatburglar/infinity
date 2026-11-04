//! Persistent corporation application notices, using notificationUtil.py and notificationconst.py.
//! Rows are stored in game.notifications by the mail service in the caller's transaction.
//! Only application and invitation transitions are covered.
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
