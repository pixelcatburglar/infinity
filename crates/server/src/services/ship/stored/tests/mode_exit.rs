//! A paid Bastion makes the same otherwise-valid public exits unavailable.
//! The hull and maintenance host are selected from loaded data, with the host's
//! unmodified capacity containing the hull. Retirement uses the paid cycle.
use std::sync::{Arc, atomic::AtomicBool};
use infinity_core::consts::dogma::{ATTR_DISALLOW_EARLY_DEACTIVATION, ATTR_SHIP_MAINTENANCE_BAY_CAPACITY, EFFECT_BASTION};
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_CARGO, GROUP_CAPSULE};
use marshal::Value;
use crate::dao::items::loading::{load_items, ItemQuery};
use crate::services::combat::ActiveCycle;
use crate::services::fit::Key;
use crate::services::skills::gm::{self, Change};
use super::*;

fn bastion_pair(sde: &crate::sde::Sde) -> (i32, i32, i32) {
    let module = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_MODULE)
        && ty.effects().iter().any(|linked| linked.effect_id == EFFECT_BASTION && linked.is_default))
        .map(|ty| ty.type_id).expect("published bastion");
    let data = sde.types.get(module).expect("bastion type");
    let restriction = |prefix: &str| (1..).map_while(|index| sde.dogma.attributes_by_name(&format!("{prefix}{index:02}")).next())
        .filter_map(|attribute| data.attribute(attribute.attribute_id)).map(|value| value as i32).filter(|value| *value > 0).collect::<Vec<_>>();
    let (groups, types) = (restriction("canFitShipGroup"), restriction("canFitShipType"));
    let hull = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_SHIP)
        && ty.group_id != Some(GROUP_CAPSULE) && ty.volume.is_some_and(|volume| volume.is_finite() && volume > 0.0)
        && (groups.contains(&ty.group_id.unwrap_or(0)) || types.contains(&ty.type_id))
        && super::super::super::balls::radius(sde, ty.type_id).is_ok() && sde.is_tradable(ty.type_id)
        && crate::services::inventory::holds::accepts(sde, FLAG_SHIP_HANGAR, ty.type_id)
        && ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS).any(|(&skill, level)| ty.attribute(skill).unwrap_or(0.0) > 0.0 && ty.attribute(level).unwrap_or(0.0) > 0.0))
        .expect("legal bastion hull");
    let volume = hull.volume.expect("hull volume");
    let host = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_SHIP)
        && ty.type_id != hull.type_id && ty.group_id != Some(GROUP_CAPSULE)
        && super::super::super::balls::radius(sde, ty.type_id).is_ok()
        && ty.attribute(ATTR_SHIP_MAINTENANCE_BAY_CAPACITY).is_some_and(|capacity| capacity.is_finite() && capacity >= volume))
        .map(|ty| ty.type_id).expect("legal maintenance host contains the bastion hull");
    (module, hull.type_id, host)
}

fn running_arc(module: i64) -> Option<(Arc<AtomicBool>, i64, i64, u64)> {
    match crate::services::combat::state().active_cycles.get(&module) {
        Some(ActiveCycle::Running { generation, owner, ship, membership, .. }) => Some((generation.clone(), *owner, *ship, *membership)),
        _ => None,
    }
}

