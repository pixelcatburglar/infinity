//! Deterministic period assignment using retained daily content.
//! Daily client periods are persisted in game daily-goal tables.
//! Progress and reward settlement are separate operations.
use std::{collections::HashMap, sync::Arc};
use crate::{dao::{characters::persistence, daily_goals::{instances::{self, GoalRow}, periods::{self, Bounds, PeriodRow}, rewards}}, db::outbox::{self, Outbox}, lua::{self, content::{PinnedPolicy, daily_goals::{model::{Category, Template}, plans::{self, AssignInput}}}, events::combat::BinaryAmount}};
use super::{activation, frozen, ids, queries, wire, Error, Resources, DAILY_GOALS_NAMESPACE};

pub(crate) struct EnsureOutcome { pub bounds: Bounds, pub day: Option<PeriodRow>, pub month: Option<PeriodRow>, pub created: bool, pub current: Option<Vec<super::GoalView>>, pub(super) pins: HashMap<(String, String), PinnedPolicy> }
struct Prepared { period: PeriodRow, goals: Vec<(GoalRow, Vec<rewards::RewardRow>)>, policy: PinnedPolicy }

fn prepare(resources: &Resources, token: &activation::PeriodPolicy, character: i64, at: i64) -> Result<Prepared, Error> {
    let pin = token.policy();
    let definitions = pin.daily_goals().ok_or(Error::Unavailable)?;
    let period = PeriodRow { character_id: character, kind: token.kind().into(), period_start: token.start(), period_end: token.end(), assigned_at: at, generation: 0, namespace: pin.namespace().into(), version: pin.version().into(), revision: 0 };
    let mut templates: Vec<(String, &Template)> = Vec::new();
    if token.kind() == "day" {
        let plan = plans::assign(pin, &AssignInput { entropy: ids::entropy(character, token.start(), pin.version(), 0) }, None)?;
        for (i, key) in plan.keys.iter().enumerate() {
            templates.push((format!("ordinary:{i}"), definitions.template(key).ok_or(Error::Unavailable)?));
        }
        templates.push(("bonus".into(), definitions.bonus()));
    } else {
        for (i, template) in definitions.monthly_base().iter().enumerate() { templates.push((format!("base:{i}"), template)); }
        for (i, template) in definitions.monthly_omega().iter().enumerate() { templates.push((format!("omega:{i}"), template)); }
    }
    let mut goals = Vec::with_capacity(templates.len());
    for (slot, template) in templates {
        let category = match template.category { Category::Ordinary => "ordinary", Category::Bonus => "bonus", Category::Month => "month" };
        let snapshot = frozen::snapshot(&resources.sde, template)?;
        let goal = GoalRow { goal_id: ids::goal(character, token.start(), pin.version(), &slot, 0), character_id: character, kind: token.kind().into(), period_start: token.start(), period_end: token.end(), slot_key: slot, generation: 0, namespace: pin.namespace().into(), version: pin.version().into(), category: category.into(), definition: serde_json::to_string(&snapshot).map_err(anyhow::Error::from)?, target: template.target, progress: 0, remainder: BinaryAmount { coefficient: "0".into(), exponent: 0 }, completed_at: None, earned_omega: false, paid: false, voided_at: None, has_unclaimed: false, revision: 0 };
        let assets = frozen::rewards(&goal, template)?;
        goals.push((goal, assets));
    }
    goals.sort_by(|a,b| a.0.goal_id.cmp(&b.0.goal_id));
    Ok(Prepared { period, goals, policy: pin.clone() })
}

pub(crate) async fn ensure_at(resources: &Resources, user: i64, character: i64, at: i64, notify_current: bool) -> Result<EnsureOutcome, Error> {
    if user <= 0 || character <= 0 { return Err(Error::Unauthenticated); }
    if !persistence::owned(&resources.db, user, character).await? { return Err(Error::Forbidden); }
    let bounds = periods::bounds(&resources.db, at).await?;
    let day = periods::get(&resources.db, character, "day", bounds.day_start).await?;
    let month = periods::get(&resources.db, character, "month", bounds.month_start).await?;
    if day.is_some() && month.is_some() { return Ok(EnsureOutcome { bounds, day, month, created: false, current: None, pins: HashMap::new() }); }
    let _admission = if notify_current {
        let current = lua::content_policy(DAILY_GOALS_NAMESPACE)?;
        Some(lua::admit_content(&current)?)
    } else { None };
    let mut history = activation::load(resources).await?;
    let mut prepared = Vec::new();
    for (missing, kind, start, end) in [(day.is_none(), "day", bounds.day_start, bounds.day_end), (month.is_none(), "month", bounds.month_start, bounds.month_end)] {
        if missing {
            if let Some(token) = activation::resolve(resources, &mut history, kind, start, end).await? { prepared.push(prepare(resources, &token, character, at)?); }
        }
    }
    prepared.sort_by(|a,b| (a.period.character_id, &a.period.kind, a.period.period_start).cmp(&(b.period.character_id, &b.period.kind, b.period.period_start)));
    if prepared.is_empty() { return Ok(EnsureOutcome { bounds, day, month, created: false, current: None, pins: HashMap::new() }); }
    let prepared = Arc::new(prepared);
    let transaction_prepared = Arc::clone(&prepared);
    resources.db.transaction::<_, _, Error, Outbox, _, _, _>(|tx, _outbox| {
        let mut day = day.clone();
        let mut month = month.clone();
        let bounds = bounds.clone();
        let prepared = Arc::clone(&transaction_prepared);
        Box::pin(async move {
            let mut created = false;
            let mut inserted = Vec::new();
            for p in prepared.iter() {
                let won = periods::insert(tx, &p.period).await?;
                let winner = if won { created = true; inserted.push(p); p.period.clone() }
                    else { periods::get(tx, character, &p.period.kind, p.period.period_start).await?.ok_or(Error::Unavailable)? };
                if p.period.kind == "day" { day = Some(winner); } else { month = Some(winner); }
            }
            let mut all: Vec<_> = inserted.into_iter().flat_map(|p| p.goals.iter()).collect();
            all.sort_by(|a,b| a.0.goal_id.cmp(&b.0.goal_id));
            for (goal, assets) in all {
                if !instances::insert(tx, goal).await? { return Err(anyhow::anyhow!("daily goal identity collision").into()); }
                for asset in assets { if !rewards::insert(tx, character, asset).await? { return Err(anyhow::anyhow!("daily reward identity collision").into()); } }
            }
            Ok(EnsureOutcome { bounds, day, month, created, current: None, pins: HashMap::new() })
        })
    }, |mut outcome, mut outbox| async move {
        if notify_current {
            for p in prepared.iter() {
                let winner = if p.period.kind == "day" { &outcome.day } else { &outcome.month };
                if winner.as_ref().is_some_and(|row| row.namespace == p.period.namespace && row.version == p.period.version) {
                    outcome.pins.insert((p.period.namespace.clone(), p.period.version.clone()), p.policy.clone());
                }
            }
        }
        if outcome.created && notify_current {
            let views = queries::load_current_authorized(resources, user, character, at, &outcome.bounds, std::mem::take(&mut outcome.pins)).await?;
            outbox.notify_gateway(wire::current_notice(character, &views)?);
            outcome.current = Some(views);
        }
        outbox::dispatch_world(outcome, outbox, &resources.world).map_err(Error::from)
    }).await
}

#[cfg(test)] mod tests;
