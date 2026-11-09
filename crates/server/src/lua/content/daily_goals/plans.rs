//! Closed daily-goal planners over frozen definitions and authoritative facts.
//! Client contribution configuration is described by `dailygoals/client/utils.py`.
//! Policy is server-authored; persistence, source authority and settlement are outside this module.
use std::{collections::HashSet, time::Instant};
use mlua::{Lua, UserData, UserDataMethods, Value};
use super::model::{Category, Matcher, Method, Owner, Period};
use super::super::{PinnedPolicy, policy::run_closed};
use crate::config::lua::LuaConfig;

/// Server safety ceiling for bounded planner collections and identifiers.
pub(crate) const MAX_PLAN_ENTRIES: usize = 64;
/// Four ordinary slots are specified by the daily assignment contract.
const SLOT_COUNT: usize = 4;

#[derive(Clone, Debug, Default)]
pub(crate) struct FactInput {
    pub method: String,
    pub system: Option<u64>, pub type_id: Option<i32>,
    pub character: Option<u64>, pub corporation: Option<u64>, pub alliance: Option<u64>, pub faction: Option<u64>,
    pub facility: Option<u64>,
    /// Trusted facility kind: true for station, false for structure, None if unproven.
    pub facility_is_station: Option<bool>,
    pub owner: Option<Owner>, pub signature: Option<u64>, pub archetype: Option<u64>,
    pub goal: Option<String>, pub category: Option<Category>, pub period: Option<Period>,
}
#[derive(Clone, Debug)]
pub(crate) struct Candidate { pub key: String, pub template: String }
#[derive(Clone, Debug)]
pub(crate) struct MatchInput { pub fact: FactInput, pub candidates: Vec<Candidate> }
#[derive(Clone, Debug)]
pub(crate) struct AssignInput { pub entropy: [u64; SLOT_COUNT] }
#[derive(Clone, Debug)]
pub(crate) struct CascadeInput { pub ordinary_ids: Vec<String>, pub bonus_dates: Vec<String> }
#[derive(Clone, Debug)]
pub(crate) struct Projection { pub key: String, pub selected: bool }
#[derive(Clone, Debug)]
pub(crate) struct ProjectInput { pub entries: Vec<Projection> }
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct KeysPlan { pub keys: Vec<String> }
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CascadePlan { pub bonus: u64, pub month: u64 }

