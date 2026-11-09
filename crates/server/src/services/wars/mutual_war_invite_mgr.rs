//! `mutualWarInviteMgr`, mirroring the client mutualWarInvites service. Invitations
//! and blocks are persisted; aggressive war fees and allies are not implemented.
use infinity_core::consts::FILETIME_DAY;
use crate::{client::{Client, filetime_now}, dao::wars::{self, Invite}, db::outbox::{Outbox, dispatch_world}, scheduler, services::{Call, CallResult, keyval}};
use marshal::Value;
use super::{authority, deadlines, lifecycle::{self, invalid, WAR_MUTUAL_WAR_EXPIRY_DAYS, WAR_SPOOLUP}};

fn id(value: Option<&Value>) -> Result<i64, crate::services::CallError> {
    match value { Some(Value::Int(id)) if *id > 0 => Ok(*id), _ => Err(invalid()) }
}
fn boolean(value: Option<&Value>) -> Result<bool, crate::services::CallError> {
    match value { Some(Value::Bool(b)) => Ok(*b), Some(Value::Int(0)) => Ok(false), Some(Value::Int(1)) => Ok(true), _ => Err(invalid()) }
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "mutualWarInviteMgr" { return None; }
    let actor = c.session.int("charid").unwrap_or(0);
    Some(match call.method {
        "GetPendingInvitesForSession" => async {
            if !call.args.is_empty() { return Err(invalid()); }
            let owner = authority::owner(&c.db, actor).await?;
            let rows = wars::invitations(&c.db, owner, filetime_now()).await?;
            Ok(Value::list(rows.iter().map(|r| keyval([("fromOwnerID", Value::Int(r.from_owner)), ("toOwnerID", Value::Int(r.to_owner)), ("sentDate", Value::Int(r.sent_at))]))))
        }.await,
        "IsCorpInvitesBlockedPlayer" => async {
            if !call.args.is_empty() { return Err(invalid()); }
            let owner = authority::owner(&c.db, actor).await?;
            Ok(Value::Bool(wars::invites_blocked(&c.db, owner).await?))
        }.await,
        "SetInvitesBlockedByPlayer" => async {
            if call.args.len() != 1 { return Err(invalid()); }
            let blocked = boolean(call.args.first())?;
            let owner = authority::owner(&c.db, actor).await?;
            let world = &c.world;
            c.db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
                authority::lock_actor(tx, actor).await?;
                wars::lock_owners(tx, &[owner]).await?;
                if authority::owner(tx, actor).await? != owner { return Err(invalid()); }
                authority::require_owner(tx, actor, owner).await?;
                if wars::set_invites_blocked(tx, owner, blocked).await? {
                    lifecycle::tell(tx, outbox, &[owner], "OnMutualWarsUpdated_Remote", vec![]).await?;
                }
                Ok(Value::None)
            }), |value, outbox| async move { dispatch_world(value, outbox, world) }).await
        }.await,
        "SendInviteByPlayer" | "WithdrawInviteByPlayer" | "RespondToInviteByPlayer" => async {
            if call.args.len() != if call.method == "RespondToInviteByPlayer" { 2 } else { 1 } { return Err(invalid()); }
            let other = id(call.args.first())?;
            let accepts = if call.method == "RespondToInviteByPlayer" { Some(boolean(call.args.get(1))?) } else { None };
            let sending = call.method == "SendInviteByPlayer";
            let owner = authority::owner(&c.db, actor).await?;
            let now = filetime_now();
            let world = &c.world;
            c.db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
                authority::lock_actor(tx, actor).await?;
                wars::lock_owners(tx, &[owner,other]).await?;
                if authority::owner(tx, actor).await? != owner || owner == other { return Err(invalid()); }
                authority::require_owner(tx, actor, owner).await?;
                let (from, to) = if accepts.is_some() { (other, owner) } else { (owner, other) };
                if sending {
                    if !authority::valid_target(tx, other).await? || wars::invites_blocked(tx, other).await? { return Err(invalid()); }
                    lifecycle::ended(tx, outbox, from, to, now).await?;
                    if wars::ongoing_pair(tx, from, to, now).await?.is_some() { return Err(invalid()); }
                    let invite = Invite { from_owner: from, to_owner: to, sent_at: now, expires_at: now + WAR_MUTUAL_WAR_EXPIRY_DAYS * FILETIME_DAY };
                    if !wars::insert_invite(tx, &invite).await? { return Err(invalid()); }
                    deadlines::schedule_invite(tx, &invite).await?;
                } else {
                    let invite = wars::invite(tx, from, to, now).await?.ok_or_else(invalid)?;
                    if accepts == Some(true) {
                        if !authority::valid_target(tx, from).await? || wars::invites_blocked(tx, to).await? { return Err(invalid()); }
                        lifecycle::ended(tx, outbox, from, to, now).await?;
                        if wars::ongoing_pair(tx, from, to, now).await?.is_some() { return Err(invalid()); }
                        let war = wars::create(tx, from, to, now, now + WAR_SPOOLUP).await?.ok_or_else(invalid)?;
                        deadlines::schedule_start(tx, &war).await?;
                        lifecycle::war_changed(tx, outbox, &war, super::wire::changes(None, &war)).await?;
                        if let Some(reverse) = wars::invite(tx, to, from, now).await? {
                            wars::remove_invite(tx, to, from, reverse.sent_at).await?;
                            deadlines::cancel_invite(tx, to, from).await?;
                        }
                    }
                    if !wars::remove_invite(tx, from, to, invite.sent_at).await? { return Err(invalid()); }
                    deadlines::cancel_invite(tx, from, to).await?;
                }
                lifecycle::invites_changed(tx, outbox, from, to).await?;
                Ok(Value::None)
            }), |value, outbox| async move { let r = dispatch_world(value, outbox, world); scheduler::wake(); r }).await
        }.await,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
