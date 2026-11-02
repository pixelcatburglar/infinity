//! Shared transactional skill learning for the client skillsvc.py skill flow.
//! Uses game.characters, game.skills, game.skill_history and game.skill_queue
//! with startup SDE data. Purchase and book policy are not handled here.
use super::{completion, db_err, trading::refusal, training, CallError};
use crate::{dao::skills::{history, trading, training as skill_dao}, db::{executor::Executor, outbox::Outbox}, sde::Sde, services::effects::online::OnlineChanged};
use std::{collections::BTreeSet, sync::Arc};

pub(super) async fn begin_tx(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<OnlineChanged>, sde: &Arc<Sde>, char_id: i64, now: i64) -> Result<completion::AdvanceResult, CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    let progress = completion::advance_tx(tx, outbox, sde, char_id, now).await?;
    let accrued = training::in_training_sp(tx, char_id, now).await?;
    skill_dao::write_skills(tx, char_id, &training::plan_skills(&[], accrued)).await.map_err(db_err)?;
    Ok(progress)
}

pub(super) async fn learn_tx(tx: &(impl Executor + ?Sized), char_id: i64, types: &BTreeSet<i32>, now: i64) -> Result<(), CallError> {
    for &type_id in types {
        if !trading::learn(tx, char_id, type_id).await.map_err(db_err)? { return Err(refusal()); }
        history::append(tx, char_id, type_id, super::history::INJECTED, 0, now).await.map_err(db_err)?;
    }
    Ok(())
}