#[derive(Clone)]
struct Condition { kind: &'static str, expected: String, actual: String }
#[derive(Clone)]
struct MatchCandidate { key: String, method: &'static str, entries: Vec<Vec<Condition>> }
#[derive(Clone)]
enum Input {
    Assign { pools: Vec<Vec<String>>, entropy: [u64; SLOT_COUNT] },
    Match { fact_method: String, candidates: Vec<MatchCandidate> },
    Cascade { ordinary: Vec<String>, dates: Vec<String>, bonus: i64, month: i64 },
    Project(Vec<Projection>),
}
fn invalid() -> mlua::Error { mlua::Error::runtime("invalid daily policy input or output") }
fn index(i: usize, len: usize) -> mlua::Result<usize> { i.checked_sub(1).filter(|i| *i < len).ok_or_else(invalid) }
impl UserData for Input {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("slot_count", |_, s, ()| match s { Self::Assign { .. } => Ok(SLOT_COUNT), _ => Err(invalid()) });
        m.add_method("pool_count", |_, s, slot: usize| match s { Self::Assign { pools, .. } => Ok(pools[index(slot, pools.len())?].len()), _ => Err(invalid()) });
        m.add_method("pool_key", |_, s, (slot, i): (usize, usize)| match s { Self::Assign { pools, .. } => { let p = &pools[index(slot, pools.len())?]; Ok(p[index(i, p.len())?].clone()) }, _ => Err(invalid()) });
        m.add_method("entropy", |_, s, (slot, count): (usize, usize)| match s { Self::Assign { entropy, pools } => { let slot = index(slot, entropy.len())?; if count == 0 || count > pools[slot].len() { return Err(invalid()); } Ok((entropy[slot] % count as u64) as i64) }, _ => Err(invalid()) });
        m.add_method("candidate_count", |_, s, ()| match s { Self::Match { candidates, .. } => Ok(candidates.len()), _ => Err(invalid()) });
        m.add_method("fact_method", |_, s, ()| match s { Self::Match { fact_method, .. } => Ok(fact_method.clone()), _ => Err(invalid()) });
        m.add_method("method", |_, s, i: usize| match s { Self::Match { candidates, .. } => Ok(candidates[index(i, candidates.len())?].method), _ => Err(invalid()) });
        m.add_method("candidate_key", |_, s, i: usize| match s { Self::Match { candidates, .. } => Ok(candidates[index(i, candidates.len())?].key.clone()), _ => Err(invalid()) });
        m.add_method("entry_count", |_, s, i: usize| match s { Self::Match { candidates, .. } => Ok(candidates[index(i, candidates.len())?].entries.len()), _ => Err(invalid()) });
        m.add_method("condition_count", |_, s, (i, j): (usize, usize)| match s { Self::Match { candidates, .. } => { let c = &candidates[index(i, candidates.len())?]; Ok(c.entries[index(j, c.entries.len())?].len()) }, _ => Err(invalid()) });
        m.add_method("condition", |_, s, (i, j, k): (usize, usize, usize)| match s { Self::Match { candidates, .. } => { let c = &candidates[index(i, candidates.len())?]; let e = &c.entries[index(j, c.entries.len())?]; let v = &e[index(k, e.len())?]; Ok((v.kind, v.expected.clone(), v.actual.clone())) }, _ => Err(invalid()) });
        m.add_method("ordinary_count", |_, s, ()| match s { Self::Cascade { ordinary, .. } => Ok(ordinary.len()), _ => Err(invalid()) });
        m.add_method("ordinary_id", |_, s, i: usize| match s { Self::Cascade { ordinary, .. } => Ok(ordinary[index(i, ordinary.len())?].clone()), _ => Err(invalid()) });
        m.add_method("bonus_date_count", |_, s, ()| match s { Self::Cascade { dates, .. } => Ok(dates.len()), _ => Err(invalid()) });
        m.add_method("bonus_date", |_, s, i: usize| match s { Self::Cascade { dates, .. } => Ok(dates[index(i, dates.len())?].clone()), _ => Err(invalid()) });
        m.add_method("bonus_target", |_, s, ()| match s { Self::Cascade { bonus, .. } => Ok(*bonus), _ => Err(invalid()) });
        m.add_method("month_maximum", |_, s, ()| match s { Self::Cascade { month, .. } => Ok(*month), _ => Err(invalid()) });
        m.add_method("projection_count", |_, s, ()| match s { Self::Project(v) => Ok(v.len()), _ => Err(invalid()) });
        m.add_method("projection_selected", |_, s, i: usize| match s { Self::Project(v) => Ok(v[index(i, v.len())?].selected), _ => Err(invalid()) });
        m.add_method("projection_key", |_, s, i: usize| match s { Self::Project(v) => Ok(v[index(i, v.len())?].key.clone()), _ => Err(invalid()) });
    }
}
fn run(lua: &Lua, source: &[u8], config: &LuaConfig, operation: Option<&str>, input: Option<Input>, deadline: Option<Instant>) -> mlua::Result<Value> {
    if source.len() > config.data_file_limit_bytes { return Err(invalid()); }
    let source = std::str::from_utf8(source).map_err(mlua::Error::external)?;
    let wrapper = format!("local operation, input = ...\nlocal exports = (function()\n{source}\nend)()\nif type(exports) ~= 'table' or type(exports.Assign) ~= 'function' or type(exports.Match) ~= 'function' or type(exports.Cascade) ~= 'function' or type(exports.Project) ~= 'function' then error('invalid daily exports') end\nfor k in pairs(exports) do if k ~= 'Assign' and k ~= 'Match' and k ~= 'Cascade' and k ~= 'Project' then error('unknown daily export') end end\nif operation == nil then return true end\nreturn exports[operation](input)");
    let input = input.map(|v| lua.create_userdata(v)).transpose()?;
    run_closed(lua, &wrapper, config, (operation, input), deadline)
}
pub(crate) fn validate(lua: &Lua, source: &[u8], config: &LuaConfig) -> mlua::Result<()> {
    if matches!(run(lua, source, config, None, None, None)?, Value::Boolean(true)) { Ok(()) } else { Err(invalid()) }
}
fn invoke(pin: &PinnedPolicy, op: &str, input: Input, deadline: Option<Instant>) -> mlua::Result<Value> {
    if pin.daily_goals().is_none() { return Err(invalid()); }
    run(&pin.registry.lua, &pin.package.source, &pin.package.config, Some(op), Some(input), deadline)
}
fn valid_key(s: &str) -> bool { !s.is_empty() && s.len() <= MAX_PLAN_ENTRIES }
fn bounded_strings(v: &[String]) -> bool { v.len() <= MAX_PLAN_ENTRIES && v.iter().all(|s| valid_key(s)) }
fn decode_table(value: Value, fields: &[&str]) -> mlua::Result<mlua::Table> {
    let Value::Table(t) = value else { return Err(invalid()); };
    let mut count = 0;
    for pair in t.clone().pairs::<Value, Value>() { let (k, _) = pair?; let Value::String(k) = k else { return Err(invalid()); }; if !fields.contains(&k.to_str()?.as_ref()) { return Err(invalid()); } count += 1; }
    if count != fields.len() { return Err(invalid()); } Ok(t)
}
fn decode_keys(value: Value, allowed: &HashSet<String>) -> mlua::Result<KeysPlan> {
    let t = decode_table(value, &["keys"])?;
    let Value::Table(list) = t.raw_get::<Value>("keys")? else { return Err(invalid()); };
    let mut ordered = vec![None; allowed.len()]; let mut seen = HashSet::new(); let mut count = 0;
    for pair in list.pairs::<Value, Value>() { let (i, v) = pair?; let (Value::Integer(i), Value::String(v)) = (i, v) else { return Err(invalid()); }; let i = usize::try_from(i).map_err(|_| invalid())?; let i = index(i, ordered.len())?; let key = v.to_str()?.to_owned(); if !allowed.contains(&key) || !seen.insert(key.clone()) || ordered[i].is_some() { return Err(invalid()); } ordered[i] = Some(key); count += 1; }
    if ordered.iter().take(count).any(Option::is_none) || ordered.iter().skip(count).any(Option::is_some) { return Err(invalid()); }
    Ok(KeysPlan { keys: ordered.into_iter().take(count).flatten().collect() })
}
/// Execute outside database, market and actor locks against this pin's frozen content.
pub(crate) fn assign(pin: &PinnedPolicy, input: &AssignInput, deadline: Option<Instant>) -> mlua::Result<KeysPlan> {
    let defs = pin.daily_goals().ok_or_else(invalid)?;
    let mut pools = defs.slot_pools().to_vec(); pools.push(defs.ordinary().iter().map(|t| t.key.clone()).collect());
    if pools.len() != SLOT_COUNT || pools.iter().any(|p| p.is_empty() || !bounded_strings(p)) { return Err(invalid()); }
    let allowed = pools[SLOT_COUNT - 1].iter().cloned().collect();
    let entropy = input.entropy;
    let plan = decode_keys(invoke(pin, "Assign", Input::Assign { pools: pools.clone(), entropy }, deadline)?, &allowed)?;
    if plan.keys.len() != SLOT_COUNT || plan.keys.iter().enumerate().any(|(i, k)| !pools[i].contains(k)) { return Err(invalid()); } Ok(plan)
}
fn condition(m: &Matcher, f: &FactInput) -> Condition {
    let (kind, expected, actual) = match m {
        Matcher::Any => ("any", String::new(), String::new()),
        Matcher::Id { id } => ("id", id.to_string(), String::new()),
        Matcher::Type { id } => ("type", id.to_string(), f.type_id.map(|v| v.to_string()).unwrap_or_default()),
        Matcher::Character { id } => ("character", id.to_string(), f.character.map(|v| v.to_string()).unwrap_or_default()),
        Matcher::Corporation { id } => ("corporation", id.to_string(), f.corporation.map(|v| v.to_string()).unwrap_or_default()),
        Matcher::Alliance { id } => ("alliance", id.to_string(), f.alliance.map(|v| v.to_string()).unwrap_or_default()),
        Matcher::Faction { id } => ("faction", id.to_string(), f.faction.map(|v| v.to_string()).unwrap_or_default()),
        Matcher::Station { id } | Matcher::Structure { id } => {
            let expected_kind = if matches!(m, Matcher::Station { .. }) { "station" } else { "structure" };
            let actual = f.facility.zip(f.facility_is_station).map(|(id, station)| {
                format!("{}:{id}", if station { "station" } else { "structure" })
            }).unwrap_or_default();
            ("facility", format!("{expected_kind}:{id}"), actual)
        },
        Matcher::Ordinary => ("category", "ordinary".into(), category(f.category)),
        Matcher::Bonus => ("category", "bonus".into(), category(f.category)),
        Matcher::Month => ("category", "month".into(), category(f.category)),
        Matcher::Goal { uuid } => ("goal", uuid.to_ascii_lowercase(), f.goal.as_ref().map(|s| s.to_ascii_lowercase()).unwrap_or_default()),
    }; Condition { kind, expected, actual }
}
fn category(c: Option<Category>) -> String { match c { Some(Category::Ordinary) => "ordinary", Some(Category::Bonus) => "bonus", Some(Category::Month) => "month", None => "" }.into() }
fn id_condition(m: &Matcher, actual: Option<u64>, f: &FactInput) -> Condition { let mut c = condition(m, f); if matches!(m, Matcher::Id { .. }) { c.actual = actual.map(|v| v.to_string()).unwrap_or_default(); } c }
fn period_condition(p: Period, actual: Option<Period>) -> Condition { let name = |p| match p { Period::Day => "day", Period::Month => "month" }; Condition { kind: "period", expected: name(p).into(), actual: actual.map(name).unwrap_or("").into() } }
fn owner_condition(p: Owner, actual: Option<Owner>) -> Condition {
    let name = |p| match p { Owner::Any => "any", Owner::Character => "character", Owner::Corporation => "corporation" };
    if matches!(p, Owner::Any) { Condition { kind: "any", expected: String::new(), actual: String::new() } }
    else { Condition { kind: "owner", expected: name(p).into(), actual: actual.map(name).unwrap_or("").into() } }
}
fn entries(method: &Method, f: &FactInput) -> Vec<Vec<Condition>> {
    let sys = |m| id_condition(m, f.system, f);
    match method {
        Method::KillNpc { entries } | Method::SalvageWreck { entries } => entries.iter().map(|e| vec![sys(&e.system)]).collect(),
        Method::DamageShip { entries } | Method::RemoteRepairArmor { entries } | Method::RemoteRepairShields { entries } => entries.iter().map(|e| vec![sys(&e.system), condition(&e.ship, f), condition(&e.identity, f)]).collect(),
        Method::MineOre { entries } => entries.iter().map(|e| vec![sys(&e.system), condition(&e.ore, f)]).collect(),
        Method::ScanSignature { entries } => entries.iter().map(|e| vec![sys(&e.system), id_condition(&e.signature, f.signature, f)]).collect(),
        Method::ManufactureItem { entries } => entries.iter().map(|e| vec![owner_condition(e.owner, f.owner), id_condition(&e.facility, f.facility, f), condition(&e.item, f)]).collect(),
        Method::EarnLoyaltyPoints { entries } => entries.iter().map(|e| vec![id_condition(&e.corporation, f.corporation, f)]).collect(),
        Method::CompleteGoal { entries } => entries.iter().map(|e| vec![condition(&e.goal, f), condition(&e.category, f), period_condition(e.period, f.period)]).collect(),
        Method::CaptureFwComplex { entries } | Method::DefendFwComplex { entries } => entries.iter().map(|e| vec![id_condition(&e.faction, f.faction, f), sys(&e.system), id_condition(&e.archetype, f.archetype, f)]).collect(),
        Method::SpaceJump => vec![vec![]],
    }
}
/// Candidate templates are resolved only from the pinned package; facts are trusted canonical projections.
pub(crate) fn matches(pin: &PinnedPolicy, input: &MatchInput, deadline: Option<Instant>) -> mlua::Result<KeysPlan> {
    let defs = pin.daily_goals().ok_or_else(invalid)?; let mut allowed = HashSet::new(); let mut candidates = Vec::new();
    if input.candidates.len() > MAX_PLAN_ENTRIES || !valid_key(&input.fact.method) || input.fact.goal.as_ref().is_some_and(|s| !valid_key(s)) { return Err(invalid()); }
    for c in &input.candidates { if !valid_key(&c.key) || !allowed.insert(c.key.clone()) { return Err(invalid()); } let t = defs.template(&c.template).ok_or_else(invalid)?; candidates.push(MatchCandidate { key: c.key.clone(), method: t.method.metadata_key(), entries: entries(&t.method, &input.fact) }); }
    decode_keys(invoke(pin, "Match", Input::Match { fact_method: input.fact.method.clone(), candidates }, deadline)?, &allowed)
}
/// Count unique authoritative completed IDs and bonus dates against frozen targets.
pub(crate) fn cascade(pin: &PinnedPolicy, input: &CascadeInput, deadline: Option<Instant>) -> mlua::Result<CascadePlan> {
    let defs = pin.daily_goals().ok_or_else(invalid)?;
    if !bounded_strings(&input.ordinary_ids) || !bounded_strings(&input.bonus_dates) { return Err(invalid()); }
    // Unique counts cannot exceed their bounded input lengths, so larger targets
    // are equivalent to those lengths and fit losslessly in Lua's integer type.
    let bonus = defs.bonus().target.min(input.ordinary_ids.len() as u64) as i64;
    let month = defs.monthly_base().iter().chain(defs.monthly_omega()).map(|t| t.target).max().ok_or_else(invalid)?.min(input.bonus_dates.len() as u64) as i64;
    let v = invoke(pin, "Cascade", Input::Cascade { ordinary: input.ordinary_ids.clone(), dates: input.bonus_dates.clone(), bonus, month }, deadline)?;
    let t = decode_table(v, &["bonus", "month"])?;
    let get = |key| -> mlua::Result<u64> { match t.raw_get::<Value>(key)? { Value::Integer(v) => u64::try_from(v).map_err(|_| invalid()), _ => Err(invalid()) } };
    let plan = CascadePlan { bonus: get("bonus")?, month: get("month")? };
    let ordinary = input.ordinary_ids.iter().collect::<HashSet<_>>().len() as u64; let dates = input.bonus_dates.iter().collect::<HashSet<_>>().len() as u64;
    if plan.bonus > ordinary.min(bonus as u64) || plan.month > dates.min(month as u64) { return Err(invalid()); } Ok(plan)
}
/// Project only keys present and selected in the immutable authoritative input.
pub(crate) fn project(pin: &PinnedPolicy, input: &ProjectInput, deadline: Option<Instant>) -> mlua::Result<KeysPlan> {
    if input.entries.len() > MAX_PLAN_ENTRIES { return Err(invalid()); } let mut all = HashSet::new(); let mut allowed = HashSet::new();
    for e in &input.entries { if !valid_key(&e.key) || !all.insert(e.key.clone()) { return Err(invalid()); } if e.selected { allowed.insert(e.key.clone()); } }
    decode_keys(invoke(pin, "Project", Input::Project(input.entries.clone()), deadline)?, &allowed)
}

#[cfg(test)] mod tests;
