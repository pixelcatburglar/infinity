//! Ordered post-commit service effects and client notifications.
//! Notifications correspond to client scatter events; durable game events are
//! prepared in the source transaction and published after commit.
//! Application-specific effects are dispatched by their owning service, including
//! typed gateway and sovereignty publications.
//! Direct client notifications can also be delivered through a supplied sender.

use std::{convert::Infallible, fmt::Display};
use marshal::Value;
use crate::{client::Client, lua::events::GameEvent, progress::{AfterCommit, Prepare, PrepareCx, PrepareError, Registry}, services::CallError, space::{NotifyTx, World}};

/// Server design: events without producer-specific metadata have an empty context.
const DEFAULT_CONTEXT: &str = "{}";
/// Server design: default origin for events emitted by this server.
const DEFAULT_ORIGIN: &str = "server";

pub struct Outbox<E = Infallible> {
    entries: Vec<Entry<E>>,
}

impl<E> Default for Outbox<E> {
    fn default() -> Self { Self { entries: Vec::new() } }
}

/// Stable identity of a committed business fact, scoped by its event kind.
pub(crate) struct SourceKey {
    key: String,
    ordinal: Option<i64>,
}
impl SourceKey {
    pub(crate) fn new(kind: &'static str, id: i64) -> Self {
        Self { key: format!("{kind}:{id}"), ordinal: None }
    }
    pub(crate) fn canonical(kind: &'static str, identity: impl Display, ordinal: i64) -> Self {
        Self { key: format!("{kind}:{identity}"), ordinal: Some(ordinal) }
    }
    pub(crate) fn as_str(&self) -> &str { &self.key }
}

enum Entry<E> {
    Notification(Notification),
    Operation(E),
    Event { event: GameEvent, occurred_at: i64, source_key: Option<String>, source_ordinal: Option<i64> },
}
pub(crate) enum Effect<E> {
    Notification(Notification),
    Operation(E),
}

pub(crate) enum Notification {
    DirectClient { event: String, args: Vec<Value> },
    Pilot { char_id: i64, event: String, args: Vec<Value> },
    Sender { sender: NotifyTx, event: String, args: Vec<Value> },
    Gateway(crate::gateway::notices::GatewayNotice),
    Sovereignty(crate::services::sovereignty::CommittedPublication),
}

impl<E> Outbox<E> {
    pub fn notify_client(&mut self, event: &str, args: Vec<Value>) {
        self.entries.push(Entry::Notification(Notification::DirectClient { event: event.into(), args }));
    }
    pub fn notify_pilot(&mut self, char_id: i64, event: &str, args: Vec<Value>) {
        self.entries.push(Entry::Notification(Notification::Pilot { char_id, event: event.into(), args }));
    }
    pub fn notify_sender(&mut self, sender: NotifyTx, event: &str, args: Vec<Value>) {
        self.entries.push(Entry::Notification(Notification::Sender { sender, event: event.into(), args }));
    }
    pub(crate) fn notify_gateway(&mut self, notice: crate::gateway::notices::GatewayNotice) {
        self.entries.push(Entry::Notification(Notification::Gateway(notice)));
    }
    pub(crate) fn sovereignty(&mut self, publication: crate::services::sovereignty::CommittedPublication) {
        self.entries.push(Entry::Notification(Notification::Sovereignty(publication)));
    }
    pub fn event(&mut self, event: GameEvent) {
        self.entries.push(Entry::Event { event, occurred_at: crate::client::filetime_now(), source_key: None, source_ordinal: None });
    }
    pub(crate) fn event_from(&mut self, source: SourceKey, event: GameEvent) {
        self.entries.push(Entry::Event {
            event, occurred_at: crate::client::filetime_now(),
            source_key: Some(source.key), source_ordinal: source.ordinal,
        });
    }
    pub fn operation(&mut self, operation: E) { self.entries.push(Entry::Operation(operation)); }
    /// Append a child outbox, retargeting its notifications without discarding
    /// operations or durable events.
    pub(crate) fn append_routed(&mut self, child: Self, mut route: impl FnMut(Notification) -> Notification) {
        self.entries.extend(child.entries.into_iter().map(|entry| match entry {
            Entry::Notification(notification) => Entry::Notification(route(notification)),
            entry => entry,
        }));
    }
    pub(crate) fn into_entries(self) -> Vec<Effect<E>> {
        self.entries.into_iter().map(|entry| match entry {
            Entry::Notification(n) => Effect::Notification(n),
            Entry::Operation(o) => Effect::Operation(o),
            Entry::Event { .. } => panic!("unprepared event reached custom dispatcher"),
        }).collect()
    }
}

