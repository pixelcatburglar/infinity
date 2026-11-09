//! Industry job calculations and lifecycle for the client industry package.
//! Uses blueprint SDE data and game jobs, items and authorized wallets.
//! Job durations include the configured industry time multiplier; NPC fees use the published
//! facility policy and submitted cost and facility-tax quotes are checked before settlement.
//! Installed deadlines persist in game.jobs and become ready before explicit delivery.
//! Structures and reactions are unsupported; legacy displaced blueprints require repair.
//! Manufacturing install and explicit delivery journal facts with their job mutations;
//! product quantities come from SDE activities, and ready jobs await delivery.

use super::*;
use crate::sde::{Sde, industry::Activity};
use crate::dao::items::quantities::{self, QuantityChange};
use crate::services::{inventory, items, wallets};
use std::collections::BTreeMap;
use crate::db::outbox::SourceKey;
use crate::lua::events::{GameEvent, resources::{FactContext, FactOrigin}};

async fn fact_context(db: &(impl Executor + ?Sized), actor: i64, ship_id: Option<i64>, system_id: Option<i64>, station_id: Option<i64>) -> Result<FactContext, CallError> {
    let organization = crate::dao::characters::members::organization(db, actor).await.map_err(db_err)?;
    Ok(FactContext {
        corporation_id: organization.as_ref().map(|org| org.corporation_id),
        alliance_id: organization.and_then(|org| org.alliance_id),
        faction_id: None, ship_id, ship_type_id: None, system_id, station_id, origin: FactOrigin::Natural,
    })
}

fn manufacturing_quantity(per_run: i64, runs: i32) -> Result<i64, CallError> {
    per_run.checked_mul(i64::from(runs)).filter(|n| *n > 0)
        .ok_or_else(|| notify_error(messages::BLUEPRINT_RUN_COUNT_EXCEEDED))
}


/// industryCommon.GetBlueprintPrice: the adjusted value of what the blueprint's manufacturing
/// takes, at base quantities (the estimated item value of one run).
async fn blueprint_price(db: &(impl Executor + ?Sized), sde: &Sde, bp_type: i32) -> Result<f64, CallError> {
    let Some(m) = sde.industry.activity(bp_type, MANUFACTURING) else { return Ok(0.0) };
    let ids: Vec<i32> = m.materials.iter().map(|(t, _)| *t).collect();
    let price = industry_dao::prices(db, &ids).await.map_err(db_err)?;
    Ok(m.materials.iter().map(|(t, q)| price.get(t).copied().unwrap_or(0.0) * *q as f64).sum())
}

async fn blueprint(db: &(impl Executor + ?Sized), sde: &Sde, item_id: i64) -> Result<Blueprint, CallError> {
    let mut bp = industry_dao::blueprint(db, item_id).await.map_err(db_err)?
        .ok_or_else(|| user_error(messages::MSG_BLUEPRINT_NOT_FOUND, vec![]))?;
    bp.station = bp.station.filter(|id| sde.map.station(*id).is_some());
    Ok(bp)
}

async fn set_blueprint(db: &(impl Executor + ?Sized), item_id: i64, me: i32, te: i32, runs: i32) -> Result<(), CallError> {
    industry_dao::set_blueprint(db, item_id, me, te, runs).await.map_err(db_err)
}

/// Decryptor bonuses from explicitly present type attributes.
fn decryptor(sde: &Sde, type_id: i32) -> (f64, i32, i32, i32) {
    let attr = |a, d| sde.types.attribute(type_id, a).unwrap_or(d);
    (attr(ATTR_INVENTION_PROBABILITY_MULTIPLIER, 1.0),
     attr(ATTR_INVENTION_ME_MODIFIER, 0.0) as i32,
     attr(ATTR_INVENTION_TE_MODIFIER, 0.0) as i32,
     attr(ATTR_INVENTION_MAX_RUN_MODIFIER, 0.0) as i32)
}

/// What a job will take, how long and what it costs, as the industry package computes it.
struct Plan {
    materials: Vec<(i32, i64)>,
    seconds: i64,
    fees: Fees,
    probability: f64,
    product: Option<i32>,
}

struct Fees {
    indexed_cost: f64,
    facility_tax: f64,
    scc: f64,
    security_tax: f64,
}

impl Fees {
    fn total(&self) -> f64 {
        self.indexed_cost + self.facility_tax + self.scc + self.security_tax
    }
}

