use super::*;
use crate::services::{self, CallError};
/// Client inventorycommon.const.flagFighterTube0.
const FIGHTER_TUBE_0: i64 = 159;


const ACTIONS: [(&str, &str, &str); 12] = [
    ("fighterMgr", "ExecuteMovementCommandOnFighters", infinity_core::messages::fighters::UNAVAILABLE),
    ("fighterMgr", "LoadFightersToTube", infinity_core::messages::fighters::UNAVAILABLE),
    ("fighterMgr", "UnloadTubeToFighterBay", infinity_core::messages::fighters::UNAVAILABLE),
    ("fighterMgr", "LaunchFightersFromTubes", infinity_core::messages::fighters::UNAVAILABLE),
    ("fighterMgr", "RecallFightersToTubes", infinity_core::messages::fighters::UNAVAILABLE),
    ("fighterMgr", "CmdAbandonFighter", infinity_core::messages::fighters::UNAVAILABLE),
    ("fighterMgr", "CmdScoopAbandonedFighterFromSpace", infinity_core::messages::fighters::UNAVAILABLE),
    ("fighterMgr", "CmdActivateAbilitySlots", infinity_core::messages::fighters::UNAVAILABLE),
    ("fighterMgr", "CmdDeactivateAbilitySlots", infinity_core::messages::fighters::UNAVAILABLE),
    ("warpVectorMgr", "UseVector", infinity_core::messages::warp_vector::UNAVAILABLE),
    ("superWeaponMgr", "ActivateSinglePointTargetedModule", infinity_core::messages::super_weapon::UNAVAILABLE),
    ("superWeaponMgr", "ActivateSlashModule", infinity_core::messages::super_weapon::UNAVAILABLE),
];

fn args(method: &str, id: i64) -> Vec<Value> {
    let ids = || Value::list([Value::Int(id)]);
    let point = || Value::tuple([Value::Int(1), Value::Int(0), Value::Int(0)]);
    match method {
        "ExecuteMovementCommandOnFighters" => vec![ids(), Value::str("STOP")],
        "LoadFightersToTube" => vec![Value::Int(id), Value::Int(FIGHTER_TUBE_0)],
        "CmdScoopAbandonedFighterFromSpace" => vec![Value::Int(id), Value::Int(i64::from(infinity_core::consts::FLAG_CARGO))],
        "UnloadTubeToFighterBay" => vec![Value::Int(FIGHTER_TUBE_0)],
        "LaunchFightersFromTubes" => vec![Value::list([Value::Int(FIGHTER_TUBE_0)])],
        "RecallFightersToTubes" => vec![ids()],
        "CmdAbandonFighter" | "UseVector" => vec![Value::Int(id)],
        "CmdActivateAbilitySlots" | "CmdDeactivateAbilitySlots" => vec![ids(), Value::Int(0)],
        "ActivateSinglePointTargetedModule" => vec![Value::Int(id), Value::Int(0), point()],
        "ActivateSlashModule" => vec![Value::Int(id), point(), point()],
        _ => unreachable!(),
    }
}

fn refusal(error: CallError, text: &str) {
    let CallError::User { msg, dict } = error else { panic!("expected UserError") };
    assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
    assert_eq!(dict, vec![("notify".into(), Value::unicode(text))]);
    let wire = crate::client::user_error(&msg, dict);
    assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
}

#[test]
fn fighter_and_positional_boundaries_wire_and_fallthrough() {
    let empty = Value::Dict(vec![]);
    let bad = Value::dict([("unexpected", Value::Bool(true))]);
    for (service, method, text) in ACTIONS {
        let arguments = args(method, 1);
        for (arguments, kwargs) in [(&arguments[..], &empty), (&[Value::None][..], &bad)] {
            let call = Call { service, method, args: arguments, kwargs, bound: false, bind_params: None };
            refusal(root(&call).unwrap().unwrap_err(), text);
        }
        let wrong = Call { service: "other", method, args: &[], kwargs: &empty, bound: false, bind_params: None };
        assert!(root(&wrong).is_none());
    }
    for (service, method) in [("fighterMgr", "Unknown"), ("warpVectorMgr", "Unknown"), ("superWeaponMgr", "Unknown"), ("fighterMgr", "GetFighterAbilityRearmPerChargeBonusTime")] {
        let call = Call { service, method, args: &[], kwargs: &empty, bound: false, bind_params: None };
        assert!(root(&call).is_none() && station(&call).is_none());
    }
    let call = Call { service: "fighterMgr", method: "GetFightersForShip", args: &[], kwargs: &empty, bound: false, bind_params: None };
    assert!(root(&call).is_none());
    let snapshot = station(&call).unwrap().unwrap();
    assert_eq!(snapshot, Value::tuple([Value::list([]), Value::list([]), Value::Dict(vec![])]));
    assert_eq!(marshal::loads(&marshal::dumps(&snapshot)).unwrap(), snapshot);
}

