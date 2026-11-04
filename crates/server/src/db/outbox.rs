//! Ordered post-commit service effects and client notifications.
//! Notifications correspond to client scatter events; no database tables are accessed here.
//! Application-specific effects must be dispatched by their owning service.
//! Direct client notifications can also be delivered through a supplied sender.

use std::convert::Infallible;
use marshal::Value;
use crate::{client::Client, services::CallError, space::{NotifyTx, World}};

pub struct Outbox<E = Infallible> {
    entries: Vec<Effect<E>>,
}

impl<E> Default for Outbox<E> {
    fn default() -> Self { Self { entries: Vec::new() } }
}

pub(crate) enum Effect<E> {
    Notification(Notification),
    Operation(E),
    Event(crate::lua::events::GameEvent),
}

pub(crate) enum Notification {
    DirectClient { event: String, args: Vec<Value> },
    Pilot { char_id: i64, event: String, args: Vec<Value> },
    Sender { sender: NotifyTx, event: String, args: Vec<Value> },
    Gateway(crate::gateway::notices::GatewayNotice),
}

impl<E> Outbox<E> {
    pub fn notify_client(&mut self, event: &str, args: Vec<Value>) {
        self.entries.push(Effect::Notification(Notification::DirectClient { event: event.into(), args }));
    }
    pub fn notify_pilot(&mut self, char_id: i64, event: &str, args: Vec<Value>) {
        self.entries.push(Effect::Notification(Notification::Pilot { char_id, event: event.into(), args }));
    }
    pub fn notify_sender(&mut self, sender: NotifyTx, event: &str, args: Vec<Value>) {
        self.entries.push(Effect::Notification(Notification::Sender { sender, event: event.into(), args }));
    }
    pub(crate) fn notify_gateway(&mut self, notice: crate::gateway::notices::GatewayNotice) {
        self.entries.push(Effect::Notification(Notification::Gateway(notice)));
    }
    pub fn event(&mut self, event: crate::lua::events::GameEvent) { self.entries.push(Effect::Event(event)); }
    pub fn operation(&mut self, operation: E) { self.entries.push(Effect::Operation(operation)); }
    pub(crate) fn into_entries(self) -> Vec<Effect<E>> { self.entries }
}

impl Notification {
    pub fn dispatch(self, world: &World, client: Option<&mut Client>) -> Result<(), CallError> {
        match self {
            Self::DirectClient { event, args } => {
                let client = client.ok_or_else(|| CallError::Internal(anyhow::anyhow!("direct notification requires a client")))?;
                client.notify(&event, args);
            }
            Self::Pilot { char_id, event, args } => world.notify_pilot(char_id, &event, args),
            Self::Sender { sender, event, args } => { let _ = sender.send((event, args)); }
            Self::Gateway(notice) => world.notice_hub().dispatch(notice, |id| world.online_session(id)),
        }
        Ok(())
    }

    /// Dispatch a direct client notification through `sender`, preserving other targets.
    /// A closed sender is ignored, as with `Sender` notifications.
    pub fn dispatch_sender(self, world: &World, sender: &NotifyTx) -> Result<(), CallError> {
        match self {
            Self::DirectClient { event, args } => {
                let _ = sender.send((event, args));
                Ok(())
            }
            notification => notification.dispatch(world, None),
        }
    }
}

pub fn dispatch_world<T>(value: T, outbox: Outbox, world: &World) -> Result<T, CallError> {
    for entry in outbox.into_entries() {
        match entry {
            Effect::Notification(notification) => notification.dispatch(world, None)?,
            Effect::Event(event) => crate::lua::on_event(event),
            Effect::Operation(never) => match never {},
        }
    }
    Ok(value)
}

pub fn dispatch_client<T>(value: T, outbox: Outbox, client: &mut Client) -> Result<T, CallError> {
    let world = client.world.clone();
    for entry in outbox.into_entries() {
        match entry {
            Effect::Notification(notification) => notification.dispatch(&world, Some(&mut *client))?,
            Effect::Event(event) => crate::lua::on_event(event),
            Effect::Operation(never) => match never {},
        }
    }
    Ok(value)
}