/// The client rounds the base before each fee, but applies the index before rounding cost.
fn fees(job_cost: f64, activity: i32, security: f64, index: f64, tax: f64, scc_rate: f64, highsec_rate: f64) -> Fees {
    let base = job_cost.round();
    let scc_rate = if matches!(activity, RESEARCH_TIME | RESEARCH_MATERIAL) { scc_rate * 0.5 } else { scc_rate };
    Fees {
        indexed_cost: (job_cost * index).round(),
        facility_tax: (base * tax).round(),
        scc: (base * scc_rate).round(),
        security_tax: if security >= 0.45 { (base * highsec_rate).round() } else { 0.0 },
    }
}

fn validate_quote(cost: Option<&Value>, tax: Option<&Value>, fees: &Fees) -> Result<(), CallError> {
    for (value, expected) in [(cost, fees.indexed_cost), (tax, fees.facility_tax)] {
        if let Some(value) = value {
            if !value.as_float().is_some_and(|n| n.is_finite() && n == expected) {
                return Err(notify_error(messages::industry::QUOTE_CHANGED));
            }
        }
    }
    Ok(())
}

fn job_seconds(seconds: f64, multiplier: f64, runs: i32, max_run_secs: f64) -> Result<i64, CallError> {
    let seconds = seconds * multiplier;
    let rounded = seconds.round();
    if !seconds.is_finite()
        || rounded < i64::MIN as f64
        || rounded >= i64::MAX as f64
        || seconds - seconds / runs as f64 > max_run_secs
    {
        return Err(notify_error(messages::INDUSTRY_PROJECT_TOO_LONG));
    }
    Ok(rounded as i64)
}

fn job_deadline(now: i64, seconds: i64) -> Result<i64, CallError> {
    seconds.checked_mul(FILETIME_SECOND)
        .and_then(|duration| now.checked_add(duration))
        .ok_or_else(|| notify_error(messages::INDUSTRY_PROJECT_TOO_LONG))
}

