//! Analyzer effect validation and timed client notifications.
//!
//! Uses the item database and SDE dogma data; hacking gameplay remains in the
//! hacking service. Timers run on the solar-system actor.
//! Accepted analyzer activation cancels arrival jump cloak after validation.

use crate::client::filetime_now;
use infinity_core::consts::dogma::EFFECT_HACKING;
use crate::sde::Sde;
use crate::services::effects::online::OnlinePolicy;
use infinity_core::messages;
use crate::services::{combat, notify_error, CallResult};
use crate::space::{ms_to_seconds, seconds_to_ms};
use marshal::Value;

use super::{lifecycle, notify, registry, EffectKind};

/// Validate the actual item before emitting an analyzer effect.
pub(in crate::services) async fn start_effect(cx: combat::Ctx, sde: &Sde, policy: &OnlinePolicy, module: i64, target: i64, duration_ms: f64) -> CallResult {
    lifecycle::subject(&cx.db, sde, policy, cx.ship_id, module, EFFECT_HACKING, true).await?;
    if registry::kind(sde, EFFECT_HACKING) != Some(EffectKind::Hacking) {
        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    }
    let ship = cx.ship_id;
    cx.world.post(cx.system_id, move |s| { s.drop_jump_cloak(ship); });
    let started = filetime_now();
    notify::ship_effect(&cx, module, EFFECT_HACKING as i64, Some(target), None, true, started, duration_ms, 0);
    let world = cx.world.clone();
    world.schedule(cx.system_id, seconds_to_ms(ms_to_seconds(duration_ms.max(0.0))), move |s| {
        s.defer(async move {
            notify::ship_effect(&cx, module, EFFECT_HACKING as i64, Some(target), None, false, started, duration_ms, 0);
        });
    });
    Ok(Value::None)
}

#[cfg(test)] mod tests;
