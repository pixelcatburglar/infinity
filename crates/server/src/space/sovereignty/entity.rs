//! Physical sovereignty Hub projection in the destiny ballpark.
//! Geometry comes from the loaded SDE; score data comes from the committed capture row.
//! The Hub has no activation component.
use crate::{dao::sovereignty::{capture::CaptureRow, structures::HubRow}, space::{Ball, MODE_RIGID, V3, WarpState}};
use anyhow::{ensure, Context};
use marshal::Value;

fn optional(id: Option<i64>) -> Value { id.map(Value::Int).unwrap_or(Value::None) }

pub(super) fn score_value(capture: &CaptureRow) -> Value {
    Value::tuple([optional(capture.controlling_team), optional(capture.scoring_team),
        Value::tuple([Value::Float(capture.score), Value::Int(capture.score_sim_time), Value::Float(capture.rate), Value::Bool(capture.blocked)]),
        Value::Int(capture.defender_team)])
}

pub(super) fn ball(sde: &crate::sde::Sde, hub: &HubRow, capture: &CaptureRow, ms: i64) -> anyhow::Result<Ball> {
    let definition = sde.sovereignty.hub();
    let row = &hub.system;
    ensure!(row.hub_item_id == Some(hub.structure.item_id) && row.system_id == hub.structure.system_id
        && capture.target_item_id == hub.structure.item_id && capture.system_id == row.system_id
        && capture.ownership_epoch == row.ownership_epoch && hub.structure.source_item_id > 0
        && hub.type_id == definition.type_id && hub.owner_id == row.claim_corporation_id.unwrap_or(definition.deploy.default_owner)
        && row.claim_corporation_id.is_some() == row.alliance_id.is_some()
        && capture.defender_team == row.alliance_id.unwrap_or(infinity_core::consts::OWNER_UNOWNED)
        && capture.content_namespace == hub.structure.content_namespace && capture.content_version == hub.structure.content_version,
        "inconsistent sovereignty Hub snapshot");
    let pos = [hub.x.context("missing Hub x")?, hub.y.context("missing Hub y")?, hub.z.context("missing Hub z")?];
    ensure!(pos.iter().all(|v| v.is_finite()) && capture.score.is_finite() && capture.rate.is_finite()
        && capture.capture_time_seconds.is_finite() && capture.capture_time_seconds > 0.0
        && capture.defensive_regen_rate.is_finite() && capture.defensive_regen_rate >= 0.0,
        "invalid sovereignty Hub coordinates or capture data");
    let ty = sde.types.get(hub.type_id).context("missing Hub SDE type")?;
    let radius = ty.radius.context("missing Hub SDE radius")?;
    let mass = ty.fit_attribute(infinity_core::consts::dogma::ATTR_MASS).context("missing Hub SDE mass")?;
    ensure!(radius.is_finite() && radius > 0.0 && (radius as f32).is_finite() && radius as f32 > 0.0
        && mass.is_finite() && mass > 0.0, "invalid Hub SDE geometry");
    let score = score_value(capture);
    Ok(Ball { id: hub.structure.item_id, type_id: hub.type_id, owner_id: hub.owner_id,
        corp_id: row.claim_corporation_id.unwrap_or(hub.owner_id), alliance_id: row.alliance_id.unwrap_or(-1), char_id: None,
        signature_radius: None, mode: MODE_RIGID, radius: radius as f32, mass, max_vel: 0.0, agility: 1.0,
        speed_fraction: 0.0, pos, vel: V3::default(), old_pos: pos, old_vel: V3::default(), segment_acceleration: None,
        goto: V3::default(), follow_id: 0, follow_range: 0.0, free: false, global: definition.is_always_global,
        massive: true, interactive: false, warp: WarpState::default(), last_ms: ms, interpolation_anchor_ms: ms,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0,
        slim: vec![("corpID", optional(row.claim_corporation_id)), ("allianceID", optional(row.alliance_id)), ("entosis_score", score)] })
}
