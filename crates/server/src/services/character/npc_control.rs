//! Persistent per-character NPC target-ignore switch behind GM `/invu`.
//! The flag lives in `game.characters`; `true` makes the character's piloted
//! ship and owned space drones ineligible NPC targets without granting any
//! damage immunity. Reads never lock or publish. A write commits inside the
//! existing market guard, synchronously enqueues the change to every live
//! system actor before the guard is released, then awaits each actor's ack
//! after the unlock; a closed receiver means the flag is saved but that scene
//! was already gone and will pick the persisted value up on its next real
//! load. This service has no client wire contract; the slash service owns
//! authorization and reply text.
use marshal::Value;
use infinity_core::messages;
use crate::dao::characters::{npc_control as dao, selection};
use crate::db::Db;
use crate::services::{db_err, identity::Identity, market, user_error, CallError};
use crate::space::World;

/// Caller intent: read, set explicitly, or atomically invert the flag.
#[derive(Clone, Copy)]
pub(crate) enum Request { Toggle, Set(bool), Status }

/// Committed result, distinguishing reads and incomplete live confirmation.
pub(crate) enum Outcome {
    Status(bool),
    Updated { ignored: bool, live_synced: bool },
}

/// The caller's selected character is absent, not owned or no longer live.
fn no_character() -> CallError {
    user_error(messages::MSG_SLASH_ERROR, vec![("reason".into(), Value::unicode(messages::gm::GM_ARGS_NO_CHARACTER))])
}

/// Apply one `/invu` request to the caller's own selected character.
/// `Status` only reads; mutations serialize prepare/publish against the
/// existing market guard exactly like every production ship/drone load.
pub(crate) async fn execute(
    identity: &Identity, user: i64, db: &Db, world: &World, request: Request,
) -> Result<Outcome, CallError> {
    let character = identity.char_id;
    if character <= 0 { return Err(no_character()); }
    let desired = match request {
        Request::Status => {
            if !selection::owns(db, character, user).await.map_err(db_err)? { return Err(no_character()); }
            return Ok(Outcome::Status(dao::get(db, character).await.map_err(db_err)?.ok_or_else(no_character)?));
        }
        Request::Toggle => None,
        Request::Set(ignored) => Some(ignored),
    };
    let (ignored, acks) = {
        let _market = market::lock().await;
        db.transaction::<_, _, _, crate::db::outbox::Outbox, _, _, _>(
            move |tx, _| Box::pin(async move {
                dao::set(tx, character, user, desired).await.map_err(db_err)?.ok_or_else(no_character)
            }),
            move |ignored, _| std::future::ready(Ok::<_, CallError>((ignored, world.publish_npc_ignored(character, ignored)))),
        ).await?
    };
    let mut live_synced = true;
    for ack in acks { live_synced &= ack.await.is_ok(); }
    Ok(Outcome::Updated { ignored, live_synced })
}
