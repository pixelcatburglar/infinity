//! Jump clones (jumpCloneSvc, bound per station; eve/client/script/ui/services/clonejumpsvc.py).
//!
//! A character can keep one jump clone per station (game.jump_clones), as many as
//! Infomorph Psychology and Advanced Infomorph Psychology levels allow. Clone jumping from a
//! station to one of them leaves the current body behind as a clone there and wakes the
//! pilot in a fresh capsule at the destination. The jump cooldown and its reduction per
//! Infomorph Synchronizing level are configured by the operator. Implants aren't modelled,
//! so clones carry none and cost nothing. Ship-borne clone vat bays aren't supported.
//! Clone destinations are resolved from the startup SDE after checking game ownership.

mod jump_clone_svc;
mod policy;

use super::{Call, CallResult};
use crate::client::Client;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    jump_clone_svc::dispatch(c, call).await
}
