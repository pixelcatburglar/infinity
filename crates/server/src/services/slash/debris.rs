//! GM debris cleanup adapters. `/sysjunkclear` destroys every wreck and ordinary
//! container ball in the session's current system (docked callers allowed), while
//! `/testclear` destroys those within 20 km of the caller's live ship. Both parse
//! zero arguments only; the selection, batch destruction and space removal live in
//! the inventory debris service.
use super::{args, slash_error, CallError, Client};
use crate::services::identity::Identity;
use crate::services::inventory::debris::{gm_clear, CleanupScope};
use infinity_core::messages;

pub(super) async fn command(c: &mut Client, command: &str, args: &[String]) -> Result<String, CallError> {
    if !args.is_empty() {
        let usage = if command == "sysjunkclear" { messages::gm::GM_SYSJUNKCLEAR_USAGE } else { messages::gm::GM_TESTCLEAR_USAGE };
        return Err(slash_error(usage.into()));
    }
    let identity = Identity::of_client(c);
    if identity.char_id == 0 {
        return Err(slash_error(messages::gm::GM_ARGS_NO_CHARACTER.into()));
    }
    let system = args::current_system(&c.session);
    let (system, scope) = match command {
        "sysjunkclear" => (system.ok_or_else(|| slash_error(messages::gm::GM_DEBRIS_NO_SYSTEM.into()))?, CleanupScope::System),
        _ => (system.ok_or_else(|| slash_error(messages::GM_UNDOCK_FIRST.into()))?, CleanupScope::Nearby),
    };
    let report = gm_clear(&identity, &c.db, &c.world, &c.sde, c.notify_tx.clone(), system, scope).await?;
    Ok(if report.space_applied {
        messages::gm::gm_debris_cleared(report.roots, report.items)
    } else {
        messages::gm::gm_debris_cleared_unconfirmed(report.roots, report.items)
    })
}
