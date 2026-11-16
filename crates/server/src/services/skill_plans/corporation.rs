//! Corporation skill plan operations for `corpSkillPlanRequest.py` and write
//! authority for `corpMilestoneRequest.py`.
//! Writes require a current member of a live player corporation holding the global
//! skill-plan role bit; the Director fallback never applies. Reads require current
//! membership only. Notice payloads and wire shapes are the gateway's concern;
//! this module owns scope, quotas, lock order and post-commit publication ordering.
use std::sync::Arc;
use infinity_core::consts::UUID_LEN;
use crate::{dao::{characters::members, corporations::registry, skills::plans::{self, Owner}}, db::{executor::Executor, outbox::{self, Outbox}, Db}, sde::Sde, services::{corps::permissions, identity::Identity}, space::World};
use super::{Error, publication, validation};

/// Client appConst.py:372 corporation skill-plan manager bit: the global role
/// gate for plan and milestone edits. Direct and unblocked title-derived grants
/// count; grantable bits and scoped variants do not.
const ROLE_SKILL_PLAN_MANAGER: i64 = 1 << 62;

/// The caller's current live corporation, or none when its row is deleted.
/// NPC corporations are normal memberships with no plans of their own.
pub(super) async fn read_corporation(ex: &(impl Executor + ?Sized), character: i64) -> Result<Option<i64>, Error> {
    Ok(members::organization(ex, character).await?.map(|org| org.corporation_id))
}

/// Admission lock then authority re-read inside the write transaction: the caller
/// must still be a member of the same live corporation holding the role bit.
/// Refuses a stale admission or a revoked grant without using the Director fallback.
pub(super) async fn authorize_edit(ex: &(impl Executor + ?Sized), character: i64, corporation: i64) -> Result<(), Error> {
    if !registry::lock_admission(ex, &[], &[corporation], &[character]).await? { return Err(Error::Precondition); }
    let allowed = permissions::load(ex, character).await?
        .is_some_and(|a| a.live && a.corporation == corporation && a.roles[0] & ROLE_SKILL_PLAN_MANAGER != 0);
    allowed.then_some(()).ok_or(Error::Precondition)
}

/// Summary rows the caller's live membership may list; NPC and dissolved
/// memberships see none.
pub(crate) async fn all(identity: &Identity, db: &Db) -> Result<Vec<plans::Summary>, Error> {
    Ok(plans::summaries_for_member(db, identity.char_id).await?)
}

/// One plan scoped to the caller's current corporation; the membership
/// predicate is part of the same statement as the plan row. `shared` reports
/// absence instead of refusing so foreign or invisible identifiers never leak.
pub(crate) async fn get(identity: &Identity, db: &Db, id: &[u8], shared: bool) -> Result<Option<plans::Plan>, Error> {
    if !super::valid_id(id) { return Err(Error::Invalid); }
    let row = plans::plan_for_member(db, identity.char_id, id).await?;
    if row.is_none() && !shared { return Err(Error::Missing); }
    Ok(row)
}

pub(crate) async fn create(identity: &Identity, db: &Db, sde: &Sde, world: &World, id: &[u8], name: &str, description: &str, skills: &[i32], levels: &[i32], category: &[u8], notice: Arc<publication::Notice>) -> Result<(), Error> {
    if !super::valid_id(id) { return Err(Error::Invalid); }
    let Ok(category) = <&[u8; UUID_LEN]>::try_from(category) else { return Err(Error::Invalid) };
    if validation::text_units(name) > validation::MAX_NAME || validation::text_units(description) > validation::MAX_DESCRIPTION || !validation::sequence(sde, skills, levels) { return Err(Error::Precondition); }
    if !validation::category(sde, category) { return Err(Error::Precondition); }
    let char_id = identity.char_id;
    let Some(corp) = read_corporation(db, char_id).await? else { return Err(Error::Precondition) };
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    let payload = Arc::new((name.to_owned(), description.to_owned(), skills.to_vec(), levels.to_vec(), *category));
    let _gate = world.notice_hub().corporation_publication(corp).await;
    db.transaction(|tx, outbox: &mut Outbox| {
        let notice = Arc::clone(&notice);
        let payload = Arc::clone(&payload);
        Box::pin(async move {
            authorize_edit(tx, char_id, corp).await?;
            if plans::all(tx, Owner::Corporation(corp)).await?.len() >= validation::MAX_CORPORATION_PLANS { return Err(Error::Precondition); }
            plans::create(tx, &id, Owner::Corporation(corp), &payload.0, &payload.1, &payload.2, &payload.3, Some(&payload.4)).await?;
            publication::publish(tx, outbox, corp, notice.as_ref()).await
        })
    }, |value, outbox| async move { outbox::dispatch_world(value, outbox, world).map_err(Error::from) }).await
}

