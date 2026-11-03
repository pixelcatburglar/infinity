//! `subscriptionMgr` clone-grade and subscription-expiry queries.
//! `omega/client/clone_grade_svc.py` uses the same grade RPC for both with- and
//! without-grace queries, so the result includes grace. Expiry is Unix nanoseconds,
//! although one client time-left calculation subtracts FILETIME from it.
//! Reads `game.account_clone_grades`; subscription commerce is not implemented.

use crate::client::{filetime_now, Client};
use crate::dao::accounts::subscriptions as dao;
use crate::services::{Call, CallResult};
use marshal::Value;

use super::rules;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if !matches!((call.service, call.method),
        ("subscriptionMgr", "GetCloneGrade" | "GetSubscriptionTime")) {
        return None;
    }
    Some(async {
        let row = dao::by_user(&c.db, c.login.user_id).await?;
        let now = filetime_now();
        match call.method {
            "GetCloneGrade" => Ok(Value::Int(i64::from(rules::effective_grade(&row, now)))),
            "GetSubscriptionTime" => Ok(Value::Int(rules::subscription_time(&row, now)?)),
            _ => unreachable!(),
        }
    }.await)
}
