//! GM skill commands forwarded by the slash service.
//! Skills and queues are persisted and notified by the skills service.
//! Quoted skill and character names are parsed by the shared argument parser.
//! `/setattr` writes all five base attributes through the shared respec
//! transaction and reports the integer base that was stored.
//! `/backintime` irreversibly restores the race's trained starter set and
//! discards the whole queue and free skill points.
use super::{args::{self, Args}, slash_error, CallError, Client};
use infinity_core::messages;
use crate::{services::skills::{gm::{self, Change}, respec}};

use infinity_core::consts::dogma::MAX_SKILL_LEVEL as MAX_LEVEL;

fn level(text: Option<&str>) -> Result<i32, CallError> {
    match text {
        None => Ok(MAX_LEVEL),
        Some(text) => text.parse::<i32>().ok().filter(|n| (0..=MAX_LEVEL).contains(n))
            .ok_or_else(|| slash_error(messages::gm::GM_SKILL_LEVEL_INVALID.into())),
    }
}

async fn set_attributes(c: &mut Client, args: &[String]) -> Result<String, CallError> {
    if !(1..=2).contains(&args.len()) { return Err(slash_error(messages::gm::GM_SETATTR_USAGE.into())); }
    let parsed = Args::new(args);
    let value = parsed.f64(0).map_err(|_| slash_error(messages::gm::GM_SETATTR_USAGE.into()))?;
    let target = if args.len() == 2 { parsed.target(c, 1).await? } else { args::target(c, "me").await? };
    let base = respec::gm_set_attributes(&c.db, &c.world, &c.sde, target, value).await?;
    Ok(messages::gm::gm_setattr_done(target, base))
}

pub(super) async fn command(c: &mut Client, command: &str, args: &[String]) -> Result<String, CallError> {
    if command == "setattr" { return set_attributes(c, args).await; }
    let invalid = || slash_error(messages::gm::gm_skill_usage(command));
    let parsed = Args::new(args);
    let (target, change) = match command {
        "giveskill" if (2..=3).contains(&args.len()) => {
            parsed.required(0).map_err(|_| invalid())?;
            let target = parsed.target(c, 0).await?;
            let level = level(args.get(2).map(String::as_str))?;
            let skill = parsed.required(1).map_err(|_| invalid())?;
            let levels = if skill.eq_ignore_ascii_case("all") {
                gm::all_levels(&c.sde, true, level)
            } else if skill.eq_ignore_ascii_case("super") {
                gm::all_levels(&c.sde, false, level)
            } else {
                let (type_id, _) = parsed.type_id(c, 1).await?;
                if !gm::is_skill(&c.sde, type_id) { return Err(slash_error(messages::gm::gm_skill_not_skill(skill))); }
                vec![(type_id, level)]
            };
            (target, Change::Set(levels))
        }
        "allskills" if args.len() <= 1 => {
            let target = if args.is_empty() { args::target(c, "me").await? } else {
                parsed.required(0).map_err(|_| invalid())?;
                parsed.target(c, 0).await?
            };
            (target, Change::Set(gm::all_levels(&c.sde, true, MAX_LEVEL)))
        }
        "removeskill" if args.len() == 2 => {
            parsed.required(0).map_err(|_| invalid())?;
            parsed.required(1).map_err(|_| invalid())?;
            let target = parsed.target(c, 0).await?;
            let change = if args[1].eq_ignore_ascii_case("all") { Change::RemoveAll } else {
                let (type_id, _) = parsed.type_id(c, 1).await?;
                if !gm::is_skill(&c.sde, type_id) { return Err(slash_error(messages::gm::gm_skill_not_skill(&args[1]))); }
                Change::Remove(vec![type_id])
            };
            (target, change)
        }
        "givestarterskills" if args.len() == 1 => {
            parsed.required(0).map_err(|_| invalid())?;
            (parsed.target(c, 0).await?, Change::Starters)
        }
        "backintime" if args.len() <= 1 => {
            let target = if args.is_empty() { args::target(c, "me").await? } else {
                parsed.required(0).map_err(|_| invalid())?;
                parsed.target(c, 0).await?
            };
            (target, Change::ResetToStarters)
        }
        _ => return Err(invalid()),
    };
    gm::apply(c, target, change).await?;
    Ok(messages::gm::gm_skill_done(command, target))
}

#[cfg(all(test, feature = "tests-slash"))] mod tests;
