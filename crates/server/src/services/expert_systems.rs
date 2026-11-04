//! Transport adapter for expertSystemMgr; rules live in skills::expert_systems.
use marshal::Value;
use crate::{client::Client, messages, services::{effects::online, identity::Identity, notify_error, skills::expert_systems, Call, CallResult}};

fn valid_envelope(call: &Call<'_>) -> bool {
    !call.bound && matches!(call.kwargs, Value::None) || !call.bound && matches!(call.kwargs, Value::Dict(v) if v.is_empty())
}

pub(crate) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "expertSystemMgr" { return None; }
    if !matches!(call.method, "GetMyExpertSystems" | "RemoveMyExpertSystem" | "ConsumeExpertSystem") { return None; }
    let invalid = || notify_error(messages::expert_systems::INVALID);
    if !valid_envelope(call) {
        return Some(Err(invalid()));
    }
    let identity = Identity::of_client(c);
    let dispatch = |n| online::dispatch_notification(n, &c.world, &c.notify_tx, Some(identity.char_id));
    Some(match call.method {
        "GetMyExpertSystems" if call.args.is_empty() => expert_systems::current(&identity, &c.db, &c.sde, dispatch).await,
        "RemoveMyExpertSystem" if call.args.len() == 1 => match call.args[0].as_int().and_then(|id| i32::try_from(id).ok()) {
            Some(id) => expert_systems::remove(&identity, &c.db, &c.world, &c.sde, id, dispatch).await.map(|()| Value::None),
            None => Err(invalid()),
        },
        "ConsumeExpertSystem" if call.args.len() == 1 => match call.args[0].as_int() {
            Some(id) => expert_systems::consume(&identity, &c.db, &c.world, &c.sde, id, dispatch).await.map(|()| Value::None),
            None => Err(notify_error(messages::expert_systems::INVALID_ITEM)),
        },
        _ => Err(invalid()),
    })
}

#[cfg(test)]
#[path = "expert_systems/tests.rs"]
mod tests;
