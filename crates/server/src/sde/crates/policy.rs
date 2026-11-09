//! Typed crate reward policies generated from the approved content policy report.
//! The entries are a server policy inferred from localized descriptions, not
//! official client tables. This module validates SDE references and structure;
//! it performs no random selection or inventory writes. Preview uses the
//! inclusive range minimum and never draws.
use crate::sde::types::Types;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Floating-point tolerance for normalized JSON probabilities in this server policy.
const PROBABILITY_EPSILON: f64 = 1.0e-9;
/// Client `raceFlags`: Amarr, Caldari, Gallente and Minmatar respectively.
const RACE_AMARR: i32 = 4;
const RACE_CALDARI: i32 = 1;
const RACE_GALLENTE: i32 = 8;
const RACE_MINMATAR: i32 = 2;

#[derive(Debug, Clone)]
pub(super) struct Policy {
    mode: Mode,
    entries: Vec<Entry>,
    groups: Vec<Group>,
    draws: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode { FixedAll, FixedWeighted, RaceBased, RandomWeighted, RandomMulti }
#[derive(Debug, Clone)]
struct Entry { type_id: i32, min: i64, max: i64, probability: Option<f64>, race: Option<i32>, soulbound: bool }
#[derive(Debug, Clone)]
struct Group { entries: Vec<Entry>, chance: f64 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Summary { pub entries: usize, pub bound_entries: usize, pub groups: usize, pub draws: u32 }

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy { type_id: i32, mode: String, #[serde(default)] entries: Option<Vec<RawEntry>>, #[serde(default)] groups: Option<Vec<RawGroup>>, #[serde(default)] draws: Option<u32> }
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry { type_id: i32, #[serde(default)] quantity: Option<i64>, #[serde(default)] quantity_min: Option<i64>, #[serde(default)] quantity_max: Option<i64>, #[serde(default)] probability: Option<f64>, #[serde(default)] race: Option<String>, #[serde(default)] soulbound: Option<bool> }
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGroup { entries: Vec<RawEntry>, chance: f64 }

pub(super) fn parse(text: &str, types: &Types) -> Result<BTreeMap<i32, Policy>> {
    parse_with_type_exists(text, |id| types.get(id).is_some())
}

fn parse_with_type_exists<F>(text: &str, mut type_exists: F) -> Result<BTreeMap<i32, Policy>>
where F: FnMut(i32) -> bool {
    let mut result = BTreeMap::new();
    let mut previous = None;
    for (line_no, line) in text.lines().enumerate() {
        if line.trim().is_empty() { bail!("policy line {} is empty", line_no + 1); }
        let raw: RawPolicy = serde_json::from_str(line).with_context(|| format!("invalid policy JSON at line {}", line_no + 1))?;
        if raw.type_id <= 0 { bail!("policy line {} has non-positive type_id", line_no + 1); }
        if previous.is_some_and(|id| raw.type_id <= id) { bail!("policy IDs must be unique and ascending"); }
        previous = Some(raw.type_id);
        if !type_exists(raw.type_id) { bail!("policy type_id {} is unknown", raw.type_id); }
        let mode = match raw.mode.as_str() { "fixed_all" => Mode::FixedAll, "fixed_weighted" => Mode::FixedWeighted, "race_based" => Mode::RaceBased, "random_weighted" => Mode::RandomWeighted, "random_multi" => Mode::RandomMulti, _ => bail!("unknown policy mode {}", raw.mode) };
        match mode {
            Mode::FixedAll | Mode::FixedWeighted | Mode::RaceBased if raw.draws.is_some() || raw.groups.is_some() => bail!("fixed/race policy cannot specify draws or groups"),
            Mode::RandomWeighted if raw.groups.is_some() => bail!("random_weighted cannot specify groups"),
            Mode::RandomMulti if raw.entries.is_some() == raw.groups.is_some() => bail!("random_multi must specify exactly one of entries or groups"),
            _ => {}
        }
        let entries = raw.entries.unwrap_or_default().into_iter().map(|e| parse_entry(e, &mut type_exists)).collect::<Result<Vec<_>>>()?;
        let groups = raw.groups.unwrap_or_default().into_iter().map(|g| parse_group(g, &mut type_exists)).collect::<Result<Vec<_>>>()?;
        let draws = raw.draws.unwrap_or(0);
        validate_shape(mode, &entries, &groups, draws)?;
        result.insert(raw.type_id, Policy { mode, entries, groups, draws });
    }
    Ok(result)
}

fn parse_entry<F>(raw: RawEntry, type_exists: &mut F) -> Result<Entry>
where F: FnMut(i32) -> bool {
    if raw.type_id <= 0 || !type_exists(raw.type_id) { bail!("entry type_id {} is unknown or non-positive", raw.type_id); }
    let (min, max) = match (raw.quantity, raw.quantity_min, raw.quantity_max) {
        (Some(quantity), None, None) if quantity > 0 => (quantity, quantity),
        (None, Some(min), Some(max)) if min > 0 && min <= max => (min, max),
        _ => bail!("entry quantity must be a positive scalar or complete valid range"),
    };
    if let Some(probability) = raw.probability { if !probability.is_finite() || !(0.0..=1.0).contains(&probability) { bail!("entry probability must be finite and in [0,1]"); } }
    let race = raw.race.map(|r| match r.as_str() { "Amarr" => Ok(RACE_AMARR), "Caldari" => Ok(RACE_CALDARI), "Gallente" => Ok(RACE_GALLENTE), "Minmatar" => Ok(RACE_MINMATAR), _ => bail!("unknown race {r}"), }).transpose()?;
    Ok(Entry { type_id: raw.type_id, min, max, probability: raw.probability, race, soulbound: raw.soulbound.unwrap_or(false) })
}
fn parse_group<F>(raw: RawGroup, type_exists: &mut F) -> Result<Group>
where F: FnMut(i32) -> bool {
    if !raw.chance.is_finite() || !(0.0..=1.0).contains(&raw.chance) { bail!("group chance must be finite and in [0,1]"); }
    if raw.entries.is_empty() { bail!("random group must not be empty"); }
    let entries = raw.entries.into_iter().map(|e| parse_entry(e, type_exists)).collect::<Result<Vec<_>>>()?;
    Ok(Group { entries, chance: raw.chance })
}

fn validate_shape(mode: Mode, entries: &[Entry], groups: &[Group], draws: u32) -> Result<()> {
    match mode {
        Mode::FixedAll | Mode::RaceBased => {
            if entries.is_empty() || !groups.is_empty() || draws != 0 || entries.iter().any(|e| e.probability.is_some()) { bail!("fixed/race policy has invalid random fields"); }
            for race in [RACE_AMARR, RACE_CALDARI, RACE_GALLENTE, RACE_MINMATAR] {
                let mut totals = BTreeMap::new();
                for entry in entries.iter().filter(|e| e.race.is_none_or(|r| r == race)) {
                    add_quantity(&mut totals, entry.type_id, entry.max)?;
                }
            }
            if mode == Mode::RaceBased && ![RACE_AMARR, RACE_CALDARI, RACE_GALLENTE, RACE_MINMATAR].iter().all(|race| entries.iter().any(|e| e.race.is_none() || e.race == Some(*race))) { bail!("race policy does not cover all starting races"); }
        }
        Mode::FixedWeighted => {
            if entries.is_empty() || !groups.is_empty() || draws != 0 { bail!("fixed_weighted has invalid random fields"); }
            if !entries.iter().any(|e| e.probability.is_some()) { bail!("fixed_weighted requires weighted entries"); }
            if entries.iter().any(|e| e.probability.is_some_and(|p| p <= 0.0)) { bail!("fixed_weighted probabilities must be positive"); }
            validate_sum(entries.iter().filter_map(|e| e.probability))?;
        }
        Mode::RandomWeighted => { if entries.is_empty() || entries.iter().any(|e| e.probability.is_none()) || !groups.is_empty() || draws == 0 { bail!("random_weighted requires entries and positive draws"); } validate_sum(entries.iter().map(|e| e.probability.unwrap()))?; checked_total(entries, draws)?; }
        Mode::RandomMulti => {
            if draws == 0 || (entries.is_empty() == groups.is_empty()) { bail!("random_multi requires entries or groups and positive draws"); }
            if !entries.is_empty() { if entries.iter().any(|e| e.probability.is_none()) { bail!("random_multi entries require probabilities"); } validate_sum(entries.iter().map(|e| e.probability.unwrap()))?; checked_total(entries, draws)?; }
            if !groups.is_empty() {
                if groups.iter().any(|g| g.entries.iter().any(|e| e.probability.is_none())) { bail!("random_multi groups require probabilities"); }
                if groups.iter().filter(|g| g.chance == 1.0).count() != draws as usize { bail!("random_multi guaranteed group count must equal draws"); }
                let mut totals = BTreeMap::new();
                for group in groups {
                    validate_sum(group.entries.iter().map(|e| e.probability.unwrap()))?;
                    if group.chance == 0.0 { continue; }
                    let mut maxima = BTreeMap::new();
                    for entry in group.entries.iter().filter(|e| e.probability.unwrap() > 0.0) {
                        let max = maxima.entry(entry.type_id).or_insert(0i64);
                        *max = (*max).max(entry.max);
                    }
                    for (id, quantity) in maxima { add_quantity(&mut totals, id, quantity)?; }
                }
            }
        }
    }
    Ok(())
}
fn validate_sum(values: impl Iterator<Item = f64>) -> Result<()> { let sum: f64 = values.sum(); if (sum - 1.0).abs() > PROBABILITY_EPSILON { bail!("probabilities must sum to one"); } Ok(()) }
fn add_quantity(totals: &mut BTreeMap<i32, i64>, id: i32, quantity: i64) -> Result<()> {
    let total = totals.entry(id).or_default();
    *total = total.checked_add(quantity).ok_or_else(|| anyhow::anyhow!("aggregate quantity overflow"))?;
    Ok(())
}
fn checked_total(entries: &[Entry], draws: u32) -> Result<()> {
    for entry in entries.iter().filter(|e| e.probability.unwrap_or(1.0) > 0.0) {
        entry.max.checked_mul(i64::from(draws)).ok_or_else(|| anyhow::anyhow!("aggregate quantity overflow"))?;
    }
    Ok(())
}

impl Policy {
    pub(super) fn preview(&self, race_id: Option<i32>) -> Vec<(i32, i64)> {
        let mut out = Vec::new();
        let mut add = |e: &Entry| { if ((self.mode == Mode::FixedAll || self.mode == Mode::FixedWeighted) || e.probability.unwrap_or(1.0) > 0.0) && e.race.is_none_or(|race| Some(race) == race_id) { out.push((e.type_id, e.min)); } };
        match self.mode { Mode::FixedAll | Mode::FixedWeighted | Mode::RaceBased | Mode::RandomWeighted | Mode::RandomMulti => { for e in &self.entries { add(e); } for g in &self.groups { if g.chance > 0.0 { for e in &g.entries { add(e); } } } } }
        out
    }
    pub(super) fn summary(&self) -> Summary { let all = self.entries.iter().chain(self.groups.iter().flat_map(|g| g.entries.iter())); Summary { entries: all.clone().count(), bound_entries: all.filter(|e| e.soulbound).count(), groups: self.groups.len(), draws: self.draws } }
}

#[cfg(test)] mod tests;
