//! Populate unclaimed responses for `requestBroker.py:_send_request`.
//! Uses request and descriptor fields without World or DAO access; unsupported
//! action requests are refused by exact name, unclaimed requests with a
//! `no_local_version_available` field instead have no payload, and other
//! unclaimed requests receive defaults.
use std::collections::HashMap;
use bytes::Bytes;
use prost_reflect::{DynamicMessage, ReflectMessage};
use infinity_core::consts::UUID_LEN;
use super::{Caller, message::message};

/// Server limit for nesting when collecting request identifiers.
const IDENTIFIER_DEPTH: u32 = 2;
/// Identifier-like sub-messages of a request (and the caller's character), by type,
/// so a default response can echo what it was asked about.
pub(super) fn known_ids(req: &DynamicMessage, caller: &Caller) -> HashMap<String, DynamicMessage> {
    use prost_reflect::Value as PV;
    let mut ids = HashMap::new();
    if let (Some(ch), Some(d)) = (caller.character_id, message("eve_public.character.Identifier")) {
        let mut m = DynamicMessage::new(d);
        if m.try_set_field_by_name("sequential", PV::U32(ch as u32)).is_ok() {
            ids.insert(m.descriptor().full_name().to_string(), m);
        }
    }
    fn walk(m: &DynamicMessage, ids: &mut HashMap<String, DynamicMessage>, depth: u32) {
        for (f, v) in m.fields() {
            if let PV::Message(sub) = v {
                if f.is_list() || f.is_map() {
                    continue;
                }
                if sub.descriptor().full_name().ends_with(".Identifier") {
                    ids.insert(sub.descriptor().full_name().to_string(), sub.clone());
                } else if depth > 0 {
                    walk(sub, ids, depth - 1);
                }
            }
        }
    }
    walk(req, &mut ids, IDENTIFIER_DEPTH);
    ids
}

/// Make a default response safe to read:
/// * singular sub-messages are populated (echoing identifiers from `ids`),
/// * `uuid` byte fields get the all-zero UUID (the client's "none", e.g. skill plans),
/// * `eve_public.math.Fraction`s are 0/1 rather than 0/0,
/// * a oneof offering a "nothing" flag (`no_title`, `no_equipped`, `not_completed`,
///   `none`...) has it set, which is how these APIs say "absent".
pub(super) fn fill_defaults(m: &mut DynamicMessage, depth: u32, ids: &HashMap<String, DynamicMessage>) {
    use prost_reflect::{Kind, Value as PV};
    if depth == 0 {
        return;
    }
    let desc = m.descriptor();
    if desc.full_name() == "eve_public.math.Fraction" {
        let _ = m.try_set_field_by_name("denominator", PV::U64(1));
        return;
    }
    for o in desc.oneofs() {
        if o.fields().any(|f| m.has_field(&f)) {
            continue;
        }
        let nothing = o.fields().find(|f| {
            let n = f.name();
            matches!(f.kind(), Kind::Bool) && (n.starts_with("no_") || n.starts_with("not_") || n == "none")
        });
        if let Some(f) = nothing {
            m.set_field(&f, PV::Bool(true));
        }
    }
    for f in desc.fields() {
        if f.is_list() || f.is_map() || f.containing_oneof().is_some() {
            continue;
        }
        match f.kind() {
            Kind::Bytes if f.name() == "uuid" => m.set_field(&f, PV::Bytes(Bytes::from_static(&[0u8; UUID_LEN]))),
            Kind::Message(md) if !md.full_name().starts_with("google.protobuf.") => {
                if let Some(id) = ids.get(md.full_name()) {
                    m.set_field(&f, PV::Message(id.clone()));
                    continue;
                }
                let mut sub = DynamicMessage::new(md);
                fill_defaults(&mut sub, depth - 1, ids);
                m.set_field(&f, PV::Message(sub));
            }
            _ => {}
        }
    }
}

/// The refusal a known unsupported action gets instead of a manufactured reply.
pub(super) enum UnsupportedAction {
    /// Status only: no response payload is sent.
    Bodyless,
    /// The typed response with `unspecified_error` selected, which the freelance
    /// project and item-delivery messengers read on their error paths
    /// (`freelanceprojects/client/messenger.py`, item delivery messenger).
    TypedFailure,
}

