//! Corporation and alliance authority for mutual wars, as used by
//! `eve/client/script/ui/services/wars/mutualWarInvites.py`.
//! Reads `game.characters`, `game.corporations` and `game.alliances` membership and roles.
//! NPC corporations, aggressive wars, allies and paid treaties are unsupported.
use infinity_core::{consts::ROLE_DIRECTOR, messages};
use crate::{dao::{alliances, characters::crimewatch}, db::executor::Executor, services::{self, CallError}};

fn refused() -> CallError { services::notify_error(messages::wars::AUTHORITY) }

pub(crate) async fn owner(ex: &(impl Executor + ?Sized), actor: i64) -> Result<i64, CallError> {
    let corp = crimewatch::corporation(ex, actor).await?.ok_or_else(refused)?;
    if !services::corps::is_player_corp(corp) { return Err(refused()); }
    let alliance = alliances::live_corporation_alliance(ex, corp).await?.ok_or_else(refused)?;
    if let Some(alliance) = alliance {
        if !alliances::exists(ex, alliance).await? { return Err(refused()); }
        Ok(alliance)
    } else { Ok(corp) }
}
/// Lock the actor's current organization before war-owner business locks.
pub(super) async fn lock_actor(ex: &(impl Executor + ?Sized), actor: i64) -> Result<(), CallError> {
    let corp = crimewatch::corporation(ex, actor).await?.ok_or_else(refused)?;
    if !services::corps::is_player_corp(corp) { return Err(refused()); }
    let alliance = alliances::live_corporation_alliance(ex, corp).await?.ok_or_else(refused)?;
    let alliances: Vec<i64> = alliance.into_iter().collect();
    if !crate::dao::corporations::registry::lock_admission(ex, &alliances, &[corp], &[actor]).await? { return Err(refused()); }
    Ok(())
}


pub(crate) async fn require_owner(ex: &(impl Executor + ?Sized), actor: i64, owner: i64) -> Result<(), CallError> {
    let corp = crimewatch::corporation(ex, actor).await?.ok_or_else(refused)?;
    if corp == owner && services::corps::is_player_corp(corp)
        && alliances::live_corporation_alliance(ex, corp).await?.is_some()
        && services::has_role_or_is_ceo(ex, actor, corp, ROLE_DIRECTOR).await? { return Ok(()); }
    if alliances::exists(ex, owner).await? && services::alliances::executor_director(ex, actor, owner).await? { return Ok(()); }
    Err(refused())
}

pub(super) async fn valid_target(ex: &(impl Executor + ?Sized), owner: i64) -> Result<bool, CallError> {
    if alliances::exists(ex, owner).await? { return Ok(true); }
    Ok(services::corps::is_player_corp(owner) && matches!(alliances::live_corporation_alliance(ex, owner).await?, Some(None)))
}
