//! Persistent sovereignty notifications via the existing notification center.
//! Formatter fields mirror notifications/common/formatters and groupSov in
//! eve/common/script/util/notificationconst.py. Recipient selection is server-authored.
//! Call only after inserting a unique transition history row in the same transaction.
use std::collections::BTreeSet;
use crate::{dao::{alliances, corporations::registry, sovereignty::{history::HistoryRow, policy::PolicyRow, structures::HubRow}}, db::{executor::Executor, outbox::Outbox}, services::{db_err, mail, CallError}};
use super::TransitionKind;

/// notificationconst.groupSov.
const GROUP_SOV: i32 = 6;
/// notificationconst corporate sovereignty loss.
const CLAIM_LOST: i32 = 42;
/// notificationconst corporate sovereignty acquisition.
const CLAIM_ACQUIRED: i32 = 44;
/// notificationconst.notificationTypeAllianceCapitalChanged.
const CAPITAL_CHANGED: i32 = 165;
/// notificationconst.notificationTypeSovStructureSelfDestructRequested.
const DESTRUCT_REQUESTED: i32 = 167;
/// notificationconst.notificationTypeSovStructureSelfDestructCancelled.
const DESTRUCT_CANCELLED: i32 = 168;
/// notificationconst.notificationTypeSovStructureSelfDestructFinished.
const DESTRUCT_FINISHED: i32 = 169;

pub(crate) struct HubTransition {
    pub history: HistoryRow,
    pub before: Option<HubRow>,
    pub after: Option<HubRow>,
    pub capital: Option<(PolicyRow, PolicyRow)>,
    pub destruct_deadline: Option<i64>,
}

pub(crate) async fn transition<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, change: &HubTransition) -> Result<(), CallError> {
    let history = &change.history;
    let sender = history.actor_id.or(history.old_corporation_id).unwrap_or(infinity_core::consts::OWNER_SCC);
    if let Some(hub) = change.before.as_ref().or(change.after.as_ref()) {
        let ownership_cancel = (history.reason == TransitionKind::Transferred.as_str() || history.reason == TransitionKind::Neutralized.as_str())
            && change.before.as_ref().is_some_and(|before| before.structure.self_destruct_due_at.is_some())
            && change.after.as_ref().is_some_and(|after| after.structure.self_destruct_due_at.is_none());
        let kind = match history.reason.as_str() {
            value if value == TransitionKind::DestructRequested.as_str() => Some(DESTRUCT_REQUESTED),
            value if value == TransitionKind::DestructCancelled.as_str() || value == TransitionKind::RecoveryCancelled.as_str() => Some(DESTRUCT_CANCELLED),
            value if value == TransitionKind::DestructFinished.as_str() => Some(DESTRUCT_FINISHED),
            _ if ownership_cancel => Some(DESTRUCT_CANCELLED),
            _ => None,
        };
        if let (Some(kind), Some(corp)) = (kind, history.old_corporation_id) {
            let data = match kind {
                DESTRUCT_REQUESTED => {
                    let actor = history.actor_id.ok_or_else(|| db_err(anyhow::anyhow!("destruct request missing actor")))?;
                    let deadline = change.destruct_deadline.ok_or_else(|| db_err(anyhow::anyhow!("destruct request missing deadline")))?;
                    let name = registry::corporation(ex, corp).await.map_err(db_err)?.ok_or_else(|| db_err(anyhow::anyhow!("destruct owner missing")))?.name;
                    serde_json::json!({"structureTypeID":hub.type_id,"solarSystemID":history.system_id,"charID":actor,"corpName":name,"destructTime":deadline})
                }
                DESTRUCT_CANCELLED => {
                    let actor = if ownership_cancel { change.before.as_ref().and_then(|before| before.structure.self_destruct_actor) } else { history.actor_id }
                        .ok_or_else(|| db_err(anyhow::anyhow!("destruct cancellation missing actor")))?;
                    serde_json::json!({"structureTypeID":hub.type_id,"solarSystemID":history.system_id,"charID":actor})
                }
                _ => serde_json::json!({"structureTypeID":hub.type_id,"solarSystemID":history.system_id}),
            }.to_string();
            let receivers: BTreeSet<_> = alliances::pilots(ex, None, Some(corp)).await.map_err(db_err)?.into_iter().collect();
            let actual_sender = if ownership_cancel { hub.structure.self_destruct_actor.unwrap_or(sender) } else { sender };
            for receiver in receivers { mail::notify(ex, outbox, receiver, kind, GROUP_SOV, actual_sender, &data).await?; }
        }
    }
    if history.old_alliance_id != history.new_alliance_id {
        if let Some(corp) = history.old_corporation_id {
            let data = serde_json::json!({"solarSystemID":history.system_id}).to_string();
            let receivers: BTreeSet<_> = alliances::pilots(ex, None, Some(corp)).await.map_err(db_err)?.into_iter().collect();
            for receiver in receivers { mail::notify(ex, outbox, receiver, CLAIM_LOST, GROUP_SOV, infinity_core::consts::OWNER_DED, &data).await?; }
        }
        if let (Some(corp), Some(alliance)) = (history.new_corporation_id, history.new_alliance_id) {
            let data = serde_json::json!({"solarSystemID":history.system_id,"corpID":corp,"allianceID":alliance}).to_string();
            let receivers: BTreeSet<_> = alliances::pilots(ex, None, Some(corp)).await.map_err(db_err)?.into_iter().collect();
            for receiver in receivers { mail::notify(ex, outbox, receiver, CLAIM_ACQUIRED, GROUP_SOV, infinity_core::consts::OWNER_DED, &data).await?; }
        }
    }
    if let Some((before, after)) = &change.capital {
        if before.current_capital_system != after.current_capital_system {
            if let Some(system) = after.current_capital_system {
                let data = serde_json::json!({"solarSystemID":system,"allianceID":after.alliance_id}).to_string();
            let receivers: BTreeSet<_> = alliances::pilots(ex, Some(after.alliance_id), None).await.map_err(db_err)?.into_iter().collect();
            for receiver in receivers { mail::notify(ex, outbox, receiver, CAPITAL_CHANGED, GROUP_SOV, sender, &data).await?; }
            }
        }
    }
    Ok(())
}
