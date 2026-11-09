//! Process-local diagnostics for dogma effects that cannot be executed.
//!
//! Effect names and type IDs come from the SDE and activation requests. This
//! records refusals only; it does not implement the unsupported effects. The
//! client receives a CustomNotify error, as used by its UserError handling.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard, OnceLock};

use marshal::Value;
use tracing::warn;

use infinity_core::messages;

#[derive(Default)]
struct State {
    effects: BTreeMap<String, BTreeSet<i32>>,
}

impl State {
    fn record(&mut self, effect_name: &str, type_id: i32) -> bool {
        if let Some(types) = self.effects.get_mut(effect_name) {
            types.insert(type_id)
        } else {
            self.effects.insert(effect_name.to_owned(), BTreeSet::from([type_id]));
            true
        }
    }

    fn snapshot(&self) -> Vec<(String, i32)> {
        self.effects.iter().flat_map(|(name, types)| {
            types.iter().map(move |&type_id| (name.clone(), type_id))
        }).collect()
    }
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap()
}

/// Return the distinct refused effect names and type IDs in stable order.
pub(crate) fn snapshot() -> Vec<(String, i32)> {
    state().snapshot()
}

/// Record an unsupported effect and return the client-visible refusal.
pub(crate) fn refusal(effect_id: Option<i32>, effect_name: &str, type_id: i32) -> super::super::CallError {
    let first = state().record(effect_name, type_id);
    if first {
        warn!(effect = effect_name, type_id, ?effect_id, "unsupported effect");
    }
    super::super::user_error(
        messages::MSG_CUSTOM_NOTIFY,
        vec![("notify".into(), Value::unicode(messages::effects::effect_not_supported(effect_name)))],
    )
}

#[cfg(test)]
mod tests;
