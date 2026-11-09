//! Station jump-clone custody for jumpCloneSvc, mirroring clonejumpsvc.py.
//! Clone bodies and real implant custody live in game.jump_clones and game.items;
//! ship clone bays and body death are outside this service.
//!
//! Lifecycle and transition transaction rules are transport-independent. Their
//! caller holds the market guard through commit and ordered effect dispatch;
//! a caller applying a transition must apply its session before those effects.

mod jump_clone_svc;
mod lifecycle;
mod policy;
mod transition;

use super::{Call, CallResult};
use crate::{client::Client, db::outbox::Outbox, services::{effects::online::OnlineChanged, trade::TradeEffect}};

/// Existing server policy: maximum jump clone name characters.
pub(super) const MAX_CLONE_NAME_LENGTH: usize = 100;

pub(super) enum CloneEffect {
    Trade(Outbox<TradeEffect>),
    Online(OnlineChanged),
}
impl crate::progress::Prepare for CloneEffect {
    fn prepare<'a, 'tx>(&'a mut self, tx: &'a tokio_postgres::Transaction<'tx>, context: &'a mut crate::progress::PrepareCx) -> crate::db::transaction::TxFuture<'a, (), crate::progress::PrepareError> {
        match self {
            Self::Trade(outbox) => crate::progress::Prepare::prepare(outbox, tx, context),
            Self::Online(change) => crate::progress::Prepare::prepare(change, tx, context),
        }
    }
}
impl crate::progress::AfterCommit for CloneEffect {
    fn after_commit(&mut self, registry: &crate::progress::Registry, committed: &std::collections::HashSet<String>) -> crate::progress::AfterCommitHandle {
        match self {
            Self::Trade(outbox) => crate::progress::AfterCommit::after_commit(outbox, registry, committed),
            Self::Online(change) => crate::progress::AfterCommit::after_commit(change, registry, committed),
        }
    }
}
impl From<OnlineChanged> for CloneEffect {
    fn from(value: OnlineChanged) -> Self { Self::Online(value) }
}

pub(super) struct Transition {
    pub station: i64,
    pub system: i64,
    pub constellation: i64,
    pub region: i64,
    pub capsule: i64,
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    jump_clone_svc::dispatch(c, call).await
}
