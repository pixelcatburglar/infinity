//! Descriptor-backed daily contribution configurations from frozen authored templates.
//! Mirrors dailygoals/client/utils.py; this module performs no gameplay matching.
use anyhow::{bail, Context, Result};
use prost_reflect::{DynamicMessage, ReflectMessage, Value};
use crate::{gateway::message::{child, set}, lua::content::daily_goals::model::{Category, Matcher, Method, Owner}};

fn nested(parent: &mut DynamicMessage, name: &str, value: DynamicMessage) -> Result<()> {
    set(parent, name, Value::Message(value))
}
fn uuid(parent: &mut DynamicMessage, name: &str, text: &str) -> Result<()> {
    let mut id = child(&parent.descriptor(), name)?;
    let bytes = crate::uuid_text::parse(text, crate::uuid_text::Case::Either).context("invalid authored goal UUID")?;
    set(&mut id, "uuid", Value::Bytes(bytes.to_vec().into()))?;
    nested(parent, name, id)
}
fn enum_value(message: &DynamicMessage, field: &str, name: &str) -> Result<Value> {
    let f = message.descriptor().get_field_by_name(field).context("missing enum field")?;
    let enumeration = f.kind().as_enum().context("field is not an enum")?.clone();
    Ok(Value::EnumNumber(enumeration.get_value_by_name(name).context("missing enum value")?.number()))
}
pub(super) fn category(message: &DynamicMessage, field: &str, category: Category) -> Result<Value> {
    enum_value(message, field, match category {
        Category::Ordinary => "CATEGORY_DAILY", Category::Bonus => "CATEGORY_DAILY_BONUS", Category::Month => "CATEGORY_MONTHLY_BONUS",
    })
}
fn matcher(entry: &mut DynamicMessage, field: &str, value: &Matcher, exact: &str) -> Result<()> {
    let mut m = child(&entry.descriptor(), field)?;
    let (name, id) = match value {
        Matcher::Any => { set(&mut m, "any", Value::Bool(true))?; return nested(entry, field, m); }
        Matcher::Id { id } => (exact, *id),
        Matcher::Type { id } => (exact, u64::try_from(*id)?),
        Matcher::Character { id } => ("match_character_value", *id),
        Matcher::Corporation { id } => ("match_corporation_value", *id),
        Matcher::Alliance { id } => ("match_alliance_value", *id),
        Matcher::Faction { id } => ("match_faction_value", *id),
        Matcher::Station { id } => ("station", *id),
        Matcher::Structure { id } => ("structure", *id),
        Matcher::Goal { uuid: text } => { uuid(&mut m, "goal", text)?; return nested(entry, field, m); }
        Matcher::Ordinary | Matcher::Bonus | Matcher::Month => {
            let c = match value { Matcher::Ordinary => Category::Ordinary, Matcher::Bonus => Category::Bonus, _ => Category::Month };
            let v = category(&m, "category", c)?;
            set(&mut m, "category", v)?;
            return nested(entry, field, m);
        }
    };
    if id == 0 { bail!("zero contribution identifier"); }
    super::sequential(&mut m, name, id)?;
    nested(entry, field, m)
}
fn owner(entry: &mut DynamicMessage, value: Owner) -> Result<()> {
    let mut m = child(&entry.descriptor(), "job_owner_type")?;
    set(&mut m, match value { Owner::Any => "any", Owner::Character => "match_any_character", Owner::Corporation => "match_any_corporation" }, Value::Bool(true))?;
    nested(entry, "job_owner_type", m)
}
fn entry(branch: &DynamicMessage) -> Result<DynamicMessage> { child(&branch.descriptor(), "contribution_methods") }
fn add(branch: &mut DynamicMessage, entries: Vec<Value>) -> Result<()> { set(branch, "contribution_methods", Value::List(entries)) }

pub(super) fn build(parent: &DynamicMessage, method: &Method) -> Result<DynamicMessage> {
    let mut config = child(&parent.descriptor(), "contribution_configuration")?;
    let name = match method {
        Method::KillNpc { .. } => "kill_npc", Method::DamageShip { .. } => "damage_ship", Method::MineOre { .. } => "mine_ore",
        Method::CaptureFwComplex { .. } => "fw_capture", Method::DefendFwComplex { .. } => "fw_defend",
        Method::RemoteRepairArmor { .. } => "remote_armor_repair", Method::RemoteRepairShields { .. } => "remote_shield_repair",
        Method::ScanSignature { .. } => "scan_signature", Method::ManufactureItem { .. } => "install_manufacturing_job",
        Method::CompleteGoal { .. } => "complete_daily_goal", Method::SalvageWreck { .. } => "salvage_wreck",
        Method::EarnLoyaltyPoints { .. } => "earn_loyalty_points", Method::SpaceJump => "space_jump",
    };
    let mut branch = child(&config.descriptor(), name)?;
    let mut values = Vec::new();
    match method {
        Method::KillNpc { entries } | Method::SalvageWreck { entries } => for v in entries {
            let mut e = entry(&branch)?; matcher(&mut e, "solar_system", &v.system, "match_value")?; values.push(Value::Message(e));
        },
        Method::DamageShip { entries } | Method::RemoteRepairArmor { entries } | Method::RemoteRepairShields { entries } => for v in entries {
            let mut e = entry(&branch)?; matcher(&mut e, "solar_system", &v.system, "match_value")?; matcher(&mut e, "ship_type", &v.ship, "match_value")?; matcher(&mut e, "identity", &v.identity, "match_value")?; values.push(Value::Message(e));
        },
        Method::MineOre { entries } => for v in entries {
            let mut e = entry(&branch)?; matcher(&mut e, "solar_system", &v.system, "match_value")?; matcher(&mut e, "ore", &v.ore, "ore")?; values.push(Value::Message(e));
        },
        Method::CaptureFwComplex { entries } | Method::DefendFwComplex { entries } => for v in entries {
            let mut e = entry(&branch)?; matcher(&mut e, if name == "fw_capture" { "previous_owner" } else { "owner" }, &v.faction, "match_value")?; matcher(&mut e, "solar_system", &v.system, "match_value")?; matcher(&mut e, "archetype", &v.archetype, "match_value")?; values.push(Value::Message(e));
        },
        Method::ScanSignature { entries } => for v in entries {
            let mut e = entry(&branch)?; matcher(&mut e, "signature_type", &v.signature, "match_value")?; matcher(&mut e, "solar_system", &v.system, "match_value")?; values.push(Value::Message(e));
        },
        Method::ManufactureItem { entries } => for v in entries {
            let mut e = entry(&branch)?; owner(&mut e, v.owner)?; matcher(&mut e, "job_location", &v.facility, "match_value")?; matcher(&mut e, "item_type", &v.item, "item_type")?; values.push(Value::Message(e));
        },
        Method::CompleteGoal { entries } => for v in entries {
            let mut e = entry(&branch)?; matcher(&mut e, "daily_goal", &v.goal, "goal")?; matcher(&mut e, "daily_goal_category", &v.category, "category")?; values.push(Value::Message(e));
        },
        Method::EarnLoyaltyPoints { entries } => for v in entries {
            let mut e = entry(&branch)?; matcher(&mut e, "corporation", &v.corporation, "match_value")?; values.push(Value::Message(e));
        },
        Method::SpaceJump => (),
    }
    if !matches!(method, Method::SpaceJump) { if values.is_empty() { bail!("empty contribution methods"); } add(&mut branch, values)?; }
    nested(&mut config, name, branch)?;
    Ok(config)
}
