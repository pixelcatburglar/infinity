//! dogmaIM, bound per location, supplies item attribute and effect state.
//! `eve/client/script/environment/godma.py:2440-2454` consumes all-info state,
//! and `eve/client/script/environment/godma.py:1652-1676` reads active effects.
//! Type attributes: `eve/common/script/dogma/baseDogmaLocation.py:1960-1967`.
//! Static dogma and type data come from the shared SDE; items, base overrides, charges and online
//! state come from the runtime game database. For absent dogma rows, mass, capacity,
//! volume and radius fall back to typed SDE type columns: this is server schema
//! behavior, not a validated client fallback. Effective fitting is the subset in
//! `services/fit.rs`, not a complete implementation of client dogma.
//! Module online changes and resource reconciliation use the shared effects policy.
//! Character brains compile passive skills and active implant and booster modifiers;
//! structure effects remain unsupported.
//! Brain versions are in-memory and reset when the server restarts.
//! Ship priming reports settled runtime capacitor, rack heat and overload state;
//! ships outside space use fitted values and no transient heat.
//! Ship state can also be assembled after reconciliation under the lifecycle lock.
//! Charge sublocations share one info shape for ship priming and first-load
//! notifications; client verification of the latter remains outstanding.
pub(crate) mod brain;
pub(in crate::services) mod info;
pub(in crate::services) mod online;
mod dogma_im;

use crate::client::Client;
use super::{Call, CallResult};

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> CallResult {
    dogma_im::dispatch(c, call).await
}

#[cfg(test)] mod tests;
