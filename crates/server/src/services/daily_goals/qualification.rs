//! Refresh current daily-goal qualification after committed subscription changes.
//! Frozen game daily-goal rows retain earned rights across grade changes.
//! Offline characters are refreshed when they next query their current goals.
use std::sync::Arc;
use crate::{db::outbox::{self, Outbox}, lua, space::World};
use super::{assignment, queries, wire, Error, Resources};

pub(crate) async fn changed(world: &Arc<World>, user: i64, at: i64) -> Result<(), Error> {
    if !lua::enabled() { return Ok(()); }
    let resources = Resources::from_world(world)?;
    for character in world.online_pilots() {
        let Some(session) = world.online_session(character) else { continue };
        if user <= 0 || session.user_id != Some(user) { continue; }
        let outcome = assignment::ensure_at(&resources, user, character, at, true).await?;
        if outcome.current.is_some() { continue; }
        if outcome.day.is_none() || outcome.month.is_none() { return Err(Error::Unavailable); }
        let views = queries::load_current_authorized(&resources, user, character, at, &outcome.bounds, outcome.pins).await?;
        let mut outbox = Outbox::default();
        outbox.notify_gateway(wire::current_notice(character, &views)?);
        outbox::dispatch_world((), outbox, world)?;
    }
    Ok(())
}

#[cfg(test)] mod tests;
