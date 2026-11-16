//! Personal and corporation milestone definitions and viewer-specific reconciliation.
//! Mirrors `personalMilestoneRequest.py` and `corpMilestoneRequest.py` using game
//! plan and progress tables; completion notifications are not produced.
//! Personal entries accept only personal plans, corporation entries only plans
//! of the caller's live corporation; every other identifier is absent, not refused.
use std::sync::Arc;
use infinity_core::consts::UUID_LEN;
use crate::{dao::{skills::{plan_milestones as dao, plans::{self, Owner}}}, db::{executor::Executor, outbox::{self, Outbox}, Db}, sde::Sde, services::{identity::Identity, skills::expert_systems}, space::World};
use super::{corporation, Error, progress, publication, validation};

async fn advance(identity: &Identity, db: &Db, world: &World) -> Result<(), Error> {
    expert_systems::advance_expired(db, world, world.sde(), identity.char_id, crate::client::filetime_now(), |n| n.dispatch(world, None)).await.map_err(Error::from)
}

/// Read visibility: personal entries accept personal plans, corporation entries
/// accept plans of the caller's current live corporation, and nothing else.
async fn visible(ex: &(impl Executor + ?Sized), character: i64, plan: &plans::Plan, corp_scope: bool) -> Result<bool, Error> {
    Ok(match (corp_scope, plan.owner) {
        (false, Owner::Character(_)) => true,
        (true, Owner::Corporation(corp)) => corporation::read_corporation(ex, character).await?.is_some_and(|viewer| viewer == corp),
        _ => false,
    })
}

async fn list_scoped(identity: &Identity, db: &Db, world: &World, parent: &[u8], corp_scope: bool) -> Result<Vec<dao::Milestone>, Error> {
    if !super::valid_id(parent) { return Err(Error::Invalid); }
    advance(identity, db, world).await?;
    let char_id = identity.char_id;
    let parent: [u8; UUID_LEN] = parent.try_into().map_err(|_| Error::Invalid)?;
    let sde = Arc::clone(world.sde());
    db.transaction(|tx, _: &mut Outbox| {
        let sde = Arc::clone(&sde);
        Box::pin(async move {
            progress::reconcile_tx(tx, &sde, char_id, Some(&parent)).await.map_err(Error::from)?;
            let plan = plans::get(tx, &parent, None).await?.ok_or(Error::Missing)?;
            if !visible(tx, char_id, &plan, corp_scope).await? { return Err(Error::Missing); }
            Ok(dao::list(tx, &parent).await?)
        })
    }, |v, _| async move { Ok(v) }).await
}
pub(crate) async fn list(identity: &Identity, db: &Db, world: &World, parent: &[u8]) -> Result<Vec<dao::Milestone>, Error> { list_scoped(identity, db, world, parent, false).await }
pub(crate) async fn corporation_list(identity: &Identity, db: &Db, world: &World, parent: &[u8]) -> Result<Vec<dao::Milestone>, Error> { list_scoped(identity, db, world, parent, true).await }

/// Target rules shared by personal and corporation milestone creation. The caller
/// holds the parent row lock and has already authorized the write scope.
async fn insert_target(ex: &(impl Executor + ?Sized), plan: &plans::Plan, id: &[u8], kind: &str, type_id: i32, level: Option<i32>, required: &std::collections::BTreeMap<i32, i32>, description: &str) -> Result<(), Error> {
    let rows = dao::list(ex, &plan.plan_id).await?;
    if rows.len() >= validation::MAX_MILESTONES
        || rows.iter().any(|row| row.kind == kind && row.type_id == type_id && row.level == level)
        || required.iter().any(|(id, level)| *level > 0 && !plan.skill_ids.iter().zip(&plan.levels).any(|(skill, actual)| skill == id && actual >= level)) {
        return Err(Error::Precondition);
    }
    dao::create(ex, id, &plan.plan_id, kind, type_id, level).await?;
    if !description.is_empty() { dao::set_description(ex, id, plan.owner, description).await?; }
    Ok(())
}
fn valid_target(sde: &Sde, id: &[u8], parent: &[u8], kind: &str, type_id: i32, level: Option<i32>) -> Result<std::collections::BTreeMap<i32, i32>, Error> {
    if !super::valid_id(id) || !super::valid_id(parent) { return Err(Error::Invalid); }
    if !matches!((kind, level), ("type", None) | ("skill", Some(_))) { return Err(Error::Invalid); }
    if !validation::target(sde, type_id, level) { return Err(Error::Precondition); }
    validation::required(sde, type_id, level).ok_or(Error::Precondition)
}

pub(crate) async fn create(identity: &Identity, db: &Db, sde: &Sde, world: &World, id: &[u8], parent: &[u8], kind: &str, type_id: i32, level: Option<i32>, description: &str) -> Result<(), Error> {
    let required = valid_target(sde, id, parent, kind, type_id, level)?;
    advance(identity, db, world).await?;
    let char_id = identity.char_id;
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    let parent: [u8; UUID_LEN] = parent.try_into().map_err(|_| Error::Invalid)?;
    let sde = Arc::clone(world.sde());
    let payload = Arc::new((kind.to_owned(), description.to_owned()));
    db.transaction(|tx, _: &mut Outbox| {
        let sde = Arc::clone(&sde);
        let required = required.clone();
        let payload = Arc::clone(&payload);
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, char_id).await?;
            plans::lock(tx, &parent).await?;
            let plan = plans::get(tx, &parent, Some(Owner::Character(char_id))).await?.ok_or(Error::Missing)?;
            insert_target(tx, &plan, &id, &payload.0, type_id, level, &required, &payload.1).await?;
            progress::reconcile_tx(tx, &sde, char_id, Some(&parent)).await.map_err(Error::from)?;
            Ok(())
        })
    }, |v, _| async move { Ok(v) }).await
}

