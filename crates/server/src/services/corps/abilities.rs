//! Corporation member capacity for `eve/client/script/ui/services/corporation`: the client UI
//! reads the server capacity rather than computing a local formula. Capacity uses the effective
//! CEO skill snapshot from `game.skills` and expert systems together with SDE dogma attributes
//! and effects. Racial membership restrictions are not modeled.
use crate::{consts::dogma::ATTR_SKILL_LEVEL, db::executor::Executor, sde::Sde, services::{CallError, skills::effective}};
use crate::services::fit::{OP_MOD_ADD, OP_PRE_MUL};

pub(super) async fn capacity(ex: &(impl Executor + ?Sized), sde: &Sde, ceo: i64, now: i64) -> Result<i64, CallError> {
    let invalid = || CallError::Internal(anyhow::anyhow!("invalid SDE corporation capacity dogma"));
    let mut attrs = sde.dogma.attributes_by_name("corporationMemberLimit");
    let limit = attrs.next().ok_or_else(invalid)?;
    if attrs.next().is_some() { return Err(invalid()); }
    let mut attrs = sde.dogma.attributes_by_name("corporationMemberBonus");
    let bonus = attrs.next().ok_or_else(invalid)?;
    if attrs.next().is_some() { return Err(invalid()); }
    let mut effects = sde.dogma.effects_by_name("corporationMemberLimitBonus");
    let add = effects.next().ok_or_else(invalid)?;
    if effects.next().is_some() || !add.modifiers().iter().any(|m| m.func == "ItemModifier" && m.domain == "charID" && m.op == OP_MOD_ADD && m.modified == limit.attribute_id && m.modifying == bonus.attribute_id) { return Err(invalid()); }
    let mut effects = sde.dogma.effects_by_name("corporationManagementSkillBoostCorporationMemberBonus");
    let multiply = effects.next().ok_or_else(invalid)?;
    if effects.next().is_some() || !multiply.modifiers().iter().any(|m| m.func == "ItemModifier" && m.domain == "itemID" && m.op == OP_PRE_MUL && m.modified == bonus.attribute_id && m.modifying == ATTR_SKILL_LEVEL) { return Err(invalid()); }
    let snapshot = effective::snapshot_at(ex, sde, ceo, now).await?;
    let mut total = limit.default_value.ok_or_else(invalid)?;
    for (type_id, entry) in snapshot.iter() {
        let Some(ty) = sde.types.get(type_id) else { continue };
        if ty.effects().iter().any(|e| e.effect_id == add.effect_id) && ty.effects().iter().any(|e| e.effect_id == multiply.effect_id) {
            total += ty.attribute(bonus.attribute_id).or(bonus.default_value).ok_or_else(invalid)? * f64::from(entry.effective_level());
        }
    }
    if !total.is_finite() || total < 0.0 || total.fract() != 0.0 || total >= i64::MAX as f64 { return Err(invalid()); }
    Ok(total as i64)
}