/// Exact request names of unsupported actions refused instead of default-filled.
/// Every other unclaimed request keeps the existing default response.
pub(super) fn unsupported_action(name: &str) -> Option<UnsupportedAction> {
    Some(match name {
        "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest"
        | "eve_public.freelance.project.api.CloseRequest"
        | "eve_public.freelance.project.api.CommitRequest"
        | "eve_public.freelance.project.api.CreateRequest"
        | "eve_public.freelance.project.api.KickRequest"
        | "eve_public.freelance.project.api.ResignRequest" => UnsupportedAction::TypedFailure,
        "eve_public.achievement.api.admin.AddProgressRequest"
        | "eve_public.achievement.api.admin.CompleteRequest"
        | "eve_public.achievement.api.admin.ResetAllRequest"
        | "eve_public.achievement.api.admin.ResetRequest"
        | "eve_public.achievement.reward.api.ClaimRequest"
        | "eve_public.brawl.career.achievement.character.ClaimRewardRequest"
        | "eve_public.brawl.career.achievement.character.UnlockShipRequest"
        | "eve_public.brawl.match.ChangeShipFittingPlanRequest"
        | "eve_public.brawl.match.LeaveMatchRequest"
        | "eve_public.brawl.match.PrepareRequest"
        | "eve_public.brawl.room.KickCharacterRequest"
        | "eve_public.brawl.room.LeaveRoomRequest"
        | "eve_public.brawl.room.LockRoomSlotRequest"
        | "eve_public.brawl.room.PrepareRoomRequest"
        | "eve_public.brawl.room.UnPrepareRoomRequest"
        | "eve_public.brawl.room.UnlockRoomSlotRequest"
        | "eve_public.campaign.objective.api.CommitRequest"
        | "eve_public.campaign.objective.api.RedeemRewardsRequest"
        | "eve_public.campaign.objective.api.ResignRequest"
        | "eve_public.career.goal.api.ClaimRewardRequest"
        | "eve_public.career.goal.api.admin.CompleteRequest"
        | "eve_public.career.goal.api.admin.ProgressRequest"
        | "eve_public.chat.local.api.BroadcastMessageRequest"
        | "eve_public.chat.local.api.admin.MuteRequest"
        | "eve_public.cloudcomputing.scheme.DeleteSchemeRequest"
        | "eve_public.cloudcomputing.scheme.RenameSchemeRequest"
        | "eve_public.cloudcomputing.scheme.SaveSchemeRequest"
        | "eve_public.cloudcomputing.scheme.UpdateSchemeShipsRequest"
        | "eve_public.corporationgoal.api.RedeemMyRewardsRequest"
        | "eve_public.cosmetic.character.title.api.EquipRequest"
        | "eve_public.cosmetic.character.title.api.UnequipRequest"
        | "eve_public.cosmetic.character.title.api.admin.GrantRequest"
        | "eve_public.cosmetic.character.title.api.admin.RevokeRequest"
        | "eve_public.cosmetic.corporation.palette.api.SetRequest"
        | "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest"
        | "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest"
        | "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest"
        | "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest"
        | "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest"
        | "eve_public.cosmetic.ship.logo.ClearRequest"
        | "eve_public.cosmetic.ship.logo.DisplayRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.RevokeRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.RevokeRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.license.api.UnapplyRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.RevokeRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest"
        | "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest"
        | "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest"
        | "eve_public.cosmetic.structure.paintwork.license.api.RevokeRequest"
        | "eve_public.cosmetic.structure.paintwork.license.api.admin.IssueRequest"
        | "eve_public.cosmetic.structure.paintwork.preset.license.api.IssueRequest"
        | "eve_public.cosmetic.structure.paintwork.preset.license.character.RevokeRequest"
        | "eve_public.entitlement.character.ship.admin.alliancelogo.GrantRequest"
        | "eve_public.entitlement.character.ship.admin.alliancelogo.RevokeRequest"
        | "eve_public.entitlement.character.ship.admin.corplogo.GrantRequest"
        | "eve_public.entitlement.character.ship.admin.corplogo.RevokeRequest"
        | "eve_public.faction.activity.admin.IncrementContributionScoreRequest"
        | "eve_public.faction.activity.admin.SetContributionScoreRequest"
        | "eve_public.freelance.project.api.RedeemRewardRequest"
        | "eve_public.gacha.activity.DrawRewardRequest"
        | "eve_public.gacha.activity.PickEntryRequest"
        | "eve_public.goal.api.CloseRequest"
        | "eve_public.goal.api.CreateRequest"
        | "eve_public.goal.api.DeleteRequest"
        | "eve_public.goal.api.SetCurrentProgressRequest"
        | "eve_public.matrix.api.ConfirmRegenerateRequest"
        | "eve_public.matrix.api.DestroyRequest"
        | "eve_public.matrix.api.LockRequest"
        | "eve_public.matrix.api.OnlineRequest"
        | "eve_public.matrix.api.PrimaryLevelUpRequest"
        | "eve_public.matrix.api.RegenerateRequest"
        | "eve_public.matrix.api.RollbackRequest"
        | "eve_public.matrix.api.SecondaryLevelUpRequest"
        | "eve_public.matrix.api.SecondaryStageUpRequest"
        | "eve_public.matrix.api.TrainRequest"
        | "eve_public.matrix.api.UnbindRequest"
        | "eve_public.matrix.market.api.BuyRequest"
        | "eve_public.matrix.market.api.CancelRequest"
        | "eve_public.matrix.market.api.SellRequest"
        | "eve_public.payment.purchase.api.TokenRequest"
        | "eve_public.plex.vault.api.PurchaseRequest"
        | "eve_public.recruitment.recruitedtarget.CreateRecruitedTargetRequest"
        | "eve_public.recruitment.recruitedtarget.CreateRelationshipRequest"
        | "eve_public.recruitment.recruitedtarget.WithdrawRelationshipRequest"
        | "eve_public.recruitment.recruiter.CreateRecruiterRequest"
        | "eve_public.recruitment.recruiter.HandleRelationshipRequest"
        | "eve_public.recruitment.recruiter.ModifyRecruitmentNoticeRequest"
        | "eve_public.recruitment.recruiter.TerminateRelationshipRequest"
        | "eve_public.recruitment.task.ClaimRewardRequest"
        | "eve_public.recruitment.task.ClaimShipSkinRewardRequest"
        | "eve_public.shipkillbenefit.exchangeshop.ExchangeRequest"
        | "eve_public.sovereignty.hub.fuel.api.AddRequest"
        | "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest"
        | "eve_public.sovereignty.hub.upgrade.api.UninstallRequest"
        | "eve_public.sovereignty.hub.workforce.api.ConfigureRequest"
        | "eve_public.sovereignty.mercenaryden.activity.api.StartRequest"
        | "eve_public.sovereignty.skyhook.api.ActivateRequest"
        | "eve_public.sovereignty.skyhook.api.DeactivateRequest"
        | "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest"
        | "eve_public.survey.SubmitAnswersRequest" => UnsupportedAction::Bodyless,
        _ => return None,
    })
}

#[cfg(all(test, feature = "tests-gateway"))] mod tests;
