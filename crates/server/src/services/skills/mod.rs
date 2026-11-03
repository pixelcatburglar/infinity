//! Skill service routing and shared mechanics, mirroring characterskills/util.py.
//! Submodules use game.skills, game.skill_queue, game.characters, game.items,
//! game.item_attributes and game.jobs with startup SDE data. Training and timed
//! augmentations are durable; legacy boosters without timing have no known expiry.
//! Station clone custody preserves implants per body; boosters remain on the current body.
use super::{db_err, keyval, Call, CallError, CallResult};
use crate::client::Client;
pub(crate) mod alpha_injections;
pub(crate) mod attributes;
pub(crate) mod brain_items;
pub(crate) mod body_change;
mod brain_reads;
pub(crate) mod formulas;
pub(crate) mod requirements;
pub(crate) mod starting;
mod non_diminishing_injection_mgr;
mod skill_mgr2;
mod skill_handler;
pub(crate) mod training;
mod view;
mod timeline;
pub(crate) mod completion;
pub(crate) mod gm;
mod history;
mod free_points;
pub(crate) mod books;
mod learning;
mod purchase;
mod trading;
mod respec;
#[cfg(test)]
mod tests;

/// Internal bound-object name for skillMgr2.GetMySkillHandler.
pub const SKILL_HANDLER: &str = "skillHandler";

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service == "skillMgr2" { skill_mgr2::dispatch(c, call).await }
    else if let Some(result) = non_diminishing_injection_mgr::dispatch(c, call).await { Some(result) }
    else { skill_handler::dispatch(c, call).await }
}