pub(crate) async fn corporation_create(identity: &Identity, db: &Db, sde: &Sde, world: &World, id: &[u8], parent: &[u8], kind: &str, type_id: i32, level: Option<i32>, description: &str, notice: Arc<publication::Notice>) -> Result<(), Error> {
    let required = valid_target(sde, id, parent, kind, type_id, level)?;
    advance(identity, db, world).await?;
    let char_id = identity.char_id;
    // Plan owners are immutable, so the pre-transaction read chooses the gate correctly.
    let Owner::Corporation(corp) = plans::get(db, parent, None).await?.ok_or(Error::Missing)?.owner else { return Err(Error::Missing) };
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    let parent: [u8; UUID_LEN] = parent.try_into().map_err(|_| Error::Invalid)?;
    let sde = Arc::clone(world.sde());
    let payload = Arc::new((kind.to_owned(), description.to_owned()));
    let _gate = world.notice_hub().corporation_publication(corp).await;
    db.transaction(|tx, outbox: &mut Outbox| {
        let sde = Arc::clone(&sde);
        let notice = Arc::clone(&notice);
        let required = required.clone();
        let payload = Arc::clone(&payload);
        Box::pin(async move {
            corporation::authorize_edit(tx, char_id, corp).await?;
            plans::lock(tx, &parent).await?;
            let plan = plans::get(tx, &parent, Some(Owner::Corporation(corp))).await?.ok_or(Error::Missing)?;
            insert_target(tx, &plan, &id, &payload.0, type_id, level, &required, &payload.1).await?;
            progress::reconcile_tx(tx, &sde, char_id, Some(&parent)).await.map_err(Error::from)?;
            publication::publish(tx, outbox, corp, notice.as_ref()).await
        })
    }, |value, outbox| async move { outbox::dispatch_world(value, outbox, world).map_err(Error::from) }).await
}

pub(crate) async fn delete(identity: &Identity, db: &Db, id: &[u8]) -> Result<(), Error> { change(identity, db, id, None).await }
pub(crate) async fn set_description(identity: &Identity, db: &Db, id: &[u8], description: &str) -> Result<(), Error> { change(identity, db, id, Some(description)).await }
async fn change(identity: &Identity, db: &Db, id: &[u8], description: Option<&str>) -> Result<(), Error> {
    if !super::valid_id(id) { return Err(Error::Invalid); }
    let char_id = identity.char_id;
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    let description: Option<Arc<str>> = description.map(Arc::from);
    db.transaction(|tx, _: &mut Outbox| {
        let description = description.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, char_id).await?;
            let row = dao::get(tx, &id).await?.ok_or(Error::Missing)?;
            plans::lock(tx, &row.plan_id).await?;
            let changed = if let Some(description) = description { dao::set_description(tx, &id, Owner::Character(char_id), &description).await? } else { dao::delete(tx, &id, Owner::Character(char_id)).await? };
            if changed != 1 { return Err(Error::Missing); }
            Ok(())
        })
    }, |v, _| async move { Ok(v) }).await
}

pub(crate) async fn corporation_delete(identity: &Identity, db: &Db, world: &World, id: &[u8], notice: Arc<publication::Notice>) -> Result<(), Error> { corporation_change(identity, db, world, id, None, notice).await }
pub(crate) async fn corporation_set_description(identity: &Identity, db: &Db, world: &World, id: &[u8], description: &str, notice: Arc<publication::Notice>) -> Result<(), Error> { corporation_change(identity, db, world, id, Some(description), notice).await }
async fn corporation_change(identity: &Identity, db: &Db, world: &World, id: &[u8], description: Option<&str>, notice: Arc<publication::Notice>) -> Result<(), Error> {
    if !super::valid_id(id) { return Err(Error::Invalid); }
    let char_id = identity.char_id;
    // The plan owner is immutable; resolving it before the gate is safe.
    let Some(target) = dao::get(db, id).await? else { return Err(Error::Missing) };
    let Owner::Corporation(corp) = plans::get(db, &target.plan_id, None).await?.ok_or(Error::Missing)?.owner else { return Err(Error::Missing) };
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    let description: Option<Arc<str>> = description.map(Arc::from);
    let _gate = world.notice_hub().corporation_publication(corp).await;
    db.transaction(|tx, outbox: &mut Outbox| {
        let notice = Arc::clone(&notice);
        let description = description.clone();
        Box::pin(async move {
            corporation::authorize_edit(tx, char_id, corp).await?;
            let row = dao::get(tx, &id).await?.ok_or(Error::Missing)?;
            plans::lock(tx, &row.plan_id).await?;
            let changed = if let Some(description) = description { dao::set_description(tx, &id, Owner::Corporation(corp), &description).await? } else { dao::delete(tx, &id, Owner::Corporation(corp)).await? };
            if changed != 1 { return Err(Error::Missing); }
            publication::publish(tx, outbox, corp, notice.as_ref()).await
        })
    }, |value, outbox| async move { outbox::dispatch_world(value, outbox, world).map_err(Error::from) }).await
}