async fn rows(db: &crate::db::Db, owner: i64, other: i64, ship: i64, foreign: i64) -> String {
    use crate::dao::items::loading::{load_items, ItemQuery};
    let mut snapshot = String::new();
    for id in [ship, foreign] {
        let items = load_items(db, ItemQuery::Id(id)).await.unwrap();
        for item in items {
            snapshot.push_str(&format!("{:?}", (item.item_id, item.type_id, item.owner_id, item.location_id, item.flag_id, item.quantity, item.singleton, item.custom_info)));
        }
    }
    for id in [owner, other] {
        let ship = crate::dao::characters::selection::ship_id(db, id).await.unwrap();
        let system = crate::dao::characters::crimewatch::solar_system(db, id).await.unwrap();
        let balance = crate::dao::wallets::characters::balance(db, id).await.unwrap();
        snapshot.push_str(&format!("{:?}", (ship, system, balance)));
    }
    snapshot
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn real_dispatch_refusals_preserve_fixture_and_world() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = services::tests::fixture::create(&db).await.unwrap();
    let other = services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, peer) = services::tests::client(test_db.clone(), owner.character).await;
        client.login.user_id = owner.account;
        let system = client.sde.map.systems().next().unwrap().solar_system_id;
        let ship_type = client.sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
        let ship = crate::dao::items::transfers::insert(&test_db, ship_type, owner.character, system, 0, 1).await.unwrap();
        let foreign = crate::dao::items::transfers::insert(&test_db, ship_type, other.character, system, 0, 1).await.unwrap();
        crate::dao::characters::selection::set_ship(&test_db, owner.character, ship).await.unwrap();
        crate::dao::characters::ship::undock(&test_db, owner.character, system).await.unwrap();
        // Place the stationary ship beyond every loaded ball's collision sphere,
        // rather than at the system origin (which can lie inside a celestial).
        client.world.system(system).await.unwrap();
        let x = client.world.with(system, |s| {
            s.balls().map(|ball| ball.pos[0] + f64::from(ball.radius))
                .fold(0.0_f64, f64::max) + 1_000_000_000.0
        }).await.unwrap();
        let position = [x, 0.0, 0.0];
        client.world.add_ship(system, crate::space::Ball {
            id: ship, type_id: ship_type, owner_id: owner.character, corp_id: 1, alliance_id: -1,
            char_id: Some(owner.character), signature_radius: Some(1.0), mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
            max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
            pos: position, vel: [0.0; 3], goto: position, old_pos: position, old_vel: [0.0; 3], segment_acceleration: None,
            follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: true,
            massive: true, warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
            cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
        }).await.unwrap();
        let (notify, mut notifications) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = notify.clone();
        client.world.register_pilot(owner.character, notify);
        client.session.apply(vec![("stationid", Value::None), ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        let empty = Value::Dict(vec![]);
        let bad = Value::dict([("unexpected", Value::Bool(true))]);
        let snapshot_call = Call { service: "fighterMgr", method: "GetFightersForShip", args: &[], kwargs: &empty, bound: false, bind_params: None };
        let snapshot = services::dispatch(&mut client, &snapshot_call).await.unwrap();
        assert_eq!(snapshot, Value::tuple([Value::list([]), Value::list([]), Value::Dict(vec![])]));
        assert_eq!(marshal::loads(&marshal::dumps(&snapshot)).unwrap(), snapshot);
        for role in [0, i64::MAX] {
            client.login.role = role;
            client.session.apply(vec![("role", Value::Int(role))]);
            for (service, method, text) in ACTIONS {
                let valid = args(method, ship);
                let foreign_args = args(method, foreign);
                for (arguments, kwargs) in [(&valid[..], &empty), (&foreign_args[..], &empty), (&[Value::None][..], &bad)] {
                    let before = rows(&test_db, owner.character, other.character, ship, foreign).await;
                    let ball = client.world.ball(system, ship).await.unwrap();
                    let state = (ball.pos, ball.vel, ball.goto, ball.mode, ball.follow_id, ball.follow_range, ball.health);
                    let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
                    let outbox = client.outbox.len();
                    let call = Call { service, method, args: arguments, kwargs, bound: false, bind_params: None };
                    refusal(services::dispatch(&mut client, &call).await.unwrap_err(), text);
                    assert_eq!(rows(&test_db, owner.character, other.character, ship, foreign).await, before);
                    let ball = client.world.ball(system, ship).await.unwrap();
                    assert_eq!((ball.pos, ball.vel, ball.goto, ball.mode, ball.follow_id, ball.follow_range, ball.health), state);
                    assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
                    assert_eq!(client.outbox.len(), outbox);
                    assert!(notifications.try_recv().is_err());
                    let mut byte = [0];
                    assert_eq!(peer.try_read(&mut byte).unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
                }
            }
        }
    }).await;
    services::tests::fixture::cleanup(&db, owner).await.unwrap();
    services::tests::fixture::cleanup(&db, other).await.unwrap();
    result.unwrap();
}
