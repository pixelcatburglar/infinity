//! Personal plan operations for the public skill-plan client.
//! Uses game plan and tracking tables and the startup SDE; corporation plans are excluded.
use infinity_core::consts::UUID_LEN;
use crate::{dao::skills::{plans, plan_tracking}, db::{Db, outbox::Outbox}, sde::Sde, services::identity::Identity};
use std::sync::Arc;
use super::{Error, validation};

pub(crate) async fn create(identity: &Identity, db: &Db, sde: &Sde, id: &[u8], name: &str, description: &str, skills: &[i32], levels: &[i32]) -> Result<(), Error> {
    if !super::valid_id(id) { return Err(Error::Invalid); }
    if validation::text_units(name) > validation::MAX_NAME || validation::text_units(description) > validation::MAX_DESCRIPTION || !validation::sequence(sde, skills, levels) { return Err(Error::Precondition); }
    let char_id = identity.char_id;
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    let payload = Arc::new((name.to_owned(), description.to_owned(), skills.to_vec(), levels.to_vec()));
    db.transaction(|tx, _: &mut Outbox| {
        let payload = Arc::clone(&payload);
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, char_id).await?;
            if plans::all(tx, char_id).await?.len() >= validation::MAX_PLANS { return Err(Error::Precondition); }
            plans::create(tx, &id, char_id, &payload.0, &payload.1, &payload.2, &payload.3).await?;
            Ok(())
        })
    }, |v, _| async move { Ok(v) }).await
}
pub(crate) async fn all(identity: &Identity, db: &Db) -> Result<Vec<Vec<u8>>, Error> { Ok(plans::all(db, identity.char_id).await?) }
pub(crate) async fn get(identity: &Identity, db: &Db, id: &[u8], shared: bool) -> Result<Option<plans::Plan>, Error> {
    if !super::valid_id(id) { return Err(Error::Invalid); }
    let row = plans::get(db, id, if shared { None } else { Some(identity.char_id) }).await?;
    if row.is_none() && !shared { return Err(Error::Missing); }
    Ok(row)
}
pub(crate) async fn active(identity: &Identity, db: &Db) -> Result<(Option<plan_tracking::Tracking>, Option<plans::Plan>), Error> {
    let tracking = plan_tracking::get(db, identity.char_id).await?;
    let row = if let Some(id) = tracking.as_ref().and_then(|t| t.personal_plan_id.as_ref()) { plans::get(db, id, Some(identity.char_id)).await? } else { None };
    Ok((tracking, row))
}
pub(crate) async fn set_active(identity: &Identity, db: &Db, sde: &Sde, id: &[u8]) -> Result<(), Error> {
    if id.len() != UUID_LEN { return Err(Error::Invalid); }
    let prefix = UUID_LEN - std::mem::size_of::<u32>();
    let certified = if id[..prefix].iter().all(|b| *b == 0) { let n = u32::from_be_bytes(id[prefix..].try_into().map_err(|_| Error::Invalid)?); (n != 0 && sde.skill_plans.contains(n)).then_some(i64::from(n)) } else { None };
    let char_id = identity.char_id;
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        crate::dao::characters::selection::lock_character(tx, char_id).await?;
        if id.iter().all(|b| *b == 0) { plan_tracking::clear(tx, char_id).await?; }
        else if let Some(certified) = certified { plan_tracking::set(tx, char_id, None, Some(certified)).await?; }
        else {
            plans::lock(tx, &id).await?;
            if plans::get(tx, &id, Some(char_id)).await?.is_none() { return Err(Error::Missing); }
            plan_tracking::set(tx, char_id, Some(&id), None).await?;
        }
        Ok(())
    }), |v, _| async move { Ok(v) }).await
}
pub(crate) async fn delete(identity: &Identity, db: &Db, id: &[u8]) -> Result<(), Error> {
    if !super::valid_id(id) { return Err(Error::Invalid); }
    let char_id = identity.char_id;
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        crate::dao::characters::selection::lock_character(tx, char_id).await?;
        plans::lock(tx, &id).await?;
        if plans::delete(tx, char_id, &id).await? != 1 { return Err(Error::Missing); }
        Ok(())
    }), |v, _| async move { Ok(v) }).await
}
pub(crate) async fn set_field(identity: &Identity, db: &Db, id: &[u8], field: plans::Field, value: &str) -> Result<(), Error> {
    let limit = match field { plans::Field::Name => validation::MAX_NAME, plans::Field::Description => validation::MAX_DESCRIPTION };
    if validation::text_units(value) > limit { return Err(Error::Precondition); }
    if !super::valid_id(id) { return Err(Error::Invalid); }
    let char_id = identity.char_id;
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    let value: Arc<str> = Arc::from(value);
    db.transaction(|tx, _: &mut Outbox| {
        let value = Arc::clone(&value);
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, char_id).await?;
            plans::lock(tx, &id).await?;
            if plans::set_field(tx, char_id, &id, field, &value).await? != 1 { return Err(Error::Missing); }
            Ok(())
        })
    }, |v, _| async move { Ok(v) }).await
}
pub(crate) async fn set_requirements(identity: &Identity, db: &Db, sde: &Sde, id: &[u8], skills: &[i32], levels: &[i32]) -> Result<(), Error> {
    if !super::valid_id(id) { return Err(Error::Invalid); }
    if !validation::sequence(sde, skills, levels) { return Err(Error::Precondition); }
    let char_id = identity.char_id;
    let id: [u8; UUID_LEN] = id.try_into().map_err(|_| Error::Invalid)?;
    let requirements = Arc::new((skills.to_vec(), levels.to_vec()));
    db.transaction(|tx, _: &mut Outbox| {
        let requirements = Arc::clone(&requirements);
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, char_id).await?;
            plans::lock(tx, &id).await?;
            if plans::set_requirements(tx, char_id, &id, &requirements.0, &requirements.1).await? != 1 { return Err(Error::Missing); }
            Ok(())
        })
    }, |v, _| async move { Ok(v) }).await
}
