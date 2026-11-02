//! Sequential, process-local event and periodic Lua execution; schedules reset on reload.
//! Ticks are not persistent jobs and carry no client context.

use std::{sync::{Arc, atomic::Ordering}, time::{Duration, SystemTime, UNIX_EPOCH}};
use tokio::{sync::{mpsc, watch}, time::{Instant, Interval}};
use crate::config::lua::LuaConfig;
use super::{events::GameEvent, registry::Registry};

/// Nanoseconds per second, as defined by `Duration`.
const NANOS_PER_SECOND: u128 = 1_000_000_000;

struct Schedule {
    next: Instant,
    timer: Interval,
}

fn schedules(registry: &Registry) -> Vec<Option<Schedule>> {
    let now = Instant::now();
    registry.claims.ticks.iter().map(|tick| {
        let Some(next) = now.checked_add(tick.interval) else {
            tracing::warn!(script = %tick.handler.script, name = %tick.name, "Lua tick deadline exceeds timer range");
            return None;
        };
        Some(Schedule { next, timer: tokio::time::interval_at(next, tick.interval) })
    }).collect()
}

fn skip_overdue(registry: &Registry, schedules: &mut [Option<Schedule>], started: Instant, fired: Option<usize>) {
    let now = Instant::now();
    for (index, (tick, schedule)) in registry.claims.ticks.iter().zip(schedules).enumerate() {
        let Some(active) = schedule else { continue };
        if (active.next <= started && fired != Some(index)) || now < active.next { continue; }
        let elapsed = now.duration_since(active.next);
        let period = active.timer.period();
        let count = elapsed.as_nanos() / period.as_nanos() + 1;
        let count = u64::try_from(count).unwrap_or(u64::MAX);
        tick.skipped.fetch_add(count, Ordering::Relaxed);
        tracing::warn!(script = %tick.handler.script, name = %tick.name, skipped = count, "Lua tick occurrences skipped");
        let remainder = elapsed.as_nanos() % period.as_nanos();
        let remaining = period - Duration::new((remainder / NANOS_PER_SECOND) as u64, (remainder % NANOS_PER_SECOND) as u32);
        if let Some(next) = now.checked_add(remaining) {
            active.next = next;
            active.timer.reset_at(next);
        } else {
            tracing::warn!(script = %tick.handler.script, name = %tick.name, "Lua tick deadline exceeds timer range");
            *schedule = None;
        }
    }
}

enum Selected {
    Tick(usize),
    Event(GameEvent),
}

async fn dispatch_selected(
    selected: Selected, config: &LuaConfig, generations: &mut watch::Receiver<Arc<Registry>>,
    registry: &mut Arc<Registry>, schedule: &mut Vec<Option<Schedule>>,
) -> bool {
    match generations.has_changed() {
        Ok(true) => {
            *registry = generations.borrow_and_update().clone();
            *schedule = schedules(registry);
            if matches!(selected, Selected::Tick(_)) { return true; }
        }
        Ok(false) => {}
        Err(_) => return false,
    }
    let started = Instant::now();
    match selected {
        Selected::Tick(index) => {
            let tick = &registry.claims.ticks[index];
            let active = schedule[index].as_mut().expect("active tick");
            if let Some(next) = active.next.checked_add(active.timer.period()) {
                active.next = next;
            } else {
                tracing::warn!(script = %tick.handler.script, name = %tick.name, "Lua tick deadline exceeds timer range");
                schedule[index] = None;
            }
            let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
            let argument = i64::try_from(now_ms).map(mlua::Value::Integer).map_err(mlua::Error::external);
            if let Err(error) = super::invoke_no_context(registry, config, &tick.handler, argument).await {
                tracing::warn!(script = %tick.handler.script, name = %tick.name, "Lua tick handler failed: {error}");
            }
            skip_overdue(registry, schedule, started, Some(index));
        }
        Selected::Event(event) => {
            super::invoke_event(registry, config, &event).await;
            skip_overdue(registry, schedule, started, None);
        }
    }
    true
}

pub async fn run(config: LuaConfig, mut generations: watch::Receiver<Arc<Registry>>, mut events: mpsc::UnboundedReceiver<GameEvent>) {
    let mut registry = generations.borrow_and_update().clone();
    let mut schedule = schedules(&registry);
    let mut events_open = true;
    loop {
        match generations.has_changed() {
            Ok(true) => {
                registry = generations.borrow_and_update().clone();
                schedule = schedules(&registry);
            }
            Ok(false) => {}
            Err(_) => break,
        }
        tokio::select! {
            changed = generations.changed() => {
                if changed.is_err() { break; }
                registry = generations.borrow_and_update().clone();
                schedule = schedules(&registry);
            }
            index = std::future::poll_fn(|cx| {
                for (index, item) in schedule.iter_mut().enumerate() {
                    if let Some(item) = item {
                        if item.timer.poll_tick(cx).is_ready() { return std::task::Poll::Ready(index); }
                    }
                }
                std::task::Poll::Pending
            }) => {
                if !dispatch_selected(Selected::Tick(index), &config, &mut generations, &mut registry, &mut schedule).await { break; }
            }
            event = events.recv(), if events_open => {
                match event {
                    Some(event) => {
                        if !dispatch_selected(Selected::Event(event), &config, &mut generations, &mut registry, &mut schedule).await { break; }
                    }
                    None => events_open = false,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
