//! Shared foreign Board proof for the ship and space test domains.
//! The committed fixture is visible before capture. One transfer is the refund producer.
use crate::dao::items::boarding::RecoveryPlan;
use crate::db::executor::Executor;
use crate::services::CallError;

impl RecoveryPlan {
    pub(crate) async fn foreign_proof_for_test(
        db: &crate::db::Db, world: &crate::space::World,
        tx: &(impl Executor + ?Sized),
        sde: &std::sync::Arc<crate::sde::Sde>,
        outbox: &mut crate::db::outbox::Outbox<()>,
        identity: &crate::services::identity::Identity,
        user: i64, system: i64, old: i64, old_type: i32, target: i64, target_type: i32,
        old_pos: [f64; 3], target_pos: [f64; 3],
        states: &[crate::space::combat::ActorConditionSnapshot],
        facts: Option<&crate::space::snapshot::FactCheckpoint>,
    ) -> Result<(Self, Vec<i64>), CallError> {
        let captured = super::ownership::ForeignTake::capture(db, sde, identity, world, system, target).await?;
        let mut attempt = captured.fresh();
        attempt.transfer(tx, sde, outbox, identity.char_id, system, old, states, facts).await?;
        if !crate::dao::items::boarding::position_moved(tx, target, identity.char_id, system, target_pos).await.map_err(crate::services::db_err)?
            || !crate::dao::items::boarding::board_in_space(tx, identity.char_id, old, target, system, old_type, target_type, old_pos, target_pos).await.map_err(crate::services::db_err)? {
            return Err(crate::services::notify_error(infinity_core::messages::fitting::BOARD_UNAVAILABLE));
        }
        let created = captured.sealed.lock().unwrap_or_else(|poison| poison.into_inner()).as_ref().map(|proof| proof.created_ids().to_vec()).unwrap_or_default();
        Ok((RecoveryPlan::Board { character: identity.char_id, user, system, old, old_type, target, target_type, ownership: Some(std::sync::Arc::clone(&captured.sealed)) }, created))
    }
}
