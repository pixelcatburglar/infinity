//! Bound agentMgr home-base warp and sixth positional formation settings.
use super::*;
use super::fixture;
use crate::sde::Sde;

/// A retail basic agent whose home station exists in the SDE map: (agentID, system).
fn basic_agent(sde: &Sde) -> (i64, i64) {
    let Some(Value::Instance(_, state)) = infinity_content::retail::cached_call("agentMgr", "GetAgents", &[]) else { panic!("retail agent list missing") };
    for line in state.get("lines").and_then(|l| l.items()).unwrap_or_default() {
        // appConst agentTypeBasic
        let agent = line.items().and_then(|f| f.first()).and_then(|v| v.as_int()).and_then(crate::services::missions::agent);
        if let Some(agent) = agent.filter(|a| a.type_id == 2) {
            if let Some(system) = sde.map.station(agent.station_id).and_then(|r| r.solar_system_id) { return (agent.id, system); }
        }
    }
    panic!("no basic agent with a mapped station")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn bound_warp_to_location_consumes_sixth_positional_settings() {
    let db = fixture::db().await;
    let sde = crate::tests::shared_sde(&db).await;
    let (agent_id, system) = basic_agent(&sde);
    let station = sde.map.station(crate::services::missions::agent(agent_id).unwrap().station_id).unwrap();
    let origin = [station.x.unwrap_or(0.0) - 1e9, station.y.unwrap_or(0.0), station.z.unwrap_or(0.0)];
    let mut rig = fixture::FleetWarp::new(&db, system, 1, origin).await;
    let result = crate::tests::catch_scenario(async {
        let bind = Value::Int(agent_id);
        let wall = || fixture::settings(3, 1000, 10_000);
        fixture::train(&db, rig.commander.fixture.character, 3, 0).await;
        // WarpToLocation(locationType, number, range, fleet, referringAgentID, formation).
        let args = |referrer: Value, wire: Value| vec![
            Value::str("agenthomebase"), Value::Int(1), Value::Float(0.0),
            Value::Bool(true), referrer, wire,
        ];
        fixture::user_error(fixture::dispatch(&mut rig.client, "agentMgr", "WarpToLocation",
            &args(wall(), Value::Int(5)), &Value::None, Some(&bind)).await, messages::fleets::INVALID_REQUEST);
        rig.assert_no_warp().await;
        fixture::dispatch(&mut rig.client, "agentMgr", "WarpToLocation",
            &args(Value::Int(agent_id), wall()), &Value::None, Some(&bind)).await.unwrap();
        rig.settle().await;
        let (centre, centre_stand) = rig.warped(rig.commander.ship).await.unwrap();
        let (slot, slot_stand) = rig.warped(rig.members[0].ship).await.unwrap();
        assert!(space::dist(slot, space::add(centre, [0.0, 1000.0, 0.0])) < 1e-3);
        assert_eq!((centre_stand, slot_stand), (0.0, 0.0));
    }).await;
    rig.finish().await;
    result.unwrap();
}
