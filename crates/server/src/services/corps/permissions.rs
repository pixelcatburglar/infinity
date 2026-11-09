//! Current corporation authority and scoped role evaluation.
//! Mirrors corporation `corp_util.CanEditRole` and `CanEditBase` against game member and title rows.
//! Permissions are read from the database at each protected operation, not from a client session.
use infinity_core::consts::ROLE_DIRECTOR;
use crate::{dao::characters::roles, db::executor::Executor, services::{db_err, CallError}};

pub(crate) type Authority = roles::Authority;

pub(crate) async fn load(ex: &(impl Executor + ?Sized), character: i64) -> Result<Option<Authority>, CallError> {
    roles::authority(ex, character).await.map_err(db_err)
}
pub(crate) fn active_ceo(authority: &Authority, character: i64) -> bool {
    authority.live && authority.ceo == Some(character) && authority.roles[0] & ROLE_DIRECTOR != 0
}
pub(crate) async fn is_active_ceo(ex: &(impl Executor + ?Sized), character: i64, corporation: i64) -> Result<bool, CallError> {
    Ok(load(ex, character).await?.is_some_and(|authority| authority.corporation == corporation && active_ceo(&authority, character)))
}
pub(crate) fn at_station(authority: &Authority, station: i64) -> i64 {
    let scope = if authority.headquarters == Some(station) { 1 } else if authority.base == Some(station) { 2 } else { 3 };
    authority.roles[0] | authority.roles[scope]
}
pub(crate) fn allows_at(authority: &Authority, corporation: i64, station: i64, role: i64) -> bool {
    authority.live && authority.corporation == corporation && (authority.roles[0] & ROLE_DIRECTOR != 0 || at_station(authority, station) & role != 0)
}
pub(crate) fn allows(authority: &Authority, corporation: i64, role: i64) -> bool {
    authority.live && authority.corporation == corporation && (authority.roles[0] & ROLE_DIRECTOR != 0 || authority.roles[0] & role != 0)
}
pub(crate) async fn has(ex: &(impl Executor + ?Sized), character: i64, corporation: i64, role: i64) -> Result<bool, CallError> {
    Ok(load(ex, character).await?.is_some_and(|a| allows(&a, corporation, role)))
}
pub(crate) async fn has_at(ex: &(impl Executor + ?Sized), character: i64, corporation: i64, station: i64, role: i64) -> Result<bool, CallError> {
    Ok(load(ex, character).await?.is_some_and(|a| allows_at(&a, corporation, station, role)))
}

/// Client `corp_util.CanEditRole`; scope is global, HQ, base or other.
pub(super) fn can_edit_role(role: i64, grantable: bool, target_ceo: bool, target_director: bool, actor_ceo: bool,
                 actor_director: bool, scope: usize, actor_base: Option<i64>, target_base: Option<i64>, grants: [i64; 4]) -> bool {
    if grantable {
        return role != ROLE_DIRECTOR && actor_director && !target_ceo && !target_director;
    }
    if target_ceo || (target_director && !actor_ceo) || (target_director && role != ROLE_DIRECTOR) {
        return false;
    }
    if scope == 2 {
        if actor_ceo || (actor_director && !target_director) { return true; }
        if actor_base != target_base { return false; }
    }
    grants.get(scope).is_some_and(|mask| mask & role == role)
}

/// Client `corp_util.CanEditBase`.
pub(super) fn can_edit_base(actor_ceo: bool, actor_director: bool) -> bool {
    actor_ceo || actor_director
}

#[cfg(test)]
mod tests;
