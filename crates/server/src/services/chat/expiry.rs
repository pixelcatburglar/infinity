//! Durable mute and ban deadlines for `xmppchatclient/xmppchatsvc.py` expiring records.
//! ACL deadlines and scheduler jobs are committed together in game.chat_channel_members
//! and game.jobs. Only player-room character restrictions are supported.
use std::{future::Future, pin::Pin, sync::Arc};
use marshal::Value;
use serde::{Deserialize, Serialize};
use infinity_core::{consts::FILETIME_SECOND, messages::chat as text, session::OnlineSession};
use crate::{chat::Hub, client::{filetime_now, filetime_ticks_from_seconds}, dao::chat_channels::{self as dao, Restriction}, db::{self, Db, executor::Executor, outbox::{self, Outbox}}, scheduler::{self, Job}, space::World};
use super::{authority, PERMISSIONS_REFRESH};
use super::super::{CallError, notify_error};

/// Durable scheduler kind for client chat restriction expiry records.
pub(crate) const KIND: &str = "chat-restriction-expiry";

#[derive(Serialize, Deserialize)]
struct Payload { room: String, character: i64, kind: String, expires_at: i64 }

fn job_id(name: &str, target: i64, kind: Restriction) -> String {
    format!("chat-restriction:{}:{name}:{target}", kind.as_str())
}

pub(super) async fn cancel_in_transaction(ex: &(impl Executor + ?Sized), name: &str, target: i64, kind: Restriction) -> Result<(), db::Error> {
    dao::clear_expiry(ex, name, target, kind).await?;
    scheduler::cancel(ex, &job_id(name, target, kind)).await?;
    Ok(())
}

async fn authorize(ex: &(impl Executor + ?Sized), actor: i64, name: &str) -> Result<(), CallError> {
    authority::locked(ex, name).await?;
    authority::require_manager(ex, name, actor).await
}

pub(crate) async fn set(db: &Db, hub: &Hub, session: &OnlineSession, name: &str, target: i64, kind: Restriction, duration_secs: i64) -> Result<(), CallError> {
    if target <= 0 || duration_secs <= 0 || duration_secs > i64::MAX / FILETIME_SECOND { return Err(notify_error(text::INVALID)); }
    let now = filetime_now();
    let deadline = now.checked_add(filetime_ticks_from_seconds(duration_secs)).ok_or_else(|| notify_error(text::INVALID))?;
    let payload = serde_json::to_string(&Payload { room: name.into(), character: target, kind: kind.as_str().into(), expires_at: deadline }).map_err(|e| CallError::Internal(e.into()))?;
    let job = Arc::new(Job { id: job_id(name, target, kind), kind: KIND.into(), due_at: deadline, payload });
    let name: Arc<str> = Arc::from(name);
    let actor = session.character_id;
    db.transaction(|tx, outbox: &mut Outbox<authority::ChatEffect>| { let job = Arc::clone(&job); let name = Arc::clone(&name); Box::pin(async move {
        authorize(tx, actor, &name).await?;
        if !dao::attach_expiry(tx, &name, target, kind, deadline, now).await? { return Err(notify_error(text::RESTRICTION_ABSENT)); }
        scheduler::cancel(tx, &job.id).await?;
        scheduler::schedule(tx, &job).await?;
        outbox.operation(authority::ChatEffect::Refresh(name.to_string()));
        Ok(())
    }) }, |value, outbox| async move { scheduler::wake(); authority::dispatch(value, outbox, hub).await }).await
}

pub(crate) async fn remove(db: &Db, hub: &Hub, session: &OnlineSession, name: &str, target: i64, kind: Restriction) -> Result<(), CallError> {
    if target <= 0 { return Err(notify_error(text::INVALID)); }
    let name: Arc<str> = Arc::from(name);
    let actor = session.character_id;
    db.transaction(|tx, outbox: &mut Outbox<authority::ChatEffect>| { let name = Arc::clone(&name); Box::pin(async move {
        authorize(tx, actor, &name).await?;
        cancel_in_transaction(tx, &name, target, kind).await?;
        outbox.operation(authority::ChatEffect::Refresh(name.to_string()));
        Ok(())
    }) }, |value, outbox| authority::dispatch(value, outbox, hub)).await
}

pub(crate) async fn list(db: &Db, session: &OnlineSession, name: &str, kind: Restriction) -> Result<Vec<i64>, CallError> {
    let name: Arc<str> = Arc::from(name);
    let actor = session.character_id;
    db.transaction(|tx, _: &mut Outbox| { let name = Arc::clone(&name); Box::pin(async move {
        authorize(tx, actor, &name).await?;
        Ok(dao::members(tx, &name).await?.into_iter().filter(|m| match kind {
            Restriction::Mute => m.voice_expires_at.is_some(),
            Restriction::Ban => m.ban_expires_at.is_some(),
        }).map(|m| m.character_id).collect())
    }) }, |value, _| async move { Ok(value) }).await
}

pub(crate) fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let payload: Arc<Payload> = Arc::new(serde_json::from_str(&job.payload)?);
        let kind = match payload.kind.as_str() { "mute" => Restriction::Mute, "ban" => Restriction::Ban, _ => anyhow::bail!("invalid chat restriction kind") };
        db.transaction(|tx, outbox: &mut Outbox| { let payload = Arc::clone(&payload); Box::pin(async move {
            let Some(room) = dao::get_for_update(tx, &payload.room).await? else { return Ok(()); };
            if !authority::valid_room(&room) || room.category != "player" { return Ok(()); }
            if dao::settle_expiry(tx, &payload.room, payload.character, kind, payload.expires_at, filetime_now()).await? {
                outbox.notify_pilot(payload.character, PERMISSIONS_REFRESH, vec![Value::str(&payload.room)]);
            }
            Ok(())
        }) }, |value, outbox| async move { outbox::dispatch_world(value, outbox, &world) }).await.map_err(|e: CallError| anyhow::anyhow!("chat expiry failed: {e:?}"))
    })
}

#[cfg(test)] mod tests;