#[allow(clippy::too_many_arguments)]
async fn plan(
    db: &(impl Executor + ?Sized),
    sde: &Sde,
    fit: &Fit,
    skills: &crate::services::skills::effective::SkillSnapshot,
    bp: &Blueprint,
    act: &Activity,
    activity_id: i32,
    runs: i32,
    licensed_runs: i32,
    requested_product: Option<i32>,
    optional: Option<i32>,
    facility: &Facility,
) -> Result<Plan, CallError> {
    let policy = &crate::config::game().industry;
    let level = |s: i32| skills.level(s) as f64;
    // Materials: ME applies to manufacturing; copying multiplies by the licensed runs.
    let material_runs = if activity_id == COPYING { runs as f64 * licensed_runs as f64 } else { runs as f64 };
    let me_factor = if activity_id == MANUFACTURING { 1.0 - bp.me as f64 / 100.0 } else { 1.0 };
    let mut materials: Vec<(i32, i64)> = act
        .materials
        .iter()
        .map(|(t, q)| {
            let quantity = (((*q as f64 * me_factor * material_runs) * 100.0).round() / 100.0).ceil();
            if !quantity.is_finite() || quantity >= i64::MAX as f64 {
                return Err(notify_error(messages::BLUEPRINT_RUN_COUNT_EXCEEDED));
            }
            Ok((*t, (quantity as i64).max(runs as i64)))
        })
        .collect::<Result<_, CallError>>()?;
    let mut probability = 1.0;
    if activity_id == INVENTION {
        let base = act.products.iter().map(|p| p.2).sum::<f64>() / act.products.len().max(1) as f64;
        let skill_bonus = 1.0 + act.skills.iter().map(|(s, _)| level(*s) * policy.invention_skill_probability).sum::<f64>();
        let decryptor_multiplier = match optional {
            Some(d) => decryptor(sde, d).0,
            None => 1.0,
        };
        probability = (base * skill_bonus * decryptor_multiplier).clamp(0.0, 1.0);
        if let Some(d) = optional {
            materials.push((d, runs as i64));
        }
    }

    // Time: the activity's own formula, then TE and the character's industry attributes.
    let char_attr = |a: i32| {
        let v = fit.value(Key::Char, a);
        if v == 0.0 {
            1.0
        } else {
            v
        }
    };
    let research = |current: i32, step: i32, max: i32| {
        let levels: Vec<f64> = (0..(max / step) as usize).map(|i| act.time * RESEARCH_TIMES[i.min(RESEARCH_TIMES.len() - 1)]).collect();
        let current = (current / step) as usize;
        (0..runs as usize).map(|i| levels[(current + i).min(levels.len() - 1)]).sum::<f64>()
    };
    let base_seconds = match activity_id {
        RESEARCH_TIME => research(bp.te, 2, MAX_TIME_EFFICIENCY),
        RESEARCH_MATERIAL => research(bp.me, 1, MAX_MATERIAL_EFFICIENCY),
        COPYING => act.time * licensed_runs as f64 * runs as f64,
        _ => act.time * runs as f64,
    };
    let mut seconds = base_seconds;
    match activity_id {
        MANUFACTURING => {
            seconds *= 1.0 - bp.te as f64 / 100.0;
            seconds *= char_attr(ATTR_MANUFACTURE_TIME_MULTIPLIER);
            for (skill, _) in &act.skills {
                let per_level = sde.types.attribute(*skill, ATTR_MANUFACTURE_TIME_PER_LEVEL);
                if let Some(p) = per_level {
                    seconds *= 1.0 + p * level(*skill) / 100.0;
                }
            }
        }
        RESEARCH_TIME => seconds *= char_attr(ATTR_MANUFACTURING_TIME_RESEARCH_SPEED),
        RESEARCH_MATERIAL => seconds *= char_attr(ATTR_MINERAL_NEED_RESEARCH_SPEED),
        COPYING => seconds *= char_attr(ATTR_COPY_SPEED_PERCENT),
        INVENTION => seconds *= char_attr(ATTR_INVENTION_RESEARCH_SPEED),
        _ => {}
    }
    let seconds = job_seconds(seconds, policy.job_time_multiplier, runs, policy.max_run_secs)?;

    // Cost: the estimated item value, scaled per activity.
    let product = match activity_id {
        MANUFACTURING | INVENTION => {
            let products: Vec<i32> = act.products.iter().map(|p| p.0).collect();
            requested_product.filter(|p| products.contains(p)).or(products.first().copied())
        }
        _ => None,
    };
    let job_cost = match activity_id {
        MANUFACTURING => blueprint_price(db, sde, bp.type_id).await? * runs as f64,
        COPYING => blueprint_price(db, sde, bp.type_id).await? * policy.job_cost_rate * licensed_runs as f64 * runs as f64,
        RESEARCH_TIME | RESEARCH_MATERIAL if act.time > 0.0 => blueprint_price(db, sde, bp.type_id).await? * policy.job_cost_rate * base_seconds / act.time,
        INVENTION => match product {
            Some(p) => blueprint_price(db, sde, p).await? * policy.job_cost_rate * runs as f64,
            None => 0.0,
        },
        _ => 0.0,
    };
    let fees = fees(job_cost, activity_id, facility.security, facility.system_index(), facility.tax(),
        policy.scc_surcharge_rate, policy.highsec_security_tax);
    Ok(Plan { materials, seconds, fees, probability, product })
}

/// Active jobs per activity of an installer (GetJobCounts).
pub(super) async fn job_counts(db: &(impl Executor + ?Sized), installer: i64) -> Result<HashMap<i32, i64>, CallError> {
    industry_dao::job_counts(db, installer, INDUSTRY_JOB_STATUS_INSTALLED).await.map_err(db_err)
}

