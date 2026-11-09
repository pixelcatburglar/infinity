//! GM wormhole commands, forwarded by the slash service.
//! Connections use the exploration service and its persistent site data.
//! Subcommands operate on the current system; remote and universe scopes are unsupported.

use super::{slash_error, CallError, Client};
use infinity_core::messages;
use crate::{services::exploration::{self, wormhole_mgr}};

fn parse(args: &[String]) -> Result<(&str, usize), CallError> {
    let usage = || slash_error(messages::gm::GM_WH_USAGE.into());
    let verb = args.first().map(String::as_str).ok_or_else(usage)?;
    let (count, scope) = if verb.eq_ignore_ascii_case("random") {
        match args.get(1) {
            Some(value) if !value.eq_ignore_ascii_case("here") => {
                let count = value.parse::<usize>().ok().filter(|n| *n > 0).ok_or_else(usage)?;
                (count, args.get(2))
            }
            scope => (1, scope),
        }
    } else { (1, args.get(1)) };
    let expected = if verb.eq_ignore_ascii_case("random") && args.get(1).is_some_and(|s| !s.eq_ignore_ascii_case("here")) { 3 } else { 2 };
    if args.len() > expected || scope.is_some_and(|s| !s.eq_ignore_ascii_case("here")) {
        return Err(usage());
    }
    Ok((verb, count))
}

pub(super) async fn command(c: &mut Client, _command: &str, args: &[String]) -> Result<String, CallError> {
    if args.is_empty() { return exploration::gm_wormhole(c).await; }
    let (verb, count) = parse(args)?;
    if verb.eq_ignore_ascii_case("status") { wormhole_mgr::gm_status(c).await }
    else if verb.eq_ignore_ascii_case("ensure") { wormhole_mgr::gm_ensure(c).await }
    else if verb.eq_ignore_ascii_case("random") { wormhole_mgr::gm_random(c, count).await }
    else if verb.eq_ignore_ascii_case("clear") { wormhole_mgr::gm_clear(c).await }
    else { Err(slash_error(messages::gm::GM_WH_USAGE.into())) }
}

#[cfg(test)]
mod tests;
