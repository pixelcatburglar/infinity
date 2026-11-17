//! `/suicide`: destroy the caller's own live ship or capsule in space.
//! The command takes no arguments; it snapshots the caller's identity and
//! session, and the combat admin adapter runs the guarded loss. The ordinary
//! ship-loss service and the client's `__ship_destroyed` notification own the
//! rest; this handler neither writes the session nor sends notifications.

use super::{slash_error, CallError, Client};
use crate::services::identity::Identity;
use infinity_core::{messages, session::OnlineSession};

pub(super) async fn command(c: &mut Client, args: &[String]) -> Result<String, CallError> {
    if !args.is_empty() {
        return Err(slash_error(messages::gm::GM_SUICIDE_USAGE.into()));
    }
    // `solarsystemid` is the in-space position key; `solarsystemid2` keeps a
    // docked pilot's map system, so both must name the same positive system
    // and no station may be set.
    let in_space = c.session.int("solarsystemid").is_some_and(|system| system > 0)
        && c.session.int("solarsystemid") == c.session.int("solarsystemid2")
        && c.session.int("stationid").is_none_or(|station| station <= 0);
    let Some(expected) = OnlineSession::from_session(&c.session).filter(|_| in_space) else {
        return Err(crate::services::notify_error(messages::gm::GM_SUICIDE_UNAVAILABLE));
    };
    let identity = Identity::of_client(c);
    crate::services::combat::admin::suicide(&identity, &c.db, &c.world, &expected).await?;
    Ok(messages::gm::gm_suicide_completed(identity.ship_id.unwrap_or_default()))
}