async fn cargo(b: &BayFixture, ship: i64) -> Vec<(i64, i64, i64, i32, i64)> {
    load_items(&b.f.db, ItemQuery::Location(ship)).await.unwrap().into_iter()
        .filter(|item| item.flag_id == FLAG_CARGO).map(|item| (item.item_id, item.owner_id, item.location_id, item.flag_id, item.quantity)).collect()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn paid_bastion_refuses_public_exits_until_the_same_requests_commit() {
    crate::tests::init_config();
    let (module_type, hull, host_kind) = {
        let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let sde = crate::tests::shared_sde(&db).await;
        bastion_pair(&sde)
    };
    for method in ["StoreVessel", "Eject", "Board", "BoardStoredShip", "LaunchFromContainer"] {
        let mut b = BayFixture::with_pair(Some((hull, host_kind))).await;
        let result = catch_scenario(Box::pin(async {
            let (ship, owner) = (b.f.ship, b.f.owner);
            let board_target = if method == "Board" {
                let stored = b.stored(b.host_owner).await;
                b.call("LaunchFromContainer", &[Value::Int(b.host), Value::Int(stored)]).await.expect("space target");
                let mut target = b.f.world.ball(b.f.system, stored).await.expect("launched target");
                let pilot = b.f.world.ball(b.f.system, ship).await.expect("current pilot");
                assert_eq!((pilot.owner_id, pilot.char_id, pilot.mode, pilot.cloak), (owner, Some(owner), crate::space::MODE_STOP, 0));
                assert_eq!((target.id, target.owner_id, target.char_id, target.interactive, target.cloak), (stored, owner, None, false, 0));
                target.pos = [pilot.pos[0] + f64::from(pilot.radius) + f64::from(target.radius) + 100.0, pilot.pos[1], pilot.pos[2]];
                replace_ball_preserving_condition(&b, stopped(target)).await;
                let placed = surface_gap(&b, ship, stored).await;
                assert!(placed.is_finite() && placed > 0.0 && placed <= f64::from(infinity_core::consts::MAX_CARGO_CONTAINER_TRANSFER_DISTANCE), "actual surface gap {placed}");
                let authority = b.f.world.with(b.f.system, move |system| system.authorize_board(owner, ship, stored, f64::from(infinity_core::consts::MAX_CARGO_CONTAINER_TRANSFER_DISTANCE)).map(|_| ())).await.unwrap();
                assert!(authority.is_ok(), "board authority before activation: {authority:?}");
                stored
            } else { 0 };
            let flag = crate::services::effects::fitting::rack(&b.f.sde.types, module_type).expect("bastion rack").2;
            let module = crate::services::effects::tests::fixture::item(&b.f.db, module_type, owner, ship, flag, 1, true, true).await.unwrap();
            let skills = crate::services::skills::requirements::requirements(&b.f.sde, module_type).map(|(skill, _)| (skill, 5)).collect::<std::collections::BTreeMap<_, _>>();
            gm::apply(&mut b.f.client, owner, Change::Set(skills.into_iter().collect())).await.expect("bastion skills");
            let duration = b.f.sde.dogma.effect(EFFECT_BASTION).and_then(|effect| effect.duration_attribute_id).expect("duration");
            crate::dao::items::dogma::set_override(&b.f.db, module, duration, 2_000.0).await.unwrap();
            crate::services::effects::tests::fixture::publish_fit(&b.f.db, &b.f.world, b.f.system, owner, ship).await;
            let locked = b.f.world.with(b.f.system, move |system| system.resident_fit(ship, |fit, _| fit.value(Key::Module(module), ATTR_DISALLOW_EARLY_DEACTIVATION))).await.unwrap();
            assert!(locked.is_some_and(|value| value.is_finite() && value > 0.0), "resident effective 906");
            let name = b.f.sde.dogma.effect(EFFECT_BASTION).and_then(|effect| effect.name.clone()).expect("bastion name");
            crate::services::effects::tests::fixture::dogma_call(&mut b.f.client, "Activate", &[Value::Int(module), Value::str(&name), Value::None, Value::Int(100)]).await.ok().expect("bastion");
            for _ in 0..50 {
                if running_arc(module).is_some() { break; }
                assert!(b.f.world.after(b.f.system, 100).await, "actor stopped");
            }
            let (generation, cycle_owner, cycle_ship, membership) = running_arc(module).expect("real running");
            let fitted = b.f.world.with(b.f.system, move |system| system.resident_fit(ship, |fit, _| fit.module_state(module))).await.unwrap().flatten();
            assert_eq!((cycle_owner, cycle_ship, fitted), (owner, ship, Some((true, true))), "fitted active running identity");
            let same = |module| running_arc(module).is_some_and(|(arc, owner, ship, member)| Arc::ptr_eq(&arc, &generation) && owner == cycle_owner && ship == cycle_ship && member == membership);
            if method == "LaunchFromContainer" {
                let session: Vec<_> = b.f.client.session.attributes().map(|(key, value)| (key.to_owned(), value.clone())).collect();
                let held_cargo = cargo(&b, ship).await;
                let launch = b.stored(b.host_owner).await;
                b.call("LaunchFromContainer", &[Value::Int(b.host), Value::Int(launch)]).await.expect("launch while paid");
                let after: Vec<_> = b.f.client.session.attributes().map(|(key, value)| (key.to_owned(), value.clone())).collect();
                assert_eq!(after, session, "launch keeps the session");
                selected(&b, ship).await;
                assert!(same(module), "launch keeps the running arc");
                assert_eq!(cargo(&b, ship).await, held_cargo, "launch consumes no cargo");
                let launched = row(&b, launch).await;
                assert_eq!((launched.owner_id, launched.location_id, launched.flag_id), (owner, b.f.system, 0));
                let avatar = b.f.world.ball(b.f.system, launch).await.expect("launched hull");
                assert_eq!((avatar.char_id, avatar.interactive), (None, false));
                crate::services::effects::tests::fixture::dogma_call(&mut b.f.client, "Deactivate", &[Value::Int(module), Value::str(&name)]).await.ok().expect("launch stop");
                for _ in 0..40 {
                    if crate::services::combat::state().active_cycles.get(&module).is_none() { break; }
                    assert!(b.f.world.after(b.f.system, 100).await, "actor stopped");
                }
                assert!(crate::services::combat::state().active_cycles.get(&module).is_none(), "launch retired");
                let retired = b.f.world.with(b.f.system, move |system| system.resident_fit(ship, |fit, _| fit.module_state(module))).await.unwrap().flatten();
                assert_eq!(retired, Some((true, false)), "launch module is online and inactive");
            } else {
                b.capture_refusals().await;
                let stored_target = if method == "BoardStoredShip" { b.stored(b.host_owner).await } else { 0 };
                let args = match method {
                    "StoreVessel" => vec![Value::Int(b.host)],
                    "Board" => vec![Value::Int(board_target), Value::Int(ship)],
                    "BoardStoredShip" => vec![Value::Int(b.host), Value::Int(stored_target)],
                    _ => vec![],
                };
                b.refuse(method, &args, None).await;
                crate::services::effects::tests::fixture::dogma_call(&mut b.f.client, "Deactivate", &[Value::Int(module), Value::str(&name)]).await.ok().expect("stop");
                assert!(same(module), "requested stop keeps the arc");
                b.refuse(method, &args, None).await;
                for _ in 0..40 {
                    if crate::services::combat::state().active_cycles.get(&module).is_none() { break; }
                    assert!(b.f.world.after(b.f.system, 100).await, "actor stopped");
                }
                assert!(crate::services::combat::state().active_cycles.get(&module).is_none(), "natural retirement");
                let retired = b.f.world.with(b.f.system, move |system| system.resident_fit(ship, |fit, _| fit.module_state(module))).await.unwrap().flatten();
                assert_eq!(retired, Some((true, false)), "retired module is online and inactive");
                if method == "Board" {
                    let committed = b.call(method, &args).await;
                    if committed.is_err() {
                        let gap = surface_gap(&b, ship, board_target).await;
                        let authority = b.f.world.with(b.f.system, move |system| system.authorize_board(owner, ship, board_target, f64::from(infinity_core::consts::MAX_CARGO_CONTAINER_TRANSFER_DISTANCE)).map(|_| ())).await.unwrap();
                        let geometry = b.f.world.with(b.f.system, move |system| [ship, board_target].map(|id| system.sampled_ball(id).map(|ball| (ball.id, ball.owner_id, ball.char_id, ball.interactive, ball.mode, ball.cloak, ball.pos, ball.vel, ball.radius)))).await.unwrap();
                        panic!("board after retirement: {committed:?}; authority {authority:?}; gap {gap}; geometry {geometry:?}");
                    }
                } else { b.call(method, &args).await.expect(method); }
                match method {
                    "StoreVessel" => assert_eq!((row(&b, ship).await.location_id, row(&b, ship).await.flag_id), (b.host, FLAG_SHIP_HANGAR)),
                    "Board" => selected(&b, board_target).await,
                    "BoardStoredShip" => selected(&b, stored_target).await,
                    "Eject" => assert_ne!(b.f.client.session.int("shipid"), Some(ship)),
                    _ => {}
                }
            }
        })).await;
        b.cleanup().await;
        if let Err(panic) = result { std::panic::resume_unwind(panic); }
    }
}
