//! Atomic alliance application decisions and corporation admission.
//! The alliance and corporation registry clients use game alliance applications,
//! corporation membership and history through the DAOs. Governance is separate.
use super::*;
use crate::dao::corporations::registry;
use crate::services::corps::{is_player_corp, permissions};
use crate::services::identity::Identity;

#[derive(Clone)]
pub(crate) enum Decision {
    Manager { text: String, state: i32 },
    Force { role: i64 },
    Join,
}

fn refused() -> CallError { notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION) }

pub(crate) async fn actor_corp(ex: &(impl Executor + ?Sized), identity: &Identity) -> Result<i64, CallError> {
    let corp = registry::character_corp(ex, identity.char_id).await.map_err(db_err)?.ok_or_else(refused)?;
    if corp != identity.corp_id { return Err(refused()); }
    Ok(corp)
}

pub(crate) async fn director(ex: &(impl Executor + ?Sized), identity: &Identity, alliance: i64) -> Result<(), CallError> {
    let corp = actor_corp(ex, identity).await?;
    let (_, roles) = membership(ex, identity.char_id).await?;
    if roles & ROLE_DIRECTOR == 0 || dao::live_corporation_alliance(ex, corp).await.map_err(db_err)? != Some(Some(alliance))
        || dao::executor_corp(ex, alliance).await.map_err(db_err)? != Some(corp)
        || !dao::exists(ex, alliance).await.map_err(db_err)? { return Err(refused()); }
    Ok(())
}

pub(crate) async fn ceo(ex: &(impl Executor + ?Sized), identity: &Identity, corp: i64) -> Result<(), CallError> {
    if actor_corp(ex, identity).await? != corp || !is_player_corp(corp) || !permissions::is_active_ceo(ex, identity.char_id, corp).await? {
        return Err(notify_error(messages::PLAYER_CORP_CEO_REQUIRED));
    }
    Ok(())
}

/// Read private applications under the same authority locks as mutation.
pub(crate) async fn applications(db: &Db, identity: &Identity, alliance: i64) -> CallResult {
    let corp = actor_corp(db, identity).await?;
    let identity = *identity;
    db.transaction(|tx, _: &mut ()| {
        let identity = identity;
        Box::pin(async move {
            if !registry::lock_admission(tx, &[alliance], &[corp], &[identity.char_id]).await.map_err(db_err)? { return Err(refused()); }
            director(tx, &identity, alliance).await?;
            let rows = dao::applications(tx, dao::ApplicationFilter::Alliance(alliance)).await.map_err(db_err)?;
            let header = application_header();
            let rows = rows.into_iter().map(|r| (r.corporation, header.row(&application_line(r)))).collect();
            Ok(indexed_rowset(&header, "corporationID", rows))
        })
    }, |value, ()| async move { Ok(value) }).await
}

/// All entry points share the same ordered locks, guarded transition and membership effects.
pub(crate) async fn decide(db: &Db, world: &World, identity: &Identity, alliance: i64, corp: i64, decision: Decision) -> Result<(), CallError> {
    use crate::services::sovereignty::governance::{self, PreparedAllianceChange};
    loop {
        let now = filetime_now();
        let prepared = if matches!(&decision, Decision::Manager { state, .. } if *state == APP_REJECTED) {
            PreparedAllianceChange::NoOwnershipChange
        } else {
            match governance::prepare(db, corp, now).await {
                Err(error) if crate::services::sovereignty::is_stale(&error) => continue,
                result => result?,
            }
        };
        if matches!(prepared, PreparedAllianceChange::NoOwnershipChange) {
            let admitted = governance::admit(world, prepared, Some(identity.char_id))?;
            match decide_inner(db, world, identity, alliance, corp, decision.clone(), now, admitted).await {
                Err(error) if crate::services::sovereignty::is_stale(&error) => continue,
                result => return result,
            }
        }
        let db = db.clone();
        let world = world.shared().ok_or_else(refused)?;
        let identity = *identity;
        return tokio::spawn(async move {
            let mut prepared = prepared;
            let mut now = now;
            loop {
                let result = if matches!(prepared, PreparedAllianceChange::NoOwnershipChange) {
                    let admitted = governance::admit(&world, prepared, Some(identity.char_id))?;
                    decide_inner(&db, &world, &identity, alliance, corp, decision.clone(), now, admitted).await
                } else {
                    let _market = crate::services::market::lock().await;
                    let admitted = governance::admit(&world, prepared, Some(identity.char_id))?;
                    decide_inner(&db, &world, &identity, alliance, corp, decision.clone(), now, admitted).await
                };
                match result {
                    Err(error) if crate::services::sovereignty::is_stale(&error) => {
                        now = filetime_now();
                        prepared = loop {
                            match governance::prepare(&db, corp, now).await {
                                Err(error) if crate::services::sovereignty::is_stale(&error) => continue,
                                result => break result?,
                            }
                        };
                    }
                    result => return result,
                }
            }
        }).await.map_err(db_err)?;
    }
}

