//! Owner-bound daily-goal queries over retained content and frozen assets.
//! Client daily-goal views use game daily-goal tables and the recorded package.
//! Queries never settle rewards or mutate progress.
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use crate::{dao::{characters::persistence, daily_goals::{instances::{self, Cursor, GoalRow}, periods, rewards}}, lua::content::{PinnedPolicy, daily_goals::plans::{self, ProjectInput, Projection}}, services::{identity::Identity, subscriptions::rules}};
use super::{assignment, frozen, Error, GoalView, HistoryPage, Resources};

pub(crate) struct ReadGoal { pub view: GoalView, pub policy: PinnedPolicy }

async fn owner(resources: &Resources, user: i64, character: i64) -> Result<(), Error> {
    if user <= 0 || character <= 0 { return Err(Error::Unauthenticated); }
    if !persistence::owned(&resources.db, user, character).await? { return Err(Error::Forbidden); }
    Ok(())
}


async fn read_rows_authorized(resources: &Resources, user: i64, character: i64, rows: Vec<GoalRow>, at: i64, mut pins: HashMap<(String, String), PinnedPolicy>) -> Result<Vec<ReadGoal>, Error> {
    let omega = rules::current(&resources.db, user, at).await? == rules::OMEGA;
    let ids: Vec<String> = rows.iter().map(|row| row.goal_id.clone()).collect();
    let assets = rewards::for_goals(&resources.db, character, &ids).await?;
    let mut assets_by_goal: HashMap<String, Vec<_>> = HashMap::new();
    for asset in assets { assets_by_goal.entry(asset.goal_id.clone()).or_default().push(asset); }
    let mut result = Vec::with_capacity(rows.len());
    for goal in rows {
        let key = (goal.namespace.clone(), goal.version.clone());
        if !pins.contains_key(&key) { pins.insert(key.clone(), resources.policies.resolve(&key.0, &key.1).await?); }
        let policy = pins.get(&key).ok_or(Error::Unavailable)?.clone();
        let rewards = assets_by_goal.remove(&goal.goal_id).unwrap_or_default();
        let snapshot = frozen::validate(&goal)?;
        result.push(ReadGoal { view: GoalView { goal, snapshot, rewards, current_omega: omega }, policy });
    }
    Ok(result)
}

pub(crate) async fn read_one(resources: &Resources, user: i64, character: i64, id: &str, at: i64) -> Result<ReadGoal, Error> {
    crate::uuid_text::parse(id, crate::uuid_text::Case::Lowercase).ok_or(Error::Invalid)?;
    owner(resources, user, character).await?;
    let row = instances::get(&resources.db, character, id).await?.ok_or(Error::Missing)?;
    read_rows_authorized(resources, user, character, vec![row], at, HashMap::new()).await?.pop().ok_or(Error::Missing)
}

fn project(rows: Vec<ReadGoal>) -> Result<Vec<GoalView>, Error> {
    let mut groups: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (index, row) in rows.iter().enumerate() { groups.entry((row.view.goal.namespace.clone(), row.view.goal.version.clone())).or_default().push(index); }
    let mut selected = vec![false; rows.len()];
    for indexes in groups.values() {
        let input = ProjectInput { entries: indexes.iter().map(|&i| Projection { key: rows[i].view.goal.goal_id.clone(), selected: true }).collect() };
        let keys = plans::project(&rows[indexes[0]].policy, &input, None)?.keys;
        for &i in indexes { selected[i] = keys.contains(&rows[i].view.goal.goal_id); }
    }
    Ok(rows.into_iter().zip(selected).filter_map(|(row, selected)| selected.then_some(row.view)).collect())
}

