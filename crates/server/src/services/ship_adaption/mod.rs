//! Ship adaptation chip ledger and retained read-only compatibility service.
//! Client contracts follow `adaptTreeSvc.py`; node and mission mutations remain
//! unavailable. Historical chip balances use the captured retained content package.

pub(crate) mod model;
pub(crate) mod operations;
pub(crate) mod policy;
pub(crate) mod wire;

use crate::{client::Client, dao, lua::content::{self, protocol::{self, Identity, Response}}, services::{Call, CallResult, db_err, notify_error}};
use marshal::Value;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "shipAdaptionMgr" { return None; }
    match call.method {
        "InjectChips" | "call_chip_exchange" | "call_finish_node_mission" | "call_assign_mission" | "call_unlock_node" => return Some(Err(notify_error(infinity_core::messages::ship_adaption::UNAVAILABLE))),
        "fetch_tree_data" | "fetch_missions" | "fetch_user_data" => (),
        _ => return None,
    }
    Some(read_retained(c, call).await)
}

async fn read_retained(c: &mut Client, call: &Call<'_>) -> CallResult {
    let user = c.login.user_id;
    let character = c.session.int("charid").unwrap_or(0);
    let request = model::decode(call, user, character, 0).ok_or_else(|| notify_error(infinity_core::messages::ship_adaption::UNAVAILABLE))?;
    if !dao::characters::selection::owns(&c.db, character, user).await.map_err(db_err)? {
        return Err(notify_error(infinity_core::messages::ship_adaption::UNAVAILABLE));
    }
    let snapshot = dao::ship_adaption::snapshot(&c.db, character).await.map_err(db_err)?;
    let Some(subject) = &snapshot.subject else {
        let identity = || Identity { user, character };
        let (event, text) = match request {
            model::Request::Missions => ("OnMissionResponsed", protocol::encode_content_text(Response::Mission { identity: identity() }).map_err(db_err)?),
            model::Request::User => ("OnUserResponsed", protocol::encode_content_text(Response::User { identity: identity(), chips: &[] }).map_err(db_err)?),
            _ => return Err(notify_error(infinity_core::messages::ship_adaption::UNAVAILABLE)),
        };
        c.notify(event, vec![Value::str(text)]);
        return Ok(Value::None);
    };
    let row = dao::lua_content::load(&c.db, &subject.content_namespace, &subject.content_version).await.map_err(db_err)?
        .ok_or_else(|| db_err(anyhow::anyhow!("missing retained adaptation catalogue")))?;
    let definitions = content::decode_retained_adaption(&row, &c.cfg.lua, &c.sde).map_err(db_err)?;
    let identity = || Identity { user, character };
    let (event, text) = match request {
        model::Request::Tree(tree) if definitions.published_tree(tree) => ("OnTreeResponsed", protocol::encode_content_text(Response::Tree { identity: identity(), tree }).map_err(db_err)?),
        model::Request::Missions => ("OnMissionResponsed", protocol::encode_content_text(Response::Mission { identity: identity() }).map_err(db_err)?),
        model::Request::User => ("OnUserResponsed", wire::user(&definitions, &snapshot, user, character)?),
        _ => return Err(notify_error(infinity_core::messages::ship_adaption::UNAVAILABLE)),
    };
    c.notify(event, vec![Value::str(text)]);
    Ok(Value::None)
}

#[cfg(test)]
mod tests;