/// `InstallJob(job.dump())` -> jobID.
pub(super) async fn install(c: &mut Client, request: &Value) -> CallResult {
    let db = c.db.clone();
    let me = c.session.int("charid").unwrap_or(0);
    let int = |k: &str| request.get(k).filter(|v| !v.is_none()).and_then(|v| v.as_int());
    let activity_id = int("activityID").and_then(|n| i32::try_from(n).ok()).unwrap_or(0);
    let max_runs = crate::config::game().industry.max_runs.min(i32::MAX as i64) as i32;
    let runs = i32::try_from(int("runs").unwrap_or(1)).ok().filter(|n| *n > 0 && *n <= max_runs)
        .ok_or_else(|| notify_error(messages::BLUEPRINT_RUN_COUNT_EXCEEDED))?;
    let licensed_runs = i32::try_from(int("licensedRuns").unwrap_or(1)).ok().filter(|n| *n > 0)
        .ok_or_else(|| notify_error(messages::BLUEPRINT_RUN_COUNT_EXCEEDED))?;
    let requested: HashMap<i32, i64> = match request.get("materials") {
        Some(Value::Dict(pairs)) => pairs.iter().filter_map(|(k, v)| Some((i32::try_from(k.as_int()?).ok()?, v.as_int().unwrap_or(0)))).collect(),
        _ => HashMap::new(),
    };
    let quoted_cost = request.get("cost").cloned();
    let quoted_tax = request.get("tax").cloned();
    let _trading = lock().await;
    let blueprint_id = int("blueprintID").unwrap_or(0);
    let facility_id = int("facilityID").unwrap_or(0);
    let product_request = int("productTypeID").and_then(|p| i32::try_from(p).ok());
    let here = c.session.int("solarsystemid2").unwrap_or(0);
    let ship = c.session.int("shipid").unwrap_or(0);
    let fact_ship = c.session.int("shipid").filter(|id| *id > 0);
    let fact_system = c.session.int("solarsystemid2").filter(|id| *id > 0);
    let fact_station = c.session.int("stationid").filter(|id| *id > 0);
    let fit = Arc::new(if let Some(system) = c.session.int("solarsystemid") {
        Fit::load_in_space(&db, &c.world, system, me, ship).await?
    } else { Fit::load(&db, c.sde.clone(), me, ship).await? });
    let sde = Arc::clone(&c.sde);
    let request = request.clone();
    let now = filetime_now();
    let j = db.transaction(
        |tx, outbox: &mut Outbox| {
            let requested = requested.clone();
            let fit = Arc::clone(&fit);
            let sde = Arc::clone(&sde);
            let quoted_cost = quoted_cost.clone();
            let quoted_tax = quoted_tax.clone();
            let request = request.clone();
            Box::pin(async move {
                let mut bp = blueprint(tx, &sde, blueprint_id).await?;
                authorization::owner(tx, me, bp.owner, Some(activity_id)).await?;
                if request.get("characterID").and_then(Value::as_int).is_some_and(|id| id != me) {
                    return Err(user_error(messages::MSG_INDUSTRY_JOB_ACCESS_DENIED, vec![]));
                }
                if let Some(corp) = request.get("corporationID").and_then(Value::as_int) {
                    if super::super::corps::membership(tx, me).await?.0 != corp {
                        return Err(user_error(messages::MSG_INDUSTRY_JOB_ACCESS_DENIED, vec![]));
                    }
                }
                let busy = industry_dao::has_active_blueprint_job(tx, &[bp.item_id]).await.map_err(db_err)?;
                if busy {
                    return Err(notify_error(messages::BLUEPRINT_IN_USE));
                }
                let fac = facility(&sde, facility_id)?;
                if !fac.activities().contains(&activity_id) {
                    return Err(notify_error(messages::INDUSTRY_ACTIVITY_UNAVAILABLE));
                }
                if bp.station != Some(fac.id) {
                    return Err(notify_error(messages::BLUEPRINT_MUST_BE_AT_FACILITY));
                }
                let locations = authorization::locations(tx, &sde, me, bp.owner, &fac).await?;
                if !locations.iter().any(|l| l.item_id == bp.location && l.flag_id == bp.flag && l.can_view) {
                    return Err(user_error(messages::MSG_INDUSTRY_BLUEPRINT_ACCESS_DENIED, vec![]));
                }
                let input = authorization::select_location(request.get("inputLocation"), &locations, true)?;
                let output = authorization::select_location(request.get("outputLocation"), &locations, false)?;
                let (wallet_owner, wallet_account_key) = authorization::payer(tx, me, request.get("account")).await?;
                let Some(act) = sde.industry.activity(bp.type_id, activity_id) else {
                    return Err(notify_error(messages::BLUEPRINT_ACTIVITY_INVALID));
                };
                // Originals research and copy; only copies are invented from.
                let needs_original = matches!(activity_id, RESEARCH_TIME | RESEARCH_MATERIAL | COPYING);
                if needs_original && bp.copy || activity_id == INVENTION && !bp.copy {
                    return Err(notify_error(messages::BLUEPRINT_ACTIVITY_INVALID));
                }
                let max_runs = match activity_id {
                    RESEARCH_TIME => (MAX_TIME_EFFICIENCY - bp.te) / 2,
                    RESEARCH_MATERIAL => MAX_MATERIAL_EFFICIENCY - bp.me,
                    MANUFACTURING | INVENTION if bp.copy => bp.runs,
                    _ => max_runs,
                };
                if runs > max_runs || (activity_id == COPYING && licensed_runs > act.max_production_limit) {
                    return Err(notify_error(messages::BLUEPRINT_RUN_COUNT_EXCEEDED));
                }
                let skills = crate::services::skills::effective::snapshot_at(tx, &sde, me, now).await?;
                if let Some((skill, level)) = act.skills.iter().find(|(s, l)| skills.level(*s) < *l) {
                    return Err(notify_error(&messages::required_skill_message(skill, level)));
                }
                // Remote jobs need Supply Chain Management / Scientific Networking.
                let reach_skill = if activity_id == MANUFACTURING { SKILL_SUPPLY_CHAIN_MANAGEMENT } else { SKILL_SCIENTIFIC_NETWORKING };
                let reach = DISTANCE_PER_LEVEL[skills.level(reach_skill).clamp(0, 5) as usize];
                if jumps(&sde.map, here, fac.system, reach).is_none() {
                    return Err(notify_error(messages::FACILITY_OUTSIDE_REMOTE_INDUSTRY_RANGE));
                }
                let slots = if activity_id == MANUFACTURING {
                    fit.value(Key::Char, ATTR_MANUFACTURE_SLOT_LIMIT)
                } else {
                    fit.value(Key::Char, ATTR_MAX_LABORATORY_SLOTS)
                }
                .max(1.0) as i64;
                let counts = job_counts(tx, me).await?;
                let used: i64 = if activity_id == MANUFACTURING {
                    counts.get(&MANUFACTURING).copied().unwrap_or(0)
                } else {
                    [RESEARCH_TIME, RESEARCH_MATERIAL, COPYING, INVENTION].iter().map(|a| counts.get(a).copied().unwrap_or(0)).sum()
                };
                if used >= slots {
                    return Err(notify_error(&messages::industry_slots_full_message(slots)));
                }
                let optional = if activity_id == INVENTION {
                    let mut decryptor = None;
                    for t in requested.keys() {
                        let group = sde.types.get(*t).and_then(|r| r.group_id);
                        if group == Some(GROUP_DECRYPTORS) {
                            decryptor = Some(*t);
                        }
                    }
                    decryptor
                } else {
                    None
                };
                let p = plan(tx, &sde, &fit, &skills, &bp, act, activity_id, runs, licensed_runs, product_request, optional, &fac).await?;
                validate_quote(quoted_cost.as_ref(), quoted_tax.as_ref(), &p.fees)?;
                let end = job_deadline(now, p.seconds)?;

                let mut changes = BTreeMap::new();
                let mut originals = BTreeMap::new();
                for (type_id, quantity) in &p.materials {
                    let mut left = *quantity;
                    let stacks = load_items(tx, &sde, ItemQuery::OwnerLocationFlagTypeStackAscending(bp.owner, input.item_id, input.flag_id, *type_id)).await.map_err(db_err)?;
                    for s in stacks {
                        let remaining = changes.get(&s.item_id).and_then(|change: &QuantityChange| change.quantity).unwrap_or(s.quantity);
                        if changes.get(&s.item_id).is_some_and(|change| change.quantity.is_none()) { continue; }
                        let n = remaining.min(left);
                        if n <= 0 { break; }
                        let quantity = remaining - n;
                        changes.insert(s.item_id, QuantityChange::new(&s, (quantity > 0).then_some(quantity)));
                        originals.insert(s.item_id, s);
                        left -= n;
                        if left == 0 { break; }
                    }
                    if left != 0 { return Err(notify_error(&messages::required_materials_message(quantity, type_id))); }
                }
                if !quantities::apply(tx, &changes).await.map_err(db_err)? {
                    return Err(notify_error(messages::STACK_CANNOT_BE_SPLIT));
                }
                for (id, change) in &changes {
                    if change.quantity.is_some() {
                        inventory::notifications::notify_item_change(tx, &sde, outbox, *id, vec![(IX_STACK_SIZE, Value::Int(change.expected_quantity))]).await?;
                    } else if let Some(item) = originals.remove(id) { inventory::notifications::notify_item_removed(outbox, item); }
                }
                let fee = crate::isk::IskAmount::from_isk(p.fees.total())?;
                if wallet_owner == me { wallets::debit(tx, outbox, me, fee).await?; }
                else { wallets::corp_debit(tx, outbox, wallet_owner, wallet_account_key, fee).await?; }
                // Split a blueprint stack without changing its owner's physical custody.
                if !bp.singleton {
                    let stack = load_items(tx, &sde, ItemQuery::Id(bp.item_id)).await.map_err(db_err)?.pop().ok_or_else(|| user_error(messages::MSG_BLUEPRINT_NOT_FOUND, vec![]))?;
                    if stack.quantity > 1 {
                        bp.item_id = inventory::stacks::split_stack(tx, &stack, 1, bp.owner, bp.location, bp.flag, true).await?;
                        set_blueprint(tx, bp.item_id, bp.me, bp.te, bp.runs).await?;
                        inventory::notifications::notify_item_change(tx, &sde, outbox, stack.item_id, vec![(IX_STACK_SIZE, Value::Int(stack.quantity as i64))]).await?;
                        inventory::notifications::notify_item_change(tx, &sde, outbox, bp.item_id, vec![(IX_LOCATION_ID, Value::Int(LOCATION_JUNKYARD))]).await?;
                    } else {
                        if !crate::dao::items::inventory::move_item(tx, &stack, bp.location, bp.flag, bp.owner, true).await.map_err(db_err)? {
                            return Err(notify_error(messages::STACK_CANNOT_BE_SPLIT));
                        }
                        inventory::notifications::notify_item_change(tx, &sde, outbox, bp.item_id, vec![(infinity_core::consts::IX_SINGLETON, Value::Bool(false))]).await?;
                    }
                }
                let mut j = Job {
                    id: 0,
                    installer: me,
                    owner: bp.owner,
                    activity: activity_id,
                    blueprint_id: bp.item_id,
                    blueprint_type: bp.type_id,
                    blueprint_copy: bp.copy,
                    facility: fac.id,
                    system: fac.system,
                    runs,
                    licensed_runs,
                    successful_runs: None,
                    product_type: p.product,
                    optional_type: optional,
                    cost: p.fees.total(),
                    probability: p.probability,
                    time: p.seconds,
                    start: now,
                    end,
                    completed: None,
                    completed_by: None,
                    status: INDUSTRY_JOB_STATUS_INSTALLED,
                    input_location: input.item_id, input_flag: input.flag_id,
                    output_location: output.item_id, output_flag: output.flag_id,
                    wallet_owner, wallet_account_key, blueprint_location: bp.location, blueprint_flag: bp.flag,
                };
                j.id = industry_dao::insert_job(tx, &j).await.map_err(db_err)?;
                completion::schedule(tx, &j).await?;
                if activity_id == MANUFACTURING {
                    let &(product, per_run, _) = act.products.iter().find(|p| Some(p.0) == j.product_type)
                        .or(act.products.first()).ok_or_else(|| notify_error(messages::BLUEPRINT_ACTIVITY_INVALID))?;
                    let quantity = manufacturing_quantity(per_run, runs)?;
                    let context = fact_context(tx, me, fact_ship, fact_system, fact_station).await?;
                    outbox.event_from(SourceKey::new("ManufacturingInstalled", j.id), GameEvent::ManufacturingInstalled {
                        char_id: me, job_id: j.id, owner_id: j.owner, owner_is_corporation: j.owner != me,
                        product_type: product, runs, scheduled_quantity: i64::from(quantity),
                        facility_id: j.facility, occurred_at: now, context,
                    });
                }
                tell_job(tx, outbox, &j).await?;
                Ok(j)
            })
        },
        |j, outbox| async move { let result = dispatch_client(j, outbox, c); crate::scheduler::wake(); result },
    ).await?;
    info!("industry: {me} installed job {} (activity {activity_id}, {runs} runs of {}) at {} for {} ISK, {}s", j.id, j.blueprint_type, j.facility, j.cost, j.time);
    Ok(Value::Int(j.id))
}

