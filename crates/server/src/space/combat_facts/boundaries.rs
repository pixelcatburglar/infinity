//! Qualification boundaries for actor-owned combat facts.
//! FILETIME UTC days use the shared client time units; no timer or database is used.
//! The actor must close affected aggregates before publishing a returned boundary.

use std::collections::{HashMap, HashSet};
use infinity_core::consts::FILETIME_DAY;
use super::Refusal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Boundary {
    pub epoch: u64,
    pub at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DayChange {
    pub close_at: i64,
    pub current: Boundary,
}

/// Receipts are retained only for live actor membership or native source scopes.
pub(super) struct Boundaries {
    next_epoch: u64,
    day: Option<i64>,
    global: Boundary,
    scopes: HashMap<i64, Boundary>,
}

impl Boundaries {
    pub fn new() -> Self {
        Self {
            next_epoch: 0,
            day: None,
            global: Boundary { epoch: 0, at: i64::MIN },
            scopes: HashMap::new(),
        }
    }

    pub fn current(&self, source: Option<i64>, target: Option<i64>) -> Boundary {
        // Eligibility intersects scopes: latest time and greatest epoch need
        // not originate from the same receipt.
        [source, target].into_iter().flatten().filter_map(|id| self.scopes.get(&id).copied())
            .fold(self.global, |current, scope| Boundary { epoch: current.epoch.max(scope.epoch), at: current.at.max(scope.at) })
    }
    pub fn day_would_change(&self, at: i64) -> Result<bool, ()> {
        let day = at.div_euclid(FILETIME_DAY);
        match self.day {
            Some(previous) if day < previous => Err(()),
            Some(previous) => Ok(day > previous),
            None => Ok(false),
        }
    }
    /// A day change closes once at the actor's actual observation time.
    /// Regressing clocks and exhausted epochs are refused without changing state.
    pub fn observe_day(&mut self, at: i64) -> Result<Option<DayChange>, ()> {
        let day = at.div_euclid(FILETIME_DAY);
        let start = day.checked_mul(FILETIME_DAY).ok_or(())?;
        match self.day {
            None => {
                self.day = Some(day);
                self.global.at = start;
                Ok(None)
            }
            Some(previous) if day == previous => Ok(None),
            Some(previous) if day < previous => Err(()),
            Some(_) => {
                let epoch = self.next_epoch.checked_add(1).ok_or(())?;
                self.day = Some(day);
                self.next_epoch = epoch;
                self.global = Boundary { epoch, at };
                Ok(Some(DayChange { close_at: at, current: self.global }))
            }
        }
    }

    /// The caller closes all affected keys before making this boundary visible.
    pub fn close_character(&mut self, character: i64, at: i64) -> Result<Boundary, Refusal> {
        if at < self.current(Some(character), None).at { return Err(Refusal::Invalid); }
        let epoch = self.next_epoch.checked_add(1).ok_or(Refusal::Overflow)?;
        let receipt = Boundary { epoch, at };
        self.scopes.insert(character, receipt);
        self.next_epoch = epoch;
        Ok(receipt)
    }

    /// Copy a receipt for actor transfer; the old actor can still have sources.
    pub fn export_scope(&self, character: i64) -> Option<Boundary> {
        self.scopes.get(&character).copied()
    }

    pub fn prune(&mut self, keep: &HashSet<i64>) -> usize {
        let before = self.scopes.len();
        self.scopes.retain(|character, _| keep.contains(character));
        before - self.scopes.len()
    }

    /// An imported receipt keeps the destination's epoch ordering monotonic.
    pub fn import_scope(&mut self, character: i64, receipt: Boundary) -> Result<Boundary, Refusal> {
        let epoch = self.next_epoch.max(receipt.epoch).checked_add(1).ok_or(Refusal::Overflow)?;
        let imported = Boundary { epoch, at: receipt.at.max(self.current(Some(character), None).at) };
        self.scopes.insert(character, imported);
        self.next_epoch = epoch;
        Ok(imported)
    }
}
