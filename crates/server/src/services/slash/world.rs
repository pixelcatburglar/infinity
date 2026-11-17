//! GM world-scope adapters: `/loadsys` preloads the caller's captured current
//! system plus the direct destinations of that system's own public stargates,
//! and `/loadallsys` preloads the complete public static-map set, including
//! systems without stargates. `/unloadallsys` sweeps the batch-opening live
//! instances only when strictly idle; it does not run automatically. All take
//! no arguments. `/loadsys` needs a selected character and the session's
//! canonical `solarsystemid2`, falling back to `solarsystemid` only while not
//! docked. The other commands require performance-testing mode but no character
//! or position. Target selection and lifecycle operations live in `space`.
use super::{args, slash_error, CallError, Client};
use crate::space::preload::PreloadScope;
use infinity_core::messages;

pub(super) async fn command(c: &mut Client, command: &str, args: &[String]) -> Result<String, CallError> {
    if !args.is_empty() {
        let usage = match command {
            "loadsys" => messages::gm::GM_LOADSYS_USAGE,
            "loadallsys" => messages::gm::GM_LOADALLSYS_USAGE,
            _ => messages::gm::GM_UNLOADALLSYS_USAGE,
        };
        return Err(slash_error(usage.into()));
    }
    if command != "loadsys" && !crate::config::game().gm.performance_testing {
        return Err(slash_error(messages::gm::GM_PERFORMANCE_TESTING_REQUIRED.into()));
    }
    if command == "unloadallsys" {
        let report = c.world.unload_idle_systems().await?;
        return Ok(messages::gm::gm_system_unload(
            report.total, report.unloaded, report.busy, report.changed, report.failed, report.unprocessed));
    }
    let scope = match command {
        "loadallsys" => PreloadScope::All,
        _ => {
            args::positive_character(&c.session)?;
            let Some(system) = args::current_system(&c.session) else {
                return Err(slash_error(messages::gm::GM_PRELOAD_NO_PUBLIC_SYSTEM.into()));
            };
            PreloadScope::CurrentAndNeighbors(system)
        }
    };
    let report = c.world.preload_systems(scope).await
        .ok_or_else(|| slash_error(messages::gm::GM_PRELOAD_NO_PUBLIC_SYSTEM.into()))?;
    Ok(messages::gm::gm_system_preload(
        report.total, report.originally_loaded, report.newly_loaded, report.failed, report.unprocessed))
}

#[cfg(all(test, feature = "tests-slash"))]
mod tests;
