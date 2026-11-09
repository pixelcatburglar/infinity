//! Corporation militia whitelist methods for `fwCharacterEnlistmentMgr`.
//! Mirrors the corporation window enlistment calls and `facwarCommon` faction list.
//! Player corporation whitelists use `game.corp_enlistment_factions` and require a director or CEO to edit; NPCs allow all factions.
//! Direct enlistment and leaving are refused; militia enlistment is not modelled.

use super::super::{db_err, has_role_or_is_ceo, notify_error, Call, CallResult};
use crate::client::Client;
use infinity_core::consts::{ROLE_DIRECTOR, WARFARE_FACTIONS};
use crate::dao::corporations::enlistment;
use crate::db::outbox::{dispatch_world, Outbox};
use infinity_core::messages;
use marshal::Value;
use tracing::info;

fn is_npc_corp(corp_id: i64) -> bool {
    (1_000_000..2_000_000).contains(&corp_id)
}

/// Factions whose militias the corporation's members may enlist in directly. NPC
/// corporations allow every militia; player corporations keep a director-managed whitelist.
async fn allowed_factions(c: &Client, corp_id: i64) -> CallResult {
    if is_npc_corp(corp_id) {
        return Ok(Value::list(WARFARE_FACTIONS.map(Value::Int)));
    }
    let rows = enlistment::allowed(&c.db, corp_id).await.map_err(db_err)?;
    Ok(Value::list(rows.into_iter().map(Value::Int)))
}

/// SetMyCorpAllowedEnlistmentFactions(factionIDs) from the corporation window's whitelist.
async fn set_allowed_factions(c: &Client, call: &Call<'_>) -> CallResult {
    let Some(corp_id) = c.session.int("corpid").filter(|id| !is_npc_corp(*id)) else {
        return Ok(Value::None);
    };
    let me = c.session.int("charid").unwrap_or(0);
    let mut factions: Vec<i64> = match call.args.first() {
        Some(Value::List(l) | Value::Tuple(l)) => l.iter().filter_map(|v| v.as_int()).filter(|f| WARFARE_FACTIONS.contains(f)).collect(),
        _ => vec![],
    };
    factions.sort_unstable();
    factions.dedup();
    let factions = std::sync::Arc::new(factions);
    c.db.transaction(
        |tx, _: &mut Outbox| {
            let factions = std::sync::Arc::clone(&factions);
            Box::pin(async move {
                if !crate::dao::corporations::registry::lock_admission(tx, &[], &[corp_id], &[me]).await.map_err(db_err)?
                    || !has_role_or_is_ceo(tx, me, corp_id, ROLE_DIRECTOR).await? {
                    return Err(notify_error(messages::DIRECTOR_REQUIRED_TO_EDIT_CORP_INFO));
                }
                enlistment::clear(tx, corp_id).await.map_err(db_err)?;
                for &faction in factions.iter() {
                    enlistment::insert(tx, corp_id, faction).await.map_err(db_err)?;
                }
                Ok(Value::None)
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, &c.world) },
    ).await?;
    info!(user = c.login.user_id, "corp {corp_id}: militia whitelist {factions:?}");
    Ok(Value::None)
}

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    let arg_int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    Some(match call.method {
        "GetCorpAllowedEnlistmentFactions" => {
            let corp = arg_int(0).or(c.session.int("corpid")).unwrap_or(0);
            allowed_factions(c, corp).await
        }
        "SetMyCorpAllowedEnlistmentFactions" => set_allowed_factions(c, call).await,
        "CreateMyDirectEnlistment" | "RemoveMyDirectEnlistment" => Err(notify_error(messages::faction_warfare::UNAVAILABLE)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
