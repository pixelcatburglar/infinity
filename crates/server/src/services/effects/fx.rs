//! Module effect animation publishing through the system's ordered scene
//! channel, mirroring the client's destiny OnSpecialFX stream.
//! Transient effects keep the original fire path with no record; sustained
//! ("Current") effects publish through the actor's replay-registered module
//! effect API. The `graphic_info` payload rides the 14th OnSpecialFX argument
//! verbatim: `Value::None` for every effect except SDE 6995, which carries the
//! `effects.TriglavianBeam` bonus dict.
use super::lifecycle::Module;
use super::registry::{Effect, EffectFlag};
use crate::services::combat::Ctx;
use crate::space::{special_fx, FxModule, system::presentation::{FxRetention, ModuleFx}};
use marshal::Value;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Fire one bank's effect animation. Transient effects keep the original fire
/// path before any typed construction; Current effects publish through the
/// actor's replay-registered module effect API.
#[allow(clippy::too_many_arguments)]
pub(super) fn weapon_fx(cx: &Ctx, modules: &[Module], effect: &Effect, target: Option<i64>, charge: Option<i32>, start: bool, duration_ms: f64, repeat: i64, master: i64, generation: &Arc<AtomicBool>, retention: FxRetention, graphic_info: Value) {
    let Some(guid) = effect.guid.as_deref().and_then(|g| g.split(',').next()).map(str::to_owned) else { return };
    if let FxRetention::Transient = retention {
        let modules = modules.to_vec();
        let ship = cx.ship_id;
        let is_offensive = effect.has(EffectFlag::Offensive);
        cx.world.publish_destiny(cx.system_id, ship, move |stamp| {
            modules.iter().map(|m| special_fx(stamp, ship,
                Some(FxModule { item: Some(m.id), type_id: m.type_id }), target, charge.map(i64::from),
                &guid, is_offensive, i64::from(start), i64::from(start), Value::Float(duration_ms),
                repeat, None, 0, graphic_info.clone())).collect()
        });
        return;
    }
    let modules = modules.to_vec();
    let system = cx.system_id;
    let is_offensive = effect.has(EffectFlag::Offensive);
    let generation = generation.clone();
    cx.world.post(system, move |s| {
        for module in &modules {
            s.publish_module_fx(master, &generation, ModuleFx {
                module: FxModule { item: Some(module.id), type_id: module.type_id },
                guid: guid.clone(),
                charge,
                offensive: is_offensive,
                duration_ms,
                repeat,
                graphic_info: graphic_info.clone(),
            }, start, FxRetention::Current);
        }
    });
}
