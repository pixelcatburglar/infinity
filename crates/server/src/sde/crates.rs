//! Immutable crate presentation catalogue, loaded from native crate rows and server policy.
//! The client crate service consumes preview directories; native rows and embedded JSONL
//! provide their data. Opening, random rolls, special metadata and receipts are not supported.
use std::collections::{BTreeMap, HashMap};

use crate::{db::executor::Executor, sde::types::Types};

mod native;
mod policy;

const POLICY: &str = include_str!("../../content/crates.jsonl");

#[cfg_attr(test, derive(Default))]
pub(crate) struct Crates {
    defs: Vec<Definition>,
    by_id: HashMap<i32, usize>,
}

pub(crate) struct Definition {
    pub type_id: i32,
    pub splash: String,
    native: Option<native::NativeDefinition>,
    policy: Option<policy::Policy>,
    unavailable: Option<Unavailable>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unavailable {
    Metadata,
    UnknownType,
    Ambiguous,
    Malformed,
}

impl Crates {
    pub(crate) async fn load(ex: &(impl Executor + ?Sized), types: &Types) -> anyhow::Result<Self> {
        let rows = crate::dao::sde::crates::load(ex).await?;
        let policies = policy::parse(POLICY, types)?;
        let native = native::parse(rows, types);
        Ok(Self::compose(native, policies, |id| types.get(id).is_some()))
    }

    fn compose(
        native: native::NativeCatalogue,
        mut policies: BTreeMap<i32, policy::Policy>,
        known_type: impl Fn(i32) -> bool,
    ) -> Self {
        let mut defs = Vec::with_capacity(native.definitions.len() + policies.len());
        let mut source_rows = native.untyped_rows.len();
        let untyped = native.untyped_rows.len();
        let (mut native_custom, mut native_fixed, mut unknown, mut native_conflicts, mut malformed, mut special) = (0, 0, 0, 0, 0, 0);
        let (mut resolved_by_policy, mut unresolved_conflicts) = (0, 0);
        let (mut references, mut missing_fields, mut empty_fields, mut custom_fields, mut fixed_fields, mut native_bound) = (0, 0, 0, 0, 0, 0);
        for mut row in native.definitions {
            source_rows += row.source_keys.len();
            references += row.reference_count;
            missing_fields += row.missing_fields;
            empty_fields += row.empty_fields;
            custom_fields += usize::from(row.custom_present);
            fixed_fields += usize::from(row.ordinary_present);
            native_custom += usize::from(!row.custom.is_empty());
            native_fixed += usize::from(!row.ordinary.is_empty());
            native_bound += row.custom.iter().chain(&row.ordinary).filter(|r| r.soulbound).count();
            let is_known = known_type(row.type_id);
            unknown += usize::from(!is_known);
            native_conflicts += usize::from(row.ambiguous);
            malformed += usize::from(row.malformed);
            special += usize::from(row.special);
            let policy = policies.remove(&row.type_id);
            let unavailable = if !is_known { Some(Unavailable::UnknownType) }
                else if row.ambiguous && policy.is_none() { Some(Unavailable::Ambiguous) }
                else if row.special { Some(Unavailable::Metadata) }
                else if row.malformed { Some(Unavailable::Malformed) }
                else { None };
            if row.ambiguous {
                resolved_by_policy += usize::from(unavailable.is_none());
                unresolved_conflicts += usize::from(unavailable.is_some());
            }
            let type_id = row.type_id;
            defs.push(Definition {
                type_id,
                splash: std::mem::take(&mut row.splash),
                native: Some(row),
                policy,
                unavailable,
            });
        }
        for (type_id, policy) in policies {
            defs.push(Definition { type_id, splash: String::new(), native: None, policy: Some(policy), unavailable: None });
        }
        defs.sort_unstable_by_key(|d| d.type_id);
        let by_id = defs.iter().enumerate().map(|(i, d)| (d.type_id, i)).collect();
        let (mut policy_count, mut policy_entries, mut policy_bound, mut policy_groups, mut policy_draws) = (0usize, 0usize, 0usize, 0usize, 0u64);
        for policy in defs.iter().filter_map(|d| d.policy.as_ref()) {
            let summary = policy.summary();
            policy_count += 1;
            policy_entries += summary.entries;
            policy_bound += summary.bound_entries;
            policy_groups += summary.groups;
            policy_draws += u64::from(summary.draws);
        }
        tracing::info!(definitions = defs.len(), source_rows, untyped, policies = policy_count,
            native_custom, native_fixed, unknown, native_conflicts, resolved_by_policy, unresolved_conflicts, malformed, special,
            references, missing_fields, empty_fields, custom_fields, fixed_fields,
            native_bound, policy_entries, policy_bound, policy_groups, policy_draws,
            "crate catalogue loaded");
        Self { defs, by_id }
    }

    pub(crate) fn get(&self, id: i32) -> Option<&Definition> {
        self.by_id.get(&id).map(|&i| &self.defs[i])
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Definition> { self.defs.iter() }
}

impl Definition {
    pub(crate) fn sale_supported(&self) -> bool { false }

    pub(crate) fn preview(&self, custom: bool, race_id: Option<i32>) -> Result<Vec<(i32, i64)>, Unavailable> {
        if let Some(reason) = self.unavailable { return Err(reason); }
        if self.native.as_ref().is_some_and(|native| native.ambiguous) {
            return Ok(if custom { Vec::new() } else {
                self.policy.as_ref().map_or_else(Vec::new, |policy| policy.preview(race_id))
            });
        }
        if let Some(native) = &self.native {
            let rewards = if custom { &native.custom } else { &native.ordinary };
            if !rewards.is_empty() {
                return Ok(rewards.iter().map(|r| (r.type_id, r.quantity)).collect());
            }
            if custom || !native.custom.is_empty() { return Ok(Vec::new()); }
        } else if custom {
            return Ok(Vec::new());
        }
        Ok(self.policy.as_ref().map_or_else(Vec::new, |p| p.preview(race_id)))
    }
}

#[cfg(test)]
mod tests;
