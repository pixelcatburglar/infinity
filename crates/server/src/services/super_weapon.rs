//! `superWeaponMgr`: positional superweapon activation from
//! `eve/client/script/ui/inflight/positionalControl.py:111-120`, validated and
//! routed into the shared effects lifecycle with a typed `SuperWeaponAim`.
//! Point calls carry `(moduleID, effectID, destination)`; slash calls carry
//! `(moduleID, p0, p1)` — the client ignores the return value, so every
//! refusal reaches the pilot as a `CustomNotify`.
use super::{db_err, notify_error, Call, CallError, CallResult};
use crate::client::Client;
use crate::sde::super_weapons::Mode;
use crate::services::combat::Ctx;
use crate::services::effects::parameters::SuperWeaponAim;
use crate::services::identity::Identity;
use crate::services::items::load_items;
use crate::dao::items::loading::ItemQuery;
use crate::space::V3;
use infinity_core::consts::dogma::ATTR_DISALLOW_IN_EMPIRE_SPACE;
use infinity_core::messages;
use marshal::Value;

pub(crate) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "superWeaponMgr" { return None; }
    match call.method {
        "ActivateSinglePointTargetedModule" => Some(point(c, call).await),
        "ActivateSlashModule" => Some(slash(c, call).await),
        _ => None,
    }
}

/// One `float,float,float` argument, all components finite.
fn point_arg(value: Option<&Value>) -> Option<V3> {
    let items = value?.items()?;
    let point: V3 = [
        items.first()?.as_float().or_else(|| items.first()?.as_int().map(|v| v as f64))?,
        items.get(1)?.as_float().or_else(|| items.get(1)?.as_int().map(|v| v as f64))?,
        items.get(2)?.as_float().or_else(|| items.get(2)?.as_int().map(|v| v as f64))?,
    ];
    (items.len() == 3 && point.iter().all(|v| v.is_finite())).then_some(point)
}

async fn point(c: &mut Client, call: &Call<'_>) -> CallResult {
    let invalid = || notify_error(infinity_core::messages::super_weapon::INVALID_REQUEST);
    let module = call.args.first().and_then(Value::as_int).ok_or_else(invalid)?;
    let effect = call.args.get(1).and_then(Value::as_int).ok_or_else(invalid)? as i32;
    let destination = point_arg(call.args.get(2)).ok_or_else(invalid)?;
    activate(c, module, Mode::Point, Some(effect), SuperWeaponAim::Point { destination }).await
}

async fn slash(c: &mut Client, call: &Call<'_>) -> CallResult {
    let invalid = || notify_error(infinity_core::messages::super_weapon::INVALID_REQUEST);
    let module = call.args.first().and_then(Value::as_int).ok_or_else(invalid)?;
    let p0 = point_arg(call.args.get(1)).ok_or_else(invalid)?;
    let p1 = point_arg(call.args.get(2)).ok_or_else(invalid)?;
    activate(c, module, Mode::Slash, None, SuperWeaponAim::Slash { p0, p1 }).await
}

/// Shared admission for both positional RPCs: the module's catalogue row owns
/// the accepted mode and derived effect; the shared lifecycle owns every
/// later check and the activation itself.
async fn activate(c: &mut Client, module: i64, mode: Mode, effect: Option<i32>, aim: SuperWeaponAim) -> CallResult {
    let identity = Identity::of_client(c);
    let Some(cx) = Ctx::from_identity(&identity, &c.db, &c.world, c.session.int("solarsystemid"), c.notify_tx.clone()) else {
        return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    };
    let item = load_items(&cx.db, cx.world.sde(), ItemQuery::Id(module)).await.map_err(db_err)?
        .into_iter().next().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
    let (effect_id, allowed) = match cx.world.sde().super_weapons.get(item.type_id) {
        Some(definition) => (definition.effect, definition.mode == mode),
        None => (0, false),
    };
    if !allowed { return Err(notify_error(messages::super_weapon::UNSUPPORTED_TYPE)); }
    if effect.is_some_and(|effect| effect != effect_id) {
        return Err(notify_error(messages::super_weapon::INVALID_REQUEST));
    }
    // `disallowInEmpireSpace` (attribute 1074) admission: accepted catalogue
    // rows refuse empire space and allow null/wormhole space.
    if cx.world.sde().types.attribute(item.type_id, ATTR_DISALLOW_IN_EMPIRE_SPACE).unwrap_or(0.0) > 0.0
        && cx.world.sde().map.system(cx.system_id).and_then(|s| s.security).unwrap_or(0.0) > 0.0 {
        return Err(notify_error(messages::super_weapon::UNSUPPORTED_TYPE));
    }
    let name = cx.world.sde().dogma.effect(effect_id).and_then(|e| e.name.clone())
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("superweapon effect {effect_id} unnamed")))?;
    let (db, world, sde, notify_tx) = (c.db.clone(), c.world.clone(), c.sde.clone(), c.notify_tx.clone());
    let (system, charid, login_user) = (c.session.int("solarsystemid"), c.session.int("charid"), c.login.user_id);
    let notify = |notification| match notification {
        notification @ crate::db::outbox::Notification::DirectClient { .. } => notification.dispatch(&world, Some(&mut *c)),
        notification => crate::services::effects::online::dispatch_notification(notification, &world, &notify_tx, charid),
    };
    crate::services::effects::lifecycle::activate_inner(&identity, &db, &world, &sde,
        system, notify_tx.clone(), login_user, module, &name, None, 0, Some(aim), notify).await
}

#[cfg(all(test, any(feature = "tests-services", feature = "tests-effects")))]
mod tests;
