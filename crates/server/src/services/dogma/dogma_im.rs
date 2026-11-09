//! `dogmaIM` RPC routing for `eve/client/script/dogma/` and `godma.py`.
//! Handlers use game item and dogma tables, SDE data and the space actor.
//! Targeting on behalf of another object is not supported and is refused.
//! State and gameplay mechanics remain in the delegated feature modules.
use super::{brain, info::*};
use crate::services::{db_err, keyval, value_id_list, Call, CallResult};
use crate::services::{exploration, skills};
use crate::services::combat::{banks, targeting_charges};
use crate::services::effects::{heat, lifecycle};
use crate::services::identity::Identity;
use crate::services::items::load_items;
use crate::dao::items::{loading::ItemQuery, dogma as dogma_dao};
use crate::client::{short, Client};
use marshal::Value;
use tracing::warn;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> CallResult {
    let flag = |i: usize| call.args.get(i).map(|v| !v.is_none() && v.as_int() != Some(0)).unwrap_or(false);
    match call.method {
        "AddTargetOBO" | "RemoveTargetOBO" => Err(crate::services::notify_error(infinity_core::messages::ship::TARGET_OBO_UNAVAILABLE)),
        "CreateNewbieShip" => crate::services::fitting::newbie::ship(c, call).await,
        "CreateNewbieBonusItems" => crate::services::fitting::newbie::bonus(c, call).await,
        "AddTarget" => targeting_charges::add_target(c, call.args.first().and_then(|v| v.as_int()).unwrap_or(0)).await,
        "CancelAddTarget" => {
            if let (Some(ch), Some(t)) = (c.session.int("charid"), call.args.first().and_then(|v| v.as_int())) {
                targeting_charges::cancel_target(ch, t);
            }
            Ok(Value::None)
        }
        "RemoveTarget" => {
            let t = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let identity = Identity::of_client(c);
            let world = c.world.clone();
            let system = c.session.int("solarsystemid");
            targeting_charges::drop_targets(&identity, &world, system, &[t], |event, args| c.notify(event, args));
            Ok(Value::None)
        }
        "RemoveTargets" => {
            let identity = Identity::of_client(c);
            let world = c.world.clone();
            let system = c.session.int("solarsystemid");
            targeting_charges::drop_targets(&identity, &world, system, &value_id_list(call.args.first()), |event, args| c.notify(event, args));
            Ok(Value::None)
        }
        "ClearTargets" => {
            if let Some(ch) = c.session.int("charid") {
                let targets = targeting_charges::clear_character_locks(&c.world, ch);
                targeting_charges::remove_target_projections(&Identity::of_client(c), &c.world, c.session.int("solarsystemid"), targets);
            }
            c.notify("OnTarget", vec![Value::str("clear")]);
            Ok(Value::None)
        }
        "GetTargets" => Ok(Value::list(c.session.int("charid").map(targeting_charges::targets_of).unwrap_or_default().into_iter().map(Value::Int))),
        "GetTargeters" => Ok(Value::list([])),
        "LoadAmmo" => targeting_charges::load_ammo(c, call).await,
        "UnloadAmmo" => targeting_charges::unload_ammo(c, call).await,
        "LinkWeapons" => banks::weapon_groups(c, call, banks::Operation::LinkWeapons).await,
        "LinkAllWeapons" => banks::weapon_groups(c, call, banks::Operation::LinkAllWeapons).await,
        "UnlinkModule" => banks::weapon_groups(c, call, banks::Operation::UnlinkModule).await,
        "UnlinkAllModules" => banks::weapon_groups(c, call, banks::Operation::UnlinkAllModules).await,
        "MergeModuleGroups" => banks::weapon_groups(c, call, banks::Operation::MergeModuleGroups).await,
        "PeelAndLink" => banks::weapon_groups(c, call, banks::Operation::PeelAndLink).await,
        "DestroyWeaponBank" => banks::weapon_groups(c, call, banks::Operation::DestroyWeaponBank).await,
        "GetWeaponBankInfoForShip" => banks::weapon_groups(c, call, banks::Operation::GetWeaponBankInfoForShip).await,
        "Overload" => heat::rpc::handle(c, call.args, false, true).await,
        "StopOverload" => heat::rpc::handle(c, call.args, false, false).await,
        "OverloadRack" => heat::rpc::handle(c, call.args, true, true).await,
        "StopOverloadRack" => heat::rpc::handle(c, call.args, true, false).await,
        "Activate" | "Deactivate" => {
            let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let effect_name = call.args.get(1).and_then(|v| v.as_text()).map(|s| s.into_owned()).unwrap_or_default();
            let target = call.args.get(2).and_then(|v| v.as_int());
            let repeat = call.args.get(3).and_then(|v| v.as_int()).unwrap_or(0);
            let identity = Identity::of_client(c);
            let db = c.db.clone();
            let world = c.world.clone();
            let sde = c.sde.clone();
            let space_system = c.session.int("solarsystemid");
            let sender = c.notify_tx.clone();
            let login_user = c.login.user_id;
            let notify = |notification| match notification {
                notification @ crate::db::outbox::Notification::DirectClient { .. } => notification.dispatch(&world, Some(&mut *c)),
                notification => crate::services::effects::online::dispatch_notification(notification, &c.world, &c.notify_tx, c.session.int("charid")),
            };
            if call.method == "Activate" {
                lifecycle::activate(&identity, &db, &world, &sde, space_system, sender, login_user, item_id, &effect_name, target, repeat, notify).await
            } else {
                lifecycle::deactivate(&identity, &db, &world, &sde, space_system, sender, login_user, item_id, &effect_name, notify).await
            }
        }
        "ChangeDroneSettings" => {
            let owner = c.session.int("charid").ok_or_else(|| crate::services::notify_error(infinity_core::messages::drones::NOT_AVAILABLE))?;
            let patch = crate::services::drones::settings::parse(call.args)?;
            let defaults = crate::services::drones::settings::defaults(&c.sde)?;
            crate::services::drones::settings::apply(owner, defaults, patch);
            Ok(Value::None)
        }
        // GetAllInfo(getCharInfo, getShipInfo, structureID)
        "GetAllInfo" => {
            let char_info = if flag(0) {
                let ch = character_info(c).await?;
                let char_id = c.session.int("charid").unwrap_or(0);
                // (charItems, brain); brain = (version, charEffects, shipEffects, structureEffects)
                // -- godma.Prime unpacks "_, charBrain = allInfo.charInfo".
                let brain = brain::bootstrap(c).await?;
                Value::tuple([Value::Dict(vec![(Value::Int(char_id), ch)]), brain])
            } else {
                Value::None
            };
            let ship_info = if flag(1) { Value::Dict(ship_info(c).await?) } else { Value::None };
            Ok(keyval([
                ("charInfo", char_info),
                ("shipInfo", ship_info),
                ("structureInfo", Value::None),
                ("shipModifiedCharAttribs", Value::None),
                ("systemWideEffectsOnShip", Value::None),
                // (instanceCache, instanceFlagQuantityCache, weaponBanks, heatStates) for MakeShipActive.
                ("shipState", match c.session.int("shipid") {
                    Some(ship) => ship_state(c, ship).await?,
                    None => Value::tuple([Value::Dict(vec![]), Value::Dict(vec![]), Value::Dict(vec![]), Value::Dict(vec![])]),
                }),
            ]))
        }
        "ShipGetInfo" => Ok(Value::Dict(ship_info(c).await?)),
        "CharGetInfo" => character_info(c).await,
        "ItemGetInfo" => {
            let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let items = load_items(&c.db, &c.sde, ItemQuery::Id(item_id)).await.map_err(db_err)?;
            match items.into_iter().next() {
                Some(item) => {
                    let mut attrs = type_attributes(&c.sde, item.type_id);
                    overlay_overrides(&mut attrs, dogma_dao::overrides(&c.db, item.item_id).await.map_err(db_err)?
                        .into_iter().map(|row| (row.attribute_id, row.value)));
                    let online = online_modules(c, item.location_id).await?.contains(&item.item_id);
                    let char_id = c.session.int("charid").unwrap_or(0);
                    let active = if online { online_effect(item.item_id, char_id, item.location_id) } else { vec![] };
                    Ok(info(item.row(), item.item_id, attrs, active))
                }
                None if Some(item_id) == c.session.int("charid") => character_info(c).await,
                None => Ok(Value::None),
            }
        }
        "GetCharacterBaseAttributes" => {
            let ch = character_info(c).await?;
            Ok(ch)
        }
        "CheckSendLocationInfo" | "LogAttribute" => Ok(Value::None),
        "InjectSkillIntoBrain" => skills::books::inject(c, call.args).await,
        "InjectImplant" => skills::brain_items::inject_implant(c, call.args).await,
        "DestroyImplant" => skills::brain_items::destroy_implant(c, call.args).await,
        "UseBooster" => skills::brain_items::use_booster(c, call.args).await,
        "LaunchProbes" => exploration::launch_probes(c, call).await,
        // Both RPCs use the lifecycle's fitted-subject authorization and notifications.
        "SetModuleOnline" | "TakeModuleOffline" => {
            let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let module = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            let online = call.method == "SetModuleOnline";
            crate::services::effects::online::set_online(&Identity::of_client(c), &c.db, &c.world, &c.sde, c.notify_tx.clone(), c.login.user_id, ship, module, online, |notification| crate::services::effects::online::dispatch_notification(notification, &c.world, &c.notify_tx, c.session.int("charid"))).await
        }
        _ => {
            warn!(
                user = c.login.user_id,
                "UNHANDLED dogmaIM::{} args={} kwargs={}",
                call.method,
                short(&marshal::Value::Tuple(call.args.to_vec())),
                short(call.kwargs)
            );
            Ok(Value::None)
        }
    }
}