pub(crate) async fn delete(identity: &Identity, db: &Db, world: &World, id: &[u8], notice: Arc<publication::Notice>) -> Result<(), Error> {
    if !super::valid_id(id) { return Err(Error::Invalid); }
    let char_id = identity.char_id;
    let Some(corp) = read_corporation(db, char_id).await? else { return Err(Error::Precondition) };
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    super::change_plan(db, char_id, id, super::PlanMutation::Delete, Some((corp, world, notice))).await
}

pub(crate) async fn set_field(identity: &Identity, db: &Db, world: &World, id: &[u8], field: plans::Field, value: &str, notice: Arc<publication::Notice>) -> Result<(), Error> {
    let limit = match field { plans::Field::Name => validation::MAX_NAME, plans::Field::Description => validation::MAX_DESCRIPTION };
    if validation::text_units(value) > limit { return Err(Error::Precondition); }
    if !super::valid_id(id) { return Err(Error::Invalid); }
    let char_id = identity.char_id;
    let Some(corp) = read_corporation(db, char_id).await? else { return Err(Error::Precondition) };
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    let value: Arc<str> = Arc::from(value);
    super::change_plan(db, char_id, id, super::PlanMutation::Field { field, value }, Some((corp, world, notice))).await
}

pub(crate) async fn set_requirements(identity: &Identity, db: &Db, sde: &Sde, world: &World, id: &[u8], skills: &[i32], levels: &[i32], notice: Arc<publication::Notice>) -> Result<(), Error> {
    if !super::valid_id(id) { return Err(Error::Invalid); }
    if !validation::sequence(sde, skills, levels) { return Err(Error::Precondition); }
    let char_id = identity.char_id;
    let Some(corp) = read_corporation(db, char_id).await? else { return Err(Error::Precondition) };
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    let requirements = Arc::new((skills.to_vec(), levels.to_vec()));
    super::change_plan(db, char_id, id, super::PlanMutation::Requirements(requirements), Some((corp, world, notice))).await
}

pub(crate) async fn set_category(identity: &Identity, db: &Db, sde: &Sde, world: &World, id: &[u8], category: &[u8], notice: Arc<publication::Notice>) -> Result<(), Error> {
    if !super::valid_id(id) { return Err(Error::Invalid); }
    let Ok(category) = <&[u8; UUID_LEN]>::try_from(category) else { return Err(Error::Invalid) };
    if !validation::category(sde, category) { return Err(Error::Precondition); }
    let char_id = identity.char_id;
    let Some(corp) = read_corporation(db, char_id).await? else { return Err(Error::Precondition) };
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    let category: [u8; UUID_LEN] = *category;
    let _gate = world.notice_hub().corporation_publication(corp).await;
    db.transaction(|tx, outbox: &mut Outbox| {
        let notice = Arc::clone(&notice);
        Box::pin(async move {
            authorize_edit(tx, char_id, corp).await?;
            plans::lock(tx, &id).await?;
            if plans::set_category(tx, corp, &id, &category).await? != 1 { return Err(Error::Missing); }
            publication::publish(tx, outbox, corp, notice.as_ref()).await
        })
    }, |value, outbox| async move { outbox::dispatch_world(value, outbox, world).map_err(Error::from) }).await
}

#[cfg(all(test, feature = "tests-skills"))] mod tests;
