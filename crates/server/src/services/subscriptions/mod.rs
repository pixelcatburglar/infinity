//! Account clone-grade authority. Subscription commerce and full Alpha restrictions are not implemented.
//! Client query semantics follow `omega/client/clone_grade_svc.py`; authority,
//! audit and durable deadlines use `game.account_clone_grades`,
//! `game.account_clone_grade_audit` and `game.jobs`.
//! The client sends the same GetCloneGrade request for with- and without-grace queries,
//! so it receives the effective grade including grace. GetSubscriptionTime is Unix
//! nanoseconds despite a conflicting client FILETIME subtraction.

use crate::client::Client;
use crate::services::{Call, CallResult};

pub(crate) mod rules;
pub(crate) mod management;
pub(crate) mod deadlines;
mod subscription_mgr;
mod alpha_injector_mgr;

pub(crate) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "subscriptionMgr" => subscription_mgr::dispatch(c, call).await,
        "alphaInjectorMgr" => alpha_injector_mgr::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)] mod tests;
