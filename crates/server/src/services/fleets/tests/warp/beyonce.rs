//! Bound beyonce item warp: trained admission and scrambled pilots keep Wall slots.
use super::*;
use super::fixture;

fn target_ball(id: i64, pos: space::V3) -> space::Ball {
    let mut target = ball(id, 0);
    target.char_id = None;
    target.pos = pos;
    target.old_pos = pos;
    target.radius = 100.0;
    target
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn scrambled_and_untrained_keep_frozen_slots() {
    let db = fixture::db().await;
    let sde = crate::tests::shared_sde(&db).await;
    let system = sde.map.systems().next().expect("SDE has a system").solar_system_id;
    let mut rig = fixture::FleetWarp::new(&db, system, 3, [1e11, 0.0, 0.0]).await;
    let (m0, m1) = (rig.members[0].ship, rig.members[1].ship);
    let m2 = rig.members[2].ship;
    let target = -3;
    let result = crate::tests::catch_scenario(async {
        rig.client.world.add_ship(system, target_ball(target, [2e11, 0.0, 0.0])).await.unwrap();
        crate::dao::items::dogma::set_override(&db, m1, infinity_core::consts::dogma::ATTR_WARP_SCRAMBLE_STATUS, 1.0).await.unwrap();
        crate::dao::items::dogma::set_override(&db, rig.commander.ship, infinity_core::consts::dogma::ATTR_WARP_SCRAMBLE_STATUS, 1.0).await.unwrap();
        let bind = Value::Int(system);
        let args = [Value::str("item"), Value::Int(target)];
        let kwargs = fixture::kwargs(true, Some(fixture::settings(3, 5000, 10_000)));
        // Untrained fleets refuse before any pilot moves.
        fixture::user_error(fixture::dispatch(&mut rig.client, "beyonce", "CmdWarpToStuff",
            &args, &kwargs, Some(&bind)).await, messages::fleets::INVALID_REQUEST);
        rig.assert_no_warp().await;
        // Only the formation row stored: the missing coordination row reads 0.
        crate::dao::skills::training::set_level(&db, rig.commander.fixture.character, fixture::FORMATION_SKILL, 3, 0).await.unwrap();
        fixture::user_error(fixture::dispatch(&mut rig.client, "beyonce", "CmdWarpToStuff",
            &args, &kwargs, Some(&bind)).await, messages::fleets::INVALID_REQUEST);
        rig.assert_no_warp().await;
        fixture::train(&db, rig.commander.fixture.character, 3, 4).await;
        fixture::dispatch(&mut rig.client, "beyonce", "CmdWarpToStuff",
            &args, &kwargs, Some(&bind)).await.unwrap();
        rig.settle().await;
        // Wall leaves the refused middle slot and commander's centre empty.
        let radius = rig.client.world.ball(system, rig.commander.ship).await.unwrap().radius as f64;
        let anchor = [2e11 - crate::space::system::warp::ordinary_warp_standoff(Some(100.0), radius, 0.0), 0.0, 0.0];
        for (ship, want) in [(m0, space::add(anchor, [0.0, 5000.0, 0.0])),
                             (m2, space::add(anchor, [0.0, 0.0, 5000.0]))] {
            let (goto, stand) = rig.warped(ship).await.unwrap();
            assert!(space::dist(goto, want) < 1e-3 && stand == 0.0);
        }
        for ship in [m1, rig.commander.ship] { assert!(rig.warped(ship).await.is_none()); }
        assert_eq!(fixture::fleet_warps(&mut rig.members[1].notify), 0);
        assert_eq!(fixture::fleet_warps(&mut rig.commander.notify), 0);
        assert_eq!(fixture::fleet_warps(&mut rig.members[0].notify), 1);
        assert_eq!(fixture::fleet_warps(&mut rig.members[2].notify), 1);
    }).await;
    rig.client.world.remove_ball(system, target).await;
    rig.finish().await;
    result.unwrap();
}