pub(super) async fn load_current_authorized(resources: &Resources, user: i64, character: i64, at: i64, bounds: &periods::Bounds, pins: HashMap<(String, String), PinnedPolicy>) -> Result<Vec<GoalView>, Error> {
    let mut rows = instances::period(&resources.db, character, "day", bounds.day_start).await?;
    rows.extend(instances::period(&resources.db, character, "month", bounds.month_start).await?);
    if rows.len() != 13 { return Err(Error::Unavailable); }
    rows.sort_by(|a,b| {
        let rank = |row: &GoalRow| match (row.kind.as_str(), row.category.as_str()) {
            ("day", "ordinary") => 0, ("day", "bonus") => 1,
            ("month", _) if !row.slot_key.starts_with("omega:") => 2, _ => 3,
        };
        (rank(a), if a.kind == "month" { a.target } else { 0 }, a.slot_key.as_str()).cmp(&(rank(b), if b.kind == "month" { b.target } else { 0 }, b.slot_key.as_str()))
    });
    let rows = read_rows_authorized(resources, user, character, rows, at, pins).await?;
    if rows.iter().filter(|r| r.view.goal.kind == "day" && r.view.goal.category == "ordinary").count() != 4
        || rows.iter().filter(|r| r.view.goal.kind == "day" && r.view.goal.category == "bonus").count() != 1
        || rows.iter().filter(|r| r.view.goal.kind == "month" && !r.view.snapshot.template.omega_required).count() != 4
        || rows.iter().filter(|r| r.view.goal.kind == "month" && r.view.snapshot.template.omega_required).count() != 4 {
        return Err(Error::Unavailable);
    }
    let projected = project(rows)?;
    if projected.len() != 13 { return Err(Error::Unavailable); }
    Ok(projected)
}
pub(crate) async fn current(resources: &Resources, identity: &Identity, user: i64, at: i64) -> Result<Vec<GoalView>, Error> {
    let outcome = assignment::ensure_at(resources, user, identity.char_id, at, true).await?;
    if let Some(views) = outcome.current { return Ok(views); }
    if outcome.day.is_none() || outcome.month.is_none() { return Err(Error::Unavailable); }
    load_current_authorized(resources, user, identity.char_id, at, &outcome.bounds, outcome.pins).await
}
pub(crate) async fn get(resources: &Resources, identity: &Identity, user: i64, id: &str, at: i64) -> Result<GoalView, Error> {
    project(vec![read_one(resources, user, identity.char_id, id, at).await?])?.pop().ok_or(Error::Missing)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Token { owner: i64, version: u8, high_time: i64, high_id: String, last_time: i64, last_id: String }
impl Token {
    fn parse(text: &str, owner: i64) -> Result<Option<Self>, Error> {
        if text.is_empty() { return Ok(None); }
        let t: Self = serde_json::from_str(text).map_err(|_| Error::Invalid)?;
        crate::uuid_text::parse(&t.high_id, crate::uuid_text::Case::Lowercase).ok_or(Error::Invalid)?;
        crate::uuid_text::parse(&t.last_id, crate::uuid_text::Case::Lowercase).ok_or(Error::Invalid)?;
        if t.owner != owner || t.version != 1 || t.high_time <= 0 || t.last_time <= 0 || (t.last_time, &t.last_id) > (t.high_time, &t.high_id) { return Err(Error::Invalid); }
        Ok(Some(t))
    }
}
/// Client history requests use 25 entries when the size field is absent.
const DEFAULT_HISTORY_SIZE: u32 = 25;
pub(crate) async fn history(resources: &Resources, identity: &Identity, user: i64, size: u32, token: &str, maximum: u32, _at: i64) -> Result<HistoryPage, Error> {
    owner(resources, user, identity.char_id).await?;
    let size = if size == 0 { DEFAULT_HISTORY_SIZE } else { size };
    if size == 0 || size > maximum { return Err(Error::Invalid); }
    let token = Token::parse(token, identity.char_id)?;
    let high = token.as_ref().map(|t| Cursor { completed_at: t.high_time, goal_id: t.high_id.clone() });
    let last = token.as_ref().map(|t| Cursor { completed_at: t.last_time, goal_id: t.last_id.clone() });
    let cap = i64::from(size) + 1;
    let rows = instances::history(&resources.db, identity.char_id, high.as_ref(), last.as_ref(), cap, i64::from(maximum) + 1).await?;
    let next = rows.len() > size as usize;
    let page = &rows[..rows.len().min(size as usize)];
    let mut groups: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (index, row) in page.iter().enumerate() { groups.entry((row.namespace.clone(), row.version.clone())).or_default().push(index); }
    let mut selected = vec![false; page.len()];
    for ((namespace, version), indexes) in groups {
        let pin = resources.policies.resolve(&namespace, &version).await?;
        for chunk in indexes.chunks(plans::MAX_PLAN_ENTRIES) {
            let input = ProjectInput { entries: chunk.iter().map(|&i| Projection { key: page[i].goal_id.clone(), selected: true }).collect() };
            let keys = plans::project(&pin, &input, None)?.keys;
            for &i in chunk { selected[i] = keys.contains(&page[i].goal_id); }
        }
    }
    let ids = page.iter().zip(selected).filter_map(|(row, selected)| selected.then(|| row.goal_id.clone())).collect();
    let next_token = if next {
        let first = high.unwrap_or_else(|| Cursor { completed_at: rows[0].completed_at, goal_id: rows[0].goal_id.clone() });
        let last = &page[page.len()-1];
        serde_json::to_string(&Token { owner: identity.char_id, version: 1, high_time: first.completed_at, high_id: first.goal_id, last_time: last.completed_at, last_id: last.goal_id.clone() }).map_err(anyhow::Error::from)?
    } else { String::new() };
    Ok(HistoryPage { ids, next_token })
}

#[cfg(test)] mod tests;
