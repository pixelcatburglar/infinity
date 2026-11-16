//! GM `/invu` command, forwarded by the slash service: read or change the
//! caller's persistent NPC target-ignore switch through the character service.
//! Parse and reply selection only; the shared slash role gate owns
//! authorization, the caller's selected character is always the target, and
//! persistence plus live-actor publication stay in the service.
use super::{slash_error, CallError, Client};
use infinity_core::messages;
use crate::services::{character::npc_control::{self, Outcome, Request}, identity::Identity};

/// `/invu [on|off|status]`; a bare command toggles.
pub(super) async fn command(c: &mut Client, args: &[String]) -> Result<String, CallError> {
    let request = match args {
        [] => Request::Toggle,
        [arg] if arg.eq_ignore_ascii_case("on") => Request::Set(true),
        [arg] if arg.eq_ignore_ascii_case("off") => Request::Set(false),
        [arg] if arg.eq_ignore_ascii_case("status") => Request::Status,
        _ => return Err(slash_error(messages::gm::GM_INVU_USAGE.into())),
    };
    let identity = Identity::of_client(c);
    match npc_control::execute(&identity, c.login.user_id, &c.db, &c.world, request).await? {
        Outcome::Status(ignored) => Ok(messages::gm::gm_invu_status(ignored)),
        Outcome::Updated { ignored: true, live_synced: true } => Ok(messages::gm::GM_INVU_ENABLED.into()),
        Outcome::Updated { ignored: false, live_synced: true } => Ok(messages::gm::GM_INVU_DISABLED.into()),
        Outcome::Updated { ignored, live_synced: false } => Ok(messages::gm::gm_invu_saved(ignored)),
    }
}

#[cfg(all(test, feature = "tests-slash"))] mod tests;