fn subject(event: &GameEvent) -> Option<i64> {
    match event {
        GameEvent::CharacterSelected { char_id, .. } | GameEvent::CharacterLoggedOff { char_id, .. }
        | GameEvent::Undocked { char_id, .. } | GameEvent::Docked { char_id, .. }
        | GameEvent::Jumped { char_id, .. } | GameEvent::ShipLost { char_id, .. }
        | GameEvent::NpcKilled { char_id, .. } | GameEvent::ItemsMoved { char_id, .. }
        | GameEvent::SkillTrained { char_id, .. } | GameEvent::SkillStateChanged { char_id }
        | GameEvent::MarketOrderPlaced { char_id, .. }
        | GameEvent::MissionAccepted { char_id, .. } | GameEvent::MissionCompleted { char_id, .. }
        | GameEvent::BallparkBound { char_id, .. } | GameEvent::Targeted { char_id, .. }
        | GameEvent::Damaged { char_id, .. } | GameEvent::ModuleCycleEnded { char_id, .. }
        | GameEvent::WarpArrived { char_id, .. } | GameEvent::OreMined { char_id, .. }
        | GameEvent::GasHarvested { char_id, .. } | GameEvent::ManufacturingInstalled { char_id, .. }
        | GameEvent::ManufacturingDelivered { char_id, .. } | GameEvent::SignatureResolved { char_id, .. }
        | GameEvent::WreckSalvaged { char_id, .. } | GameEvent::ContainerHacked { char_id, .. }
        | GameEvent::LoyaltyPointsEarned { char_id, .. } | GameEvent::DailyGoalCompleted { char_id, .. }
        | GameEvent::EverMarksEarned { char_id, .. } => Some(*char_id),
        GameEvent::WalletChanged { .. } | GameEvent::MarketOrderFilled { .. } => None,
        GameEvent::DamageApplied { .. } | GameEvent::RemoteRepairApplied { .. }
        | GameEvent::WarpScrambleApplied { .. } | GameEvent::NpcDestroyed { .. }
        | GameEvent::LossSettled { .. } | GameEvent::PlayerKillCredited { .. } => crate::lua::events::combat::subject(event),
    }
}

impl<E: Prepare> Prepare for Outbox<E> {
    fn prepare<'a, 'tx>(&'a mut self, tx: &'a tokio_postgres::Transaction<'tx>, context: &'a mut PrepareCx) -> crate::db::transaction::TxFuture<'a, (), PrepareError> {
        Box::pin(async move {
            for entry in &mut self.entries {
                match entry {
                    Entry::Operation(operation) => operation.prepare(tx, context).await?,
                    Entry::Notification(_) => {},
                    Entry::Event { event, occurred_at, source_key, source_ordinal } => {
                        if matches!(event, GameEvent::Targeted { .. } | GameEvent::Damaged { .. } | GameEvent::ModuleCycleEnded { .. } | GameEvent::WarpArrived { .. }) {
                            return Err(PrepareError::Progress(anyhow::anyhow!("live-only event in durable outbox")));
                        }
                        if (crate::lua::events::combat::kind(event).is_some() || crate::lua::events::goals::kind(event).is_some()) && (source_key.is_none() || source_ordinal.is_none()) {
                            return Err(PrepareError::Progress(anyhow::anyhow!("canonical event requires a source and ordinal")));
                        }
                        let (invocation, current) = context.next_source()?;
                        let (namespace, key) = match source_key.take() {
                            Some(key) => ("business".to_owned(), key),
                            None => (invocation, current.to_string()),
                        };
                        *source_key = Some(key.clone());
                        let (event_time, fact_context, origin) = match event.resource_metadata().or_else(|| crate::lua::events::combat::metadata(event)).or_else(|| crate::lua::events::goals::metadata(event)) {
                            Some((time, fact)) => (time, serde_json::to_string(fact).map_err(|e| PrepareError::Progress(e.into()))?, fact.origin.as_str()),
                            None => (*occurred_at, DEFAULT_CONTEXT.to_owned(), DEFAULT_ORIGIN),
                        };
                        context.stage(crate::dao::progress::JournalEvent {
                            source_namespace: namespace, source_key: key, schema_version: crate::progress::codec::SCHEMA_VERSION,
                            kind: event.kind().to_owned(), occurred_at: event_time, recorded_at: crate::client::filetime_now(),
                            payload: crate::progress::codec::encode(event).map_err(PrepareError::Progress)?, context: fact_context,
                            origin: origin.into(), ordinal: match source_ordinal { Some(ordinal) => *ordinal, None => i64::try_from(current).map_err(|e| PrepareError::Progress(e.into()))? },
                            subject_character_id: subject(event),
                        });
                    }
                }
            }
            Ok(())
        })
    }
}
impl<E: AfterCommit> AfterCommit for Outbox<E> {
    fn after_commit(&mut self, registry: &Registry, committed: &std::collections::HashSet<String>) -> crate::progress::AfterCommitHandle {
        let mut actions = Vec::new();
        self.entries.retain_mut(|entry| match entry {
            Entry::Operation(operation) => {
                actions.push(crate::progress::AfterCommitAction::Child(operation.after_commit(registry, committed)));
                true
            }
            Entry::Notification(_) => true,
            Entry::Event { event, source_key, .. } => {
                let key = source_key.as_ref().expect("committed progress event was not prepared");
                if committed.contains(key) { actions.push(crate::progress::AfterCommitAction::Event(event.clone())); }
                false
            }
        });
        crate::progress::AfterCommitHandle::new(registry.clone(), actions)
    }
}

