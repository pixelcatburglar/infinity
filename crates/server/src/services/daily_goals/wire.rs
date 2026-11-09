//! Daily-goal protobuf projections from frozen snapshots and persisted reward rows.
//! Mirrors dailygoals/client/goalMessenger.py and utils.py; no queries or delivery.
mod contribution;
#[cfg(test)] mod tests;

use anyhow::{bail, ensure, Context, Result};
use prost_reflect::{DynamicMessage, Kind, ReflectMessage, Value};
use infinity_core::consts::PLEX_HUNDREDTHS;
use crate::{client, dao::daily_goals::rewards::RewardRow, gateway::{message::{child, identifier, message, response_for, set}, notices::GatewayNotice, protocol::target_group::Target}, lua::content::daily_goals::model::Career};
use super::{Error, GoalView, HistoryPage};

const API: &str = "eve_public.dailygoal.api.";
/// Inclusive seconds bounds of google.protobuf.Timestamp (years 0001 through 9999).
const TIMESTAMP_MIN: i64 = -62_135_596_800;
const TIMESTAMP_MAX: i64 = 253_402_300_799;
fn nested(m: &mut DynamicMessage, name: &str, value: DynamicMessage) -> Result<()> { set(m, name, Value::Message(value)) }
fn named(name: &str) -> Result<DynamicMessage> { Ok(DynamicMessage::new(message(name).with_context(|| format!("missing {name}"))?)) }
fn request(name: &str) -> Result<DynamicMessage> { response_for(&format!("{API}{name}Request")) }
fn uuid(parent: &DynamicMessage, field: &str, text: &str) -> Result<DynamicMessage> {
    let bytes = crate::uuid_text::parse(text, crate::uuid_text::Case::Lowercase).context("invalid persisted UUID")?;
    let mut id = child(&parent.descriptor(), field)?;
    set(&mut id, "uuid", Value::Bytes(bytes.to_vec().into()))?;
    Ok(id)
}
fn sequential(parent: &mut DynamicMessage, field: &str, id: u64) -> Result<()> {
    let descriptor = child(&parent.descriptor(), field)?.descriptor();
    let kind = descriptor.get_field_by_name("sequential").context("missing sequential identifier")?.kind();
    let value = match kind {
        Kind::Uint32 => Value::U32(u32::try_from(id)?),
        Kind::Int32 => Value::I32(i32::try_from(id)?),
        Kind::Uint64 => Value::U64(id),
        _ => bail!("unsupported identifier wire type"),
    };
    let value = identifier(&parent.descriptor(), field, value)?;
    nested(parent, field, value)
}
fn timestamp(parent: &mut DynamicMessage, field: &str, time: i64) -> Result<()> {
    let (seconds, nanos) = client::unix_parts_from_filetime(time);
    ensure!((TIMESTAMP_MIN..=TIMESTAMP_MAX).contains(&seconds), "timestamp out of range");
    let mut value = child(&parent.descriptor(), field)?;
    set(&mut value, "seconds", Value::I64(seconds))?;
    set(&mut value, "nanos", Value::I32(nanos))?;
    nested(parent, field, value)
}
fn unit(parent: &DynamicMessage, row: &RewardRow) -> Result<DynamicMessage> {
    let mut unit = child(&parent.descriptor(), "unit")?;
    let mut branch = child(&unit.descriptor(), &row.unit)?;
    match row.unit.as_str() {
        "isk" => {
            let (units, nanos) = row.amount_isk.context("missing ISK")?.split_precision().context("ISK overflow")?;
            let mut amount = child(&branch.descriptor(), "amount")?;
            set(&mut amount, "units", Value::I64(units))?; set(&mut amount, "nanos", Value::I32(nanos))?;
            nested(&mut branch, "amount", amount)?;
        }
        "lp" => {
            let mut amount = child(&branch.descriptor(), "amount")?;
            set(&mut amount, "amount", Value::U64(row.amount_units.context("missing LP")?))?;
            sequential(&mut amount, "associated_corporation", u64::try_from(row.issuer_id.context("missing LP issuer")?)?)?;
            nested(&mut branch, "amount", amount)?;
        }
        "sp" | "item" => {
            set(&mut branch, "amount", Value::U32(u32::try_from(row.amount_units.context("missing reward amount")?)?))?;
            if row.unit == "item" { sequential(&mut branch, "type", u64::try_from(row.type_id.context("missing item type")?)?)?; }
        }
        "plex" => {
            let cents = row.amount_units.context("missing PLEX")?.checked_mul(PLEX_HUNDREDTHS as u64).context("PLEX overflow")?;
            let mut amount = child(&branch.descriptor(), "amount")?;
            set(&mut amount, "total_in_cents", Value::I64(i64::try_from(cents)?))?;
            nested(&mut branch, "amount", amount)?;
        }
        _ => bail!("unknown reward unit"),
    }
    nested(&mut unit, &row.unit, branch)?;
    Ok(unit)
}
fn attributes(parent: &DynamicMessage, field: &str, view: &GoalView) -> Result<DynamicMessage> {
    let mut a = child(&parent.descriptor(), field)?;
    for (field, id) in [("name_message", view.snapshot.messages.name), ("description_message", view.snapshot.messages.description), ("help_text_message", view.snapshot.messages.help), ("assigner", u64::try_from(view.snapshot.assigner)?)] { sequential(&mut a, field, id)?; }
    let category = contribution::category(&a, "category", view.snapshot.template.category)?;
    set(&mut a, "category", category)?;
    let config = contribution::build(&a, &view.snapshot.template.method)?;
    nested(&mut a, "contribution_configuration", config)?;
    let career_field = a.descriptor().get_field_by_name("career").context("missing career")?;
    let career_enum = career_field.kind().as_enum().context("career not enum")?.clone();
    let name = match view.snapshot.template.career { Career::None => "CAREER_UNSPECIFIED", Career::Explorer => "CAREER_EXPLORATION", Career::Industrialist => "CAREER_INDUSTRIALIST", Career::Enforcer => "CAREER_ENFORCER", Career::SoldierOfFortune => "CAREER_SOLDIER_OF_FORTUNE" };
    let career = career_enum.get_value_by_name(name).context("missing career enum value")?.number();
    set(&mut a, "career", Value::EnumNumber(career))?;
    let mut payments = Vec::with_capacity(view.rewards.len());
    for row in &view.rewards {
        let mut p = child(&a.descriptor(), "payment")?;
        let id = uuid(&p, "asset", &row.asset_id)?; nested(&mut p, "asset", id)?;
        let period_field = p.descriptor().get_field_by_name("period").context("missing payment period")?;
        let e = period_field.kind().as_enum().context("payment period not enum")?.clone();
        let number = e.get_value_by_name("PAYMENT_PERIOD_COMPLETION").context("missing completion period")?.number();
        set(&mut p, "period", Value::EnumNumber(number))?;
        let value = unit(&p, row)?; nested(&mut p, "unit", value)?;
        payments.push(Value::Message(p));
    }
    set(&mut a, "payment", Value::List(payments))?;
    set(&mut a, "target", Value::U64(view.goal.target))?;
    timestamp(&mut a, "active_after", view.goal.period_start)?; timestamp(&mut a, "active_until", view.goal.period_end)?;
    set(&mut a, "omega", Value::Bool(view.snapshot.template.omega_required))?;
    Ok(a)
}
fn earnings(parent: &mut DynamicMessage, view: &GoalView, notice: bool) -> Result<()> {
    let mut values = Vec::new();
    for row in view.rewards.iter().filter(|r| r.earned && r.claimed_at.is_none()) {
        let mut earning = child(&parent.descriptor(), "earnings")?;
        let value = unit(&earning, row)?; nested(&mut earning, "unit", value)?;
        if !notice { set(&mut earning, "omega_required", Value::Bool(row.omega_required && !(view.goal.earned_omega || view.current_omega)))?; }
        values.push(Value::Message(earning));
    }
    set(parent, "earnings", Value::List(values))
}
fn goal(parent: &DynamicMessage, view: &GoalView, notice: bool) -> Result<DynamicMessage> {
    ensure!(!view.goal.paid || view.goal.progress == view.goal.target, "paid goal progress mismatch");
    let mut g = child(&parent.descriptor(), "goals")?;
    let id = uuid(&g, "id", &view.goal.goal_id)?; nested(&mut g, "id", id)?;
    let a = attributes(&g, "goal", view)?; nested(&mut g, "goal", a)?;
    set(&mut g, "current_progress", Value::U64(view.goal.progress))?;
    earnings(&mut g, view, notice)?;
    if !notice { set(&mut g, "paid_completion", Value::Bool(view.goal.paid))?; }
    Ok(g)
}
fn populate(m: &mut DynamicMessage, views: &[GoalView], notice: bool) -> Result<()> {
    let values = views.iter().map(|v| goal(m, v, notice).map(Value::Message)).collect::<Result<Vec<_>>>()?;
    set(m, "goals", Value::List(values))
}
fn validate_field(m: &DynamicMessage, name: &str, kind: Kind, repeated: bool) -> Result<()> {
    let desc = m.descriptor();
    let field = desc.get_field_by_name(name).with_context(|| format!("missing {}.{name}", desc.full_name()))?;
    ensure!(field.kind() == kind && field.is_list() == repeated, "incompatible {}.{name}", desc.full_name());
    Ok(())
}
fn validate_child(m: &DynamicMessage, name: &str, repeated: bool) -> Result<DynamicMessage> {
    let desc = m.descriptor();
    let field = desc.get_field_by_name(name).with_context(|| format!("missing {}.{name}", desc.full_name()))?;
    ensure!(field.is_list() == repeated, "incompatible cardinality {}.{name}", desc.full_name());
    child(&desc, name)
}
fn validate_oneof(m: &DynamicMessage, fields: &[&str]) -> Result<()> {
    let desc = m.descriptor();
    let first = desc.get_field_by_name(fields[0]).context("missing oneof field")?;
    let oneof = first.containing_oneof().context("missing oneof")?;
    for name in fields {
        let field = desc.get_field_by_name(name).with_context(|| format!("missing {}.{name}", desc.full_name()))?;
        ensure!(field.containing_oneof().as_ref() == Some(&oneof), "wrong oneof {}.{name}", desc.full_name());
    }
    ensure!(oneof.fields().count() == fields.len(), "unexpected oneof members in {}", desc.full_name());
    Ok(())
}
pub(crate) fn validate() -> Result<()> {
    for name in ["GetAllCurrent", "Get", "GetAllWithRewards", "Redeem", "PayForCompletion"] { request(name)?; }
    response_for("eve_public.dailygoal.api.admin.ProgressRequest")?;
    for name in ["CurrentGoalsNotice", "ProgressedNotice", "CompletedNotice", "RedeemedNotice"] { named(&format!("{API}{name}"))?; }
    for name in ["ProgressedNotice", "CompletedNotice", "RedeemedNotice"] {
        let n = named(&format!("{API}{name}"))?;
        let id = validate_child(&n, "goal", false)?;
        validate_field(&id, "uuid", Kind::Bytes, false)?;
        if name == "ProgressedNotice" { validate_field(&n, "current_progress", Kind::Uint64, false)?; }
    }
    let current = request("GetAllCurrent")?;
    let current_goal = validate_child(&current, "goals", true)?;
    validate_child(&current_goal, "id", false)?;
    validate_field(&current_goal, "current_progress", Kind::Uint64, false)?;
    validate_field(&current_goal, "paid_completion", Kind::Bool, false)?;
    let query_earning = validate_child(&current_goal, "earnings", true)?;
    validate_field(&query_earning, "omega_required", Kind::Bool, false)?;
    let notice = named(&format!("{API}CurrentGoalsNotice"))?;
    let notice_goal = validate_child(&notice, "goals", true)?;
    validate_child(&notice_goal, "id", false)?;
    validate_child(&notice_goal, "goal", false)?;
    validate_field(&notice_goal, "current_progress", Kind::Uint64, false)?;
    ensure!(notice_goal.descriptor().get_field_by_name("paid_completion").is_none(), "unexpected notice paid field");
    let notice_earning = validate_child(&notice_goal, "earnings", true)?;
    ensure!(notice_earning.descriptor().get_field_by_name("omega_required").is_none(), "unexpected notice earning restriction");
    validate_child(&notice_earning, "unit", false)?;
    let get = request("Get")?;
    validate_field(&get, "progress", Kind::Uint64, false)?;
    ensure!(get.descriptor().get_field_by_name("paid_completion").is_none(), "unexpected Get paid field");
    let a = validate_child(&get, "goal", false)?;
    for field in ["name_message", "description_message", "help_text_message", "assigner"] { validate_child(&a, field, false)?; }
    validate_field(&a, "target", Kind::Uint64, false)?;
    validate_field(&a, "omega", Kind::Bool, false)?;
    for field in ["active_after", "active_until"] {
        let t = validate_child(&a, field, false)?;
        validate_field(&t, "seconds", Kind::Int64, false)?;
        validate_field(&t, "nanos", Kind::Int32, false)?;
    }
    let payment = validate_child(&a, "payment", true)?;
    let asset = validate_child(&payment, "asset", false)?;
    validate_field(&asset, "uuid", Kind::Bytes, false)?;
    let unit = validate_child(&payment, "unit", false)?;
    for name in ["isk", "lp", "sp", "item", "plex"] { validate_child(&unit, name, false)?; }
    validate_oneof(&unit, &["isk", "lp", "sp", "item", "plex"])?;
    let isk = validate_child(&validate_child(&unit, "isk", false)?, "amount", false)?;
    validate_field(&isk, "units", Kind::Sint64, false)?;
    validate_field(&isk, "nanos", Kind::Sint32, false)?;
    let lp = validate_child(&validate_child(&unit, "lp", false)?, "amount", false)?;
    validate_field(&lp, "amount", Kind::Uint64, false)?;
    validate_child(&lp, "associated_corporation", false)?;
    validate_field(&validate_child(&unit, "sp", false)?, "amount", Kind::Uint32, false)?;
    let item = validate_child(&unit, "item", false)?;
    validate_field(&item, "amount", Kind::Uint32, false)?;
    validate_child(&item, "type", false)?;
    let plex = validate_child(&validate_child(&unit, "plex", false)?, "amount", false)?;
    validate_field(&plex, "total_in_cents", Kind::Sint64, false)?;
    for category in [crate::lua::content::daily_goals::model::Category::Ordinary, crate::lua::content::daily_goals::model::Category::Bonus, crate::lua::content::daily_goals::model::Category::Month] { contribution::category(&a, "category", category)?; }
    let career = a.descriptor().get_field_by_name("career").context("missing career")?.kind().as_enum().context("career not enum")?.clone();
    for name in ["CAREER_UNSPECIFIED", "CAREER_EXPLORATION", "CAREER_INDUSTRIALIST", "CAREER_ENFORCER", "CAREER_SOLDIER_OF_FORTUNE"] { career.get_value_by_name(name).with_context(|| format!("missing career {name}"))?; }
    let period = payment.descriptor().get_field_by_name("period").context("missing period")?.kind().as_enum().context("period not enum")?.clone();
    period.get_value_by_name("PAYMENT_PERIOD_COMPLETION").context("missing completion period")?;
    let config = validate_child(&a, "contribution_configuration", false)?;
    validate_oneof(&config, &["kill_npc", "damage_ship", "mine_ore", "fw_capture", "fw_defend", "remote_armor_repair", "remote_shield_repair", "scan_signature", "install_manufacturing_job", "complete_daily_goal", "salvage_wreck", "earn_loyalty_points", "space_jump"])?;
    for (name, fields) in [
        ("kill_npc", &["solar_system"][..]), ("damage_ship", &["solar_system", "ship_type", "identity"]),
        ("mine_ore", &["solar_system", "ore"]), ("fw_capture", &["previous_owner", "solar_system", "archetype"]),
        ("fw_defend", &["owner", "solar_system", "archetype"]),
        ("remote_armor_repair", &["solar_system", "ship_type", "identity"]),
        ("remote_shield_repair", &["solar_system", "ship_type", "identity"]),
        ("scan_signature", &["signature_type", "solar_system"]),
        ("install_manufacturing_job", &["job_owner_type", "job_location", "item_type"]),
        ("complete_daily_goal", &["daily_goal", "daily_goal_category"]),
        ("salvage_wreck", &["solar_system"]), ("earn_loyalty_points", &["corporation"]),
    ] {
        let branch = validate_child(&config, name, false)?;
        let entry = validate_child(&branch, "contribution_methods", true)?;
        for field in fields {
            let matcher = validate_child(&entry, field, false)?;
            validate_field(&matcher, "any", Kind::Bool, false)?;
            let descriptor = matcher.descriptor();
            let variants: &[&str] = match descriptor.name() {
                "SolarSystemMatcher" | "ShipTypeMatcher" | "FactionMatcher" | "DeadspaceArchetypeMatcher" | "SignatureTypeMatcher" | "CorporationMatcher" => &["any", "match_value"],
                "IdentityMatcher" => &["any", "match_character_value", "match_corporation_value", "match_alliance_value", "match_faction_value"],
                "OreTypeMatcher" => &["any", "ore"],
                "ItemTypeMatcher" => &["any", "item_type"],
                "OwnerTypeMatcher" => &["any", "match_any_character", "match_any_corporation"],
                "JobLocationMatcher" => &["any", "station", "structure"],
                "DailyGoalMatcher" => &["any", "goal"],
                "DailyGoalCategoryMatcher" => &["any", "category"],
                _ => bail!("unexpected matcher descriptor {}", descriptor.full_name()),
            };
            validate_oneof(&matcher, variants)?;
        }
    }
    validate_child(&config, "space_jump", false)?;
    let history = request("GetAllWithRewards")?;
    validate_child(&history, "ids", true)?;
    let page = validate_child(&history, "next_page", false)?;
    validate_field(&page, "token", Kind::String, false)?;
    Ok(())
}
pub(crate) fn current_response(views: &[GoalView]) -> std::result::Result<DynamicMessage, Error> {
    let mut m = request("GetAllCurrent")?; populate(&mut m, views, false)?; Ok(m)
}
pub(crate) fn get_response(view: &GoalView) -> std::result::Result<DynamicMessage, Error> {
    if view.goal.paid && view.goal.progress != view.goal.target { return Err(anyhow::anyhow!("paid goal progress mismatch").into()); }
    let mut m = request("Get")?; let a = attributes(&m, "goal", view)?; nested(&mut m, "goal", a)?;
    set(&mut m, "progress", Value::U64(view.goal.progress))?; earnings(&mut m, view, false)?; Ok(m)
}
pub(crate) fn history_response(page: &HistoryPage) -> std::result::Result<DynamicMessage, Error> {
    let mut m = request("GetAllWithRewards")?;
    let ids = page.ids.iter().map(|id| uuid(&m, "ids", id).map(Value::Message)).collect::<Result<Vec<_>>>()?;
    set(&mut m, "ids", Value::List(ids))?;
    let mut next = child(&m.descriptor(), "next_page")?; set(&mut next, "token", Value::String(page.next_token.clone()))?; nested(&mut m, "next_page", next)?; Ok(m)
}
pub(crate) fn empty_response(name: &str) -> std::result::Result<DynamicMessage, Error> {
    match name { "eve_public.dailygoal.api.RedeemRequest" | "eve_public.dailygoal.api.PayForCompletionRequest" | "eve_public.dailygoal.api.admin.ProgressRequest" => Ok(response_for(name)?), _ => Err(anyhow::anyhow!("unsupported empty daily response").into()) }
}
fn notice(owner: i64, m: &DynamicMessage) -> std::result::Result<GatewayNotice, Error> {
    let id = u32::try_from(owner).map_err(|e| anyhow::anyhow!(e))?;
    if id == 0 { return Err(anyhow::anyhow!("invalid notice owner").into()); }
    GatewayNotice::new(Target::Character(id), m).map_err(|e| Error::from(crate::gateway::notices::producer_error(e)))
}
pub(crate) fn current_notice(owner: i64, views: &[GoalView]) -> std::result::Result<GatewayNotice, Error> {
    let mut m = named(&format!("{API}CurrentGoalsNotice"))?; populate(&mut m, views, true)?; notice(owner, &m)
}
fn goal_notice(name: &str, owner: i64, id: &str, progress: Option<u64>) -> std::result::Result<GatewayNotice, Error> {
    let mut m = named(&format!("{API}{name}"))?; let id = uuid(&m, "goal", id)?; nested(&mut m, "goal", id)?;
    if let Some(progress) = progress { set(&mut m, "current_progress", Value::U64(progress))?; }
    notice(owner, &m)
}
pub(crate) fn progressed_notice(owner: i64, goal: &str, absolute: u64) -> std::result::Result<GatewayNotice, Error> { goal_notice("ProgressedNotice", owner, goal, Some(absolute)) }
pub(crate) fn completed_notice(owner: i64, goal: &str) -> std::result::Result<GatewayNotice, Error> { goal_notice("CompletedNotice", owner, goal, None) }
pub(crate) fn redeemed_notice(owner: i64, goal: &str) -> std::result::Result<GatewayNotice, Error> { goal_notice("RedeemedNotice", owner, goal, None) }