/// Remove a used-up blueprint copy.
async fn destroy_blueprint<X: Send>(db: &(impl Executor + ?Sized), sde: &crate::sde::Sde, outbox: &mut Outbox<X>, item_id: i64) -> Result<(), CallError> {
    if let Some(item) = load_items(db, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.pop() {
        industry_dao::delete_blueprint_item(db, item_id).await.map_err(db_err)?;
        inventory::notifications::notify_item_removed(outbox, item);
    }
    Ok(())
}

/// New blueprint copies in the selected output division.
#[allow(clippy::too_many_arguments)]
async fn make_copies<X: Send>(db: &(impl Executor + ?Sized), sde: &std::sync::Arc<Sde>, outbox: &mut Outbox<X>, j: &Job, type_id: i32, count: i32, me: i32, te: i32, runs: i32) -> Result<(), CallError> {
    let additions: Vec<_> = (0..count).map(|_| inventory::hold_storage::Addition { type_id, singleton: true, quantity: 1 }).collect();
    inventory::hold_storage::check_destination(db, sde, j.owner, j.output_location, j.output_flag, &additions, Default::default(), None).await?;
    for _ in 0..count {
        let id = industry_dao::insert_copy(db, type_id, j.owner, j.output_location, j.output_flag, BLUEPRINT_COPY_QUANTITY).await.map_err(db_err)?;
        set_blueprint(db, id, me, te, runs).await?;
        inventory::notifications::notify_item_change(db, sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(LOCATION_JUNKYARD))]).await?;
    }
    Ok(())
}

