//! Mutual-war transitions and transactional notifications, mirroring
//! `eve/client/script/ui/services/wars/war_cso.py` and `mutualWarInvites.py`.
//! Uses `game.wars`, `game.war_invites`, `game.war_invite_settings` and `game.jobs`.
//! Aggressive wars, allies and paid surrender or treaties are unsupported.
use infinity_core::{consts::FILETIME_DAY, messages};
use crate::{client::filetime_now, dao::{alliances, wars::{self, War}}, db::{Db, executor::Executor, outbox::{Outbox, dispatch_world}}, scheduler, services::{CallError, notify_error}, space::World};
use marshal::Value;
use super::{authority, deadlines, wire};

/// evewar.const WAR_SPOOLUP: 24 hours.
pub(super) const WAR_SPOOLUP: i64 = FILETIME_DAY;
/// evewar.const WAR_COOLDOWN: 24 hours.
pub(super) const WAR_COOLDOWN: i64 = FILETIME_DAY;
/// evewar.const WAR_MUTUAL_WAR_EXPIRY_DAYS: seven days.
pub(super) const WAR_MUTUAL_WAR_EXPIRY_DAYS: i64 = 7;

pub(super) fn invalid() -> CallError { notify_error(messages::wars::INVALID) }

pub(super) async fn tell(ex: &(impl Executor + ?Sized), outbox: &mut Outbox, owners: &[i64], event: &str, args: Vec<Value>) -> Result<(), CallError> {
    for &owner in owners {
        let alliance = alliances::exists(ex, owner).await?;
        for pilot in alliances::pilots(ex, alliance.then_some(owner), (!alliance).then_some(owner)).await? {
            outbox.notify_pilot(pilot, event, args.clone());
        }
    }
    Ok(())
}

pub(super) async fn invites_changed(ex: &(impl Executor + ?Sized), outbox: &mut Outbox, a: i64, b: i64) -> Result<(), CallError> {
    tell(ex, outbox, &[a,b], "OnMutualWarsUpdated_Remote", vec![]).await
}
pub(super) async fn war_changed(ex: &(impl Executor + ?Sized), outbox: &mut Outbox, war: &War, changes: Value) -> Result<(), CallError> {
    tell(ex, outbox, &[war.declared_by, war.against], "OnWarChanged", vec![wire::war(war), Value::list([Value::Int(war.declared_by), Value::Int(war.against)]), changes]).await
}

pub(super) async fn ended(ex: &(impl Executor + ?Sized), outbox: &mut Outbox, a: i64, b: i64, now: i64) -> Result<(), CallError> {
    for war in wars::settle_due_pair(ex, a, b, now).await? {
        war_changed(ex, outbox, &war, Value::Dict(vec![])).await?;
    }
    Ok(())
}

pub(crate) async fn retract(db: &Db, world: &World, actor: i64, war_id: i64) -> Result<(), CallError> {
    let now = filetime_now();
    db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        authority::lock_actor(tx, actor).await?;
        let original = wars::get(tx, war_id).await?.ok_or_else(invalid)?;
        wars::lock_owners(tx, &[original.declared_by, original.against]).await?;
        let original = wars::get(tx, war_id).await?.ok_or_else(invalid)?;
        let corp = crate::dao::characters::crimewatch::corporation(tx, actor).await?.ok_or_else(invalid)?;
        let by = if corp == original.declared_by || corp == original.against { corp } else { authority::owner(tx, actor).await? };
        if by != original.declared_by && by != original.against { return Err(invalid()); }
        authority::require_owner(tx, actor, by).await?;
        let war = wars::retract(tx, war_id, by, now, now + WAR_COOLDOWN).await?.ok_or_else(invalid)?;
        deadlines::schedule_finish(tx, &war).await?;
        war_changed(tx, outbox, &war, wire::changes(Some(&original), &war)).await?;
        Ok(())
    }), |value, outbox| async move { let r = dispatch_world(value, outbox, world); scheduler::wake(); r }).await
}
