//! Personal milestone definitions and viewer-specific reconciliation.
//! Mirrors personalMilestoneRequest.py using game plan and progress tables; corporation milestones are excluded.
use std::sync::Arc;
use infinity_core::consts::UUID_LEN;
use crate::{dao::skills::{plan_milestones as dao, plans}, db::{Db, outbox::Outbox}, sde::Sde, services::{identity::Identity, skills::expert_systems}, space::World};
use super::{Error, progress, validation};

async fn advance(identity: &Identity, db: &Db, world: &World) -> Result<(), Error> {
    expert_systems::advance_expired(db, world, world.sde(), identity.char_id, crate::client::filetime_now(), |n| n.dispatch(world, None)).await.map_err(Error::from)
}
pub(crate) async fn list(identity: &Identity, db: &Db, world: &World, parent: &[u8]) -> Result<Vec<dao::Milestone>, Error> {
    if !super::valid_id(parent) { return Err(Error::Invalid); }
    advance(identity, db, world).await?;
    let char_id = identity.char_id;
    let parent: [u8; UUID_LEN] = parent.try_into().map_err(|_| Error::Invalid)?;
    let sde = Arc::clone(world.sde());
    db.transaction(|tx, _: &mut Outbox| {
        let sde = Arc::clone(&sde);
        Box::pin(async move {
            progress::reconcile_tx(tx, &sde, char_id, Some(&parent)).await.map_err(Error::from)?;
            if plans::get(tx, &parent, None).await?.is_none() { return Err(Error::Missing); }
            Ok(dao::list(tx, &parent).await?)
        })
    }, |v, _| async move { Ok(v) }).await
}
pub(crate) async fn create(identity: &Identity, db: &Db, sde: &Sde, world: &World, id: &[u8], parent: &[u8], kind: &str, type_id: i32, level: Option<i32>, description: &str) -> Result<(), Error> {
    if !super::valid_id(id) || !super::valid_id(parent) { return Err(Error::Invalid); }
    if !matches!((kind, level), ("type", None) | ("skill", Some(_))) { return Err(Error::Invalid); }
    if !validation::target(sde, type_id, level) { return Err(Error::Precondition); }
    let required = validation::required(sde, type_id, level).ok_or(Error::Precondition)?;
    advance(identity, db, world).await?;
    let char_id = identity.char_id;
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    let parent: [u8; UUID_LEN] = parent.try_into().map_err(|_| Error::Invalid)?;
    let sde = Arc::clone(world.sde());
    let payload = Arc::new((required, kind.to_owned(), description.to_owned()));
    db.transaction(|tx, _: &mut Outbox| {
        let sde = Arc::clone(&sde);
        let payload = Arc::clone(&payload);
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, char_id).await?;
            plans::lock(tx, &parent).await?;
            let plan = plans::get(tx, &parent, Some(char_id)).await?.ok_or(Error::Missing)?;
            let rows = dao::list(tx, &parent).await?;
            if rows.len() >= validation::MAX_MILESTONES || rows.iter().any(|row| row.kind == payload.1 && row.type_id == type_id && row.level == level) || payload.0.iter().any(|(id, level)| *level > 0 && !plan.skill_ids.iter().zip(&plan.levels).any(|(skill, actual)| skill == id && actual >= level)) { return Err(Error::Precondition); }
            dao::create(tx, &id, &parent, &payload.1, type_id, level).await?;
            if !payload.2.is_empty() { dao::set_description(tx, &id, char_id, &payload.2).await?; }
            progress::reconcile_tx(tx, &sde, char_id, Some(&parent)).await.map_err(Error::from)?;
            Ok(())
        })
    }, |v, _| async move { Ok(v) }).await
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
            let changed = if let Some(description) = description { dao::set_description(tx, &id, char_id, &description).await? } else { dao::delete(tx, &id, char_id).await? };
            if changed != 1 { return Err(Error::Missing); }
            Ok(())
        })
    }, |v, _| async move { Ok(v) }).await
}