/// Deliver a finished job's output to its selected custody location.
/// Legacy jobs whose blueprint was already moved or lost before custody prevention are
/// refused at delivery; an operator may need to repair their blueprint custody.
async fn deliver_job<X: Send>(db: &(impl Executor + ?Sized), sde: &std::sync::Arc<Sde>, outbox: &mut Outbox<X>, j: &mut Job, by: i64, invention_successful: i32, completed: i64, fact_source: (Option<i64>, Option<i64>, Option<i64>)) -> Result<(), CallError> {
    let bp = blueprint(db, sde, j.blueprint_id).await?;
    if bp.item_id != j.blueprint_id || bp.type_id != j.blueprint_type || bp.owner != j.owner
        || bp.location != j.blueprint_location || bp.flag != j.blueprint_flag
        || bp.copy != j.blueprint_copy || bp.station != Some(j.facility)
    {
        return Err(user_error(messages::MSG_INDUSTRY_BLUEPRINT_ACCESS_DENIED, vec![]));
    }
    let successful = if j.activity == INVENTION { invention_successful } else { j.runs };
    j.status = STATUS_DELIVERED;
    j.successful_runs = Some(successful);
    j.completed = Some(completed);
    j.completed_by = Some(by);
    if industry_dao::deliver_job(db, j, INDUSTRY_JOB_STATUS_READY).await.map_err(db_err)? == 0 {
        return Err(notify_error(messages::INDUSTRY_PROJECT_ALREADY_DELIVERED));
    }
    let act = sde.industry.activity(j.blueprint_type, j.activity);
    match (j.activity, &act) {
        (MANUFACTURING, Some(act)) => {
            if let Some(&(product, per_run, _)) = act.products.iter().find(|p| Some(p.0) == j.product_type).or(act.products.first()) {
                let quantity = manufacturing_quantity(per_run, j.runs)?;
                use crate::dao::items::parent_locks::{LockRequest, LockStrength};
                use crate::services::inventory::destination::Destination;
                let destination = Destination::owned(j.owner, j.output_location, j.output_flag);
                let preparation = items::DepositPreparation::capture(db, sde, &destination, product).await?;
                let blueprint_write = LockRequest { item_id: bp.item_id, strength: LockStrength::Update };
                let extra = if bp.copy && bp.runs - j.runs <= 0 { std::slice::from_ref(&blueprint_write) } else { &[] };
                items::deposit_planned(db, sde, outbox, &preparation, quantity, LOCATION_JUNKYARD, Default::default(), None, extra).await?;
                let context = fact_context(db, by, fact_source.0, fact_source.1, fact_source.2).await?;
                outbox.event_from(SourceKey::new("ManufacturingDelivered", j.id), GameEvent::ManufacturingDelivered {
                    char_id: by, installer_id: j.installer, job_id: j.id, owner_id: j.owner,
                    owner_is_corporation: j.owner != j.installer, product_type: product, quantity,
                    facility_id: j.facility, output_location_id: j.output_location, occurred_at: completed, context,
                });
            }
            if bp.copy {
                let left = bp.runs - j.runs;
                if left <= 0 {
                    destroy_blueprint(db, sde, outbox, bp.item_id).await?;
                } else {
                    set_blueprint(db, bp.item_id, bp.me, bp.te, left).await?;
                }
            }
        }
        (RESEARCH_TIME, _) => {
            set_blueprint(db, bp.item_id, bp.me, (bp.te + 2 * j.runs).min(MAX_TIME_EFFICIENCY), bp.runs).await?;
        }
        (RESEARCH_MATERIAL, _) => {
            set_blueprint(db, bp.item_id, (bp.me + j.runs).min(MAX_MATERIAL_EFFICIENCY), bp.te, bp.runs).await?;
        }
        (COPYING, _) => {
            make_copies(db, sde, outbox, j, j.blueprint_type, j.runs, bp.me, bp.te, j.licensed_runs).await?;
        }
        (INVENTION, Some(act)) => {
            let (_, d_me, d_te, d_runs) = match j.optional_type {
                Some(d) => decryptor(sde, d),
                None => (1.0, 0, 0, 0),
            };
            if let Some(&(product, per_run, _)) = act.products.iter().find(|p| Some(p.0) == j.product_type).or(act.products.first()) {
                if successful > 0 {
                    let runs = per_run.checked_add(i64::from(d_runs)).and_then(|n| i32::try_from(n.max(1)).ok())
                        .ok_or_else(|| notify_error(messages::BLUEPRINT_RUN_COUNT_EXCEEDED))?;
                    make_copies(db, sde, outbox, j, product, successful, INVENTION_MATERIAL_EFFICIENCY + d_me, INVENTION_TIME_EFFICIENCY + d_te, runs).await?;
                }
            }
            let left = bp.runs - j.runs;
            if left <= 0 {
                destroy_blueprint(db, sde, outbox, bp.item_id).await?;
            } else {
                set_blueprint(db, bp.item_id, bp.me, bp.te, left).await?;
            }
        }
        _ => {}
    }
    tell_job(db, outbox, j).await?;
    Ok(())
}

