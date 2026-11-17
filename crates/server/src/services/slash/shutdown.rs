//! `/shutdown`: schedule or replace, cancel or report the in-process planned
//! shutdown kept by `crate::shutdown`. The minutes argument means "minutes from
//! now until the shutdown"; it becomes the future FILETIME `when` on the wire
//! and never a maintenance duration. The plan is memory-only and not persisted.

use super::{slash_error, CallError, Client};
use crate::shutdown::{CancelResult, StartError, Status};
use infinity_core::messages;

pub(super) fn command(c: &Client, args: &[String]) -> Result<String, CallError> {
    let usage = || slash_error(messages::gm::GM_SHUTDOWN_USAGE.into());
    match args {
        [arg] if arg.eq_ignore_ascii_case("cancel") => match crate::shutdown::cancel(&c.world) {
            CancelResult::Cancelled => Ok(messages::gm::GM_SHUTDOWN_CANCELLED.into()),
            CancelResult::None => Ok(messages::gm::GM_SHUTDOWN_NOT_SCHEDULED.into()),
            CancelResult::Due => Err(slash_error(messages::gm::GM_SHUTDOWN_DUE.into())),
        },
        [arg] if arg.eq_ignore_ascii_case("status") => Ok(match crate::shutdown::status() {
            Status::None => messages::gm::GM_SHUTDOWN_NOT_SCHEDULED.into(),
            Status::Triggered => messages::gm::GM_SHUTDOWN_TRIGGERED.into(),
            Status::Remaining(remaining) => messages::gm::gm_shutdown_remaining(remaining.as_secs()),
        }),
        [minutes] => {
            let minutes = minutes.parse::<i64>().ok().filter(|m| *m > 0).ok_or_else(usage)?;
            crate::shutdown::initiate(&c.world, minutes)
                .map(|()| messages::gm::gm_shutdown_scheduled(minutes))
                .map_err(|error| match error {
                    StartError::Invalid => usage(),
                    StartError::Due => slash_error(messages::gm::GM_SHUTDOWN_DUE.into()),
                })
        }
        _ => Err(usage()),
    }
}