impl Notification {
    pub fn dispatch(self, world: &World, client: Option<&mut Client>) -> Result<(), CallError> {
        match self {
            Self::DirectClient { event, args } => {
                if let Some(client) = client { client.notify(&event, args); }
                else { tracing::warn!(event, "direct notification requires a client"); }
            }
            Self::Pilot { char_id, event, args } => world.notify_pilot(char_id, &event, args),
            Self::Sender { sender, event, args } => { if sender.send((event, args)).is_err() { tracing::warn!("postcommit notification sender closed"); } }
            Self::Gateway(notice) => world.notice_hub().dispatch(notice, |id| world.online_session(id)),
            Self::Sovereignty(publication) => crate::space::sovereignty::publish(world, publication),
        }
        Ok(())
    }

    /// Dispatch a direct client notification through `sender`, preserving other targets.
    /// A closed sender is logged and does not prevent later notifications.
    pub fn dispatch_sender(self, world: &World, sender: &NotifyTx) -> Result<(), CallError> {
        match self {
            Self::DirectClient { event, args } => {
                if sender.send((event, args)).is_err() { tracing::warn!("postcommit notification sender closed"); }
                Ok(())
            }
            notification => notification.dispatch(world, None),
        }
    }
}

pub fn dispatch_world<T>(value: T, outbox: Outbox, world: &World) -> Result<T, CallError> {
    for entry in outbox.into_entries() {
        match entry {
            Effect::Notification(notification) => if let Err(error) = notification.dispatch(world, None) { tracing::warn!("postcommit notification failed: {error:?}"); },
            Effect::Operation(never) => match never {},
        }
    }
    Ok(value)
}
/// Deliver direct client notifications through a sender after commit.
pub fn dispatch_sender<T>(value: T, outbox: Outbox, world: &World, sender: &NotifyTx) -> Result<T, CallError> {
    for entry in outbox.into_entries() {
        match entry {
            Effect::Notification(notification) => if let Err(error) = notification.dispatch_sender(world, sender) { tracing::warn!("postcommit notification failed: {error:?}"); },
            Effect::Operation(never) => match never {},
        }
    }
    Ok(value)
}


pub fn dispatch_client<T>(value: T, outbox: Outbox, client: &mut Client) -> Result<T, CallError> {
    let world = client.world.clone();
    for entry in outbox.into_entries() {
        match entry {
            Effect::Notification(notification) => if let Err(error) = notification.dispatch(&world, Some(&mut *client)) { tracing::warn!("postcommit notification failed: {error:?}"); },
            Effect::Operation(never) => match never {},
        }
    }
    Ok(value)
}

#[cfg(test)] pub(crate) mod tests;