pub(super) async fn complete(c: &mut Client, id: i64) -> Result<(), CallError> {
    let db = c.db.clone();
    let me = c.session.int("charid").unwrap_or(0);
    let _trading = lock().await;
    let j = job(&db, id).await?;
    authorization::owner(&db, me, j.owner, None).await?;
    if j.status != INDUSTRY_JOB_STATUS_INSTALLED && j.status != INDUSTRY_JOB_STATUS_READY {
        return Err(notify_error(messages::INDUSTRY_PROJECT_ALREADY_DELIVERED));
    }
    if j.status == INDUSTRY_JOB_STATUS_INSTALLED && j.end > filetime_now() {
        return Err(notify_error(messages::INDUSTRY_PROJECT_NOT_COMPLETE));
    }
    let successful = if j.activity == INVENTION { (0..j.runs).filter(|_| rand01() < j.probability).count() as i32 } else { j.runs };
    let completed = filetime_now();
    let fact_ship = c.session.int("shipid").filter(|id| *id > 0);
    let fact_system = c.session.int("solarsystemid2").filter(|id| *id > 0);
    let fact_station = c.session.int("stationid").filter(|id| *id > 0);
    let sde = Arc::clone(&c.sde);
    let delivered = db.transaction(
        |tx, outbox: &mut Outbox| {
            let expected = j.clone();
            let sde = Arc::clone(&sde);
            Box::pin(async move {
                let mut j = job(tx, id).await?;
                authorization::owner(tx, me, j.owner, None).await?;
                if (j.status != INDUSTRY_JOB_STATUS_INSTALLED && j.status != INDUSTRY_JOB_STATUS_READY) || j.activity != expected.activity || j.runs != expected.runs || j.probability != expected.probability {
                    return Err(notify_error(messages::INDUSTRY_PROJECT_ALREADY_DELIVERED));
                }
                if j.status == INDUSTRY_JOB_STATUS_INSTALLED {
                    if j.end > completed { return Err(notify_error(messages::INDUSTRY_PROJECT_NOT_COMPLETE)); }
                    if industry_dao::mark_ready(tx, id, completed).await.map_err(db_err)? == 0 {
                        return Err(notify_error(messages::INDUSTRY_PROJECT_ALREADY_DELIVERED));
                    }
                    j.status = INDUSTRY_JOB_STATUS_READY;
                }
                deliver_job(tx, &sde, outbox, &mut j, me, successful, completed, (fact_ship, fact_system, fact_station)).await?;
                completion::cancel(tx, id).await?;
                Ok(j)
            })
        },
        |j, outbox| async move { dispatch_client(j, outbox, c) },
    ).await?;
    info!("industry: job {} delivered ({}/{} runs)", delivered.id, delivered.successful_runs.unwrap_or(0), delivered.runs);
    Ok(())
}


#[cfg(test)]
mod tests;