async fn decide_inner(db: &Db, world: &World, identity: &Identity, alliance: i64, corp: i64, decision: Decision, now: i64, admitted: crate::services::sovereignty::governance::AdmittedAllianceChange) -> Result<(), CallError> {
    let actor = actor_corp(db, identity).await?;
    let mut alliances: Vec<i64> = dao::applications(db, dao::ApplicationFilter::Corporation(corp)).await.map_err(db_err)?.into_iter().map(|a| a.alliance).collect();
    alliances.push(alliance);
    alliances.sort_unstable(); alliances.dedup();
    
    let identity = *identity;
    let admitted = std::sync::Arc::new(admitted);
    db.transaction(|tx, outbox| {
        let alliances = alliances.clone();
        let decision = decision.clone();
        let identity = identity;
        let admitted = std::sync::Arc::clone(&admitted);
        Box::pin(async move {
            if !registry::lock_admission(tx, &alliances, &[actor, corp], &[identity.char_id]).await.map_err(db_err)? { return Err(refused()); }
            dao::lock_applications(tx, corp).await.map_err(db_err)?;
            let current = dao::applications(tx, dao::ApplicationFilter::Corporation(corp)).await.map_err(db_err)?;
            if current.iter().any(|a| alliances.binary_search(&a.alliance).is_err()) { return Err(refused()); }
            match &decision {
                Decision::Manager { .. } => director(tx, &identity, alliance).await?,
                Decision::Force { role } => { actor_corp(tx, &identity).await?; if role & infinity_core::consts::GM_ROLES == 0 { return Err(refused()); } },
                Decision::Join => ceo(tx, &identity, corp).await?,
            }
            if !dao::exists(tx, alliance).await.map_err(db_err)? { return Err(notify_error(messages::ALLIANCE_NOT_FOUND)); }
            let allied = dao::live_corporation_alliance(tx, corp).await.map_err(db_err)?.ok_or_else(|| notify_error(messages::CORP_NOT_FOUND))?;
            let application = match &decision {
                Decision::Join => None,
                _ => Some(dao::application(tx, alliance, corp).await.map_err(db_err)?.filter(|a| a.state == APP_NEW).ok_or_else(|| notify_error(messages::ALLIANCE_APPLICATION_NOT_FOUND))?),
            };
            let rejecting = matches!(&decision, Decision::Manager { state, .. } if *state == APP_REJECTED);
            if !rejecting && allied.is_some() { return Err(notify_error(messages::TARGET_CORP_ALREADY_IN_ALLIANCE)); }
            if let Decision::Manager { state, .. } = &decision {
                if *state != APP_ACCEPTED && *state != APP_REJECTED { return Err(notify_error(messages::ALLIANCE_APPLICATION_STATUS_INVALID)); }
            }
            let old_alliance = if rejecting { None } else { alliance_line_of(tx, alliance).await? };
            if !rejecting { set_alliance(tx, outbox, corp, Some(alliance), now, &admitted).await?; }
            if let Some(application) = application {
                let text = match &decision { Decision::Manager { text, .. } => text.as_str(), _ => application.text.as_str() };
                let state = if rejecting { APP_REJECTED } else { APP_EFFECTIVE };
                if dao::update_application(tx, alliance, corp, text, state, APP_NEW).await.map_err(db_err)? != 1 { return Err(notify_error(messages::ALLIANCE_APPLICATION_NOT_FOUND)); }
                queue_application_changed(tx, outbox, alliance, corp, Some(application_line(application))).await?;
            }
            if !rejecting {
                for app in dao::delete_other_applications(tx, corp, alliance, APP_NEW).await.map_err(db_err)? {
                    queue_application_changed(tx, outbox, app.alliance, corp, Some(application_line(app))).await?;
                }
                queue_member_changed(tx, outbox, alliance, corp, None).await?;
                queue_alliance_changed(tx, outbox, alliance, old_alliance).await?;
            }
            Ok(())
        })
    }, |value, outbox| async move { dispatch_world(value, outbox, world) }).await
}
