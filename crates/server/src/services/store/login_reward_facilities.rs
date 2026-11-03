//! loginRewardFacilities clone grade for the login rewards client.
//! Account authority is shared with seasonal reward settlement.
use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("loginRewardFacilities", "get_my_clone_grade") => crate::services::subscriptions::rules::current(&c.db, c.login.user_id, filetime_now()).await.map(|grade| Value::Int(grade as i64)),

        _ => return None,
    })
}
