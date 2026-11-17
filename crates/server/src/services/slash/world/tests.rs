use crate::services::slash::tests::support;
use infinity_core::consts::{GM_ROLES, KNOWN_SPACE_SYSTEM_START, WORMHOLE_SYSTEM_END, WORMHOLE_SYSTEM_START};
use infinity_core::messages;
use marshal::Value;

fn reply(result: crate::services::CallResult) -> String {
    match result.unwrap() { Value::Unicode(text) => text, other => panic!("expected unicode reply, got {other:?}") }
}
fn refusal(result: crate::services::CallResult) -> String {
    match result.unwrap_err() {
        crate::services::CallError::User { dict, .. } => dict.iter().find(|(key, _)| key == "reason")
            .and_then(|(_, v)| v.as_text().map(|t| t.into_owned())).expect("slash reason"),
        other => panic!("expected user refusal, got {other:?}"),
    }
}

/// Independent oracle: the shared client ranges, without the map predicate.
fn in_public_range(id: i64) -> bool {
    (KNOWN_SPACE_SYSTEM_START..=(WORMHOLE_SYSTEM_START - 1)).contains(&id)
        || (WORMHOLE_SYSTEM_START..=WORMHOLE_SYSTEM_END).contains(&id)
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn loadsys_route_supports_station_and_reuses_live_systems() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let result = crate::tests::catch_scenario(async {
        let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
        client.login.role = GM_ROLES;
        let (station, system) = client.sde.map.stations()
            .filter_map(|s| s.solar_system_id.map(|id| (s.station_id, id)))
            .find(|&(_, id)| client.sde.map.is_public_system(id) && !client.sde.map.jumps(id).is_empty())
            .expect("public station system with a gate");
        client.session.apply(vec![("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system))]);
        let mut expected: std::collections::BTreeSet<i64> = [system].into_iter().collect();
        expected.extend(client.sde.map.celestials(system).filter_map(|c| client.sde.map.stargate(c.item_id))
            .filter(|g| g.solar_system_id == Some(system)).filter_map(|g| g.dest_solar_system_id)
            .filter(|&d| client.sde.map.is_public_system(d)));
        let n = expected.len() as i64;
        assert_eq!(reply(support::rpc(&mut client, "/loadsys").await), messages::gm::gm_system_preload(n, 0, n, 0, 0));
        let mut balls = std::collections::BTreeMap::new();
        for &id in &expected {
            balls.insert(id, client.world.with(id, |s| s.balls().count()).await.expect("loaded system"));
        }
        assert_eq!(reply(support::rpc(&mut client, "/loadsys").await), messages::gm::gm_system_preload(n, n, 0, 0, 0));
        for (&id, &count) in &balls {
            assert_eq!(client.world.with(id, |s| s.balls().count()).await, Some(count));
        }
        assert!(!crate::config::game().gm.performance_testing);
        let mut before = std::collections::BTreeSet::new();
        for id in client.sde.map.public_systems().map(|s| s.solar_system_id) {
            if client.world.with(id, |_| ()).await.is_some() { before.insert(id); }
        }
        for command in ["/loadallsys", "/unloadallsys"] {
            assert_eq!(refusal(support::rpc(&mut client, command).await), messages::gm::GM_PERFORMANCE_TESTING_REQUIRED);
        }
        for id in client.sde.map.public_systems().map(|s| s.solar_system_id) {
            assert_eq!(client.world.with(id, |_| ()).await.is_some(), before.contains(&id));
        }
        assert_eq!(refusal(support::rpc(&mut client, "/loadallsys now").await), messages::gm::GM_LOADALLSYS_USAGE);
        assert_eq!(refusal(support::rpc(&mut client, "/unloadallsys now").await), messages::gm::GM_UNLOADALLSYS_USAGE);
        assert_eq!(refusal(support::rpc(&mut client, "/loadsys now").await), messages::gm::GM_LOADSYS_USAGE);
        // A static row outside the public ranges is not a usable position.
        let outside = client.sde.map.systems().map(|s| s.solar_system_id)
            .find(|&id| !in_public_range(id)).expect("static row outside public ranges");
        client.session.apply(vec![("solarsystemid2", Value::Int(outside))]);
        assert_eq!(refusal(support::rpc(&mut client, "/loadsys").await), messages::gm::GM_PRELOAD_NO_PUBLIC_SYSTEM);
        // Docked without the canonical ID, and a no-character session, refuse too.
        client.session.apply(vec![("solarsystemid2", Value::None)]);
        assert_eq!(refusal(support::rpc(&mut client, "/loadsys").await), messages::gm::GM_PRELOAD_NO_PUBLIC_SYSTEM);
        client.session.apply(vec![("solarsystemid2", Value::Int(system)), ("charid", Value::None)]);
        assert_eq!(refusal(support::rpc(&mut client, "/loadsys").await), messages::gm::GM_ARGS_NO_CHARACTER);
        client.session.apply(vec![("charid", Value::Int(-1))]);
        assert_eq!(refusal(support::rpc(&mut client, "/loadsys").await), messages::gm::GM_ARGS_NO_CHARACTER);
        // Without a GM role the command loads nothing.
        client.login.role = 0;
        let other = client.sde.map.public_systems().map(|s| s.solar_system_id)
            .find(|id| !expected.contains(id)).expect("other public system");
        client.session.apply(vec![("charid", Value::Int(fixture.character)), ("stationid", Value::None), ("solarsystemid", Value::Int(other))]);
        assert_eq!(refusal(support::rpc(&mut client, "/loadsys").await), messages::gm::GM_ROLE_REQUIRED);
        assert_eq!(refusal(support::rpc(&mut client, "/unloadallsys").await), messages::gm::GM_ROLE_REQUIRED);
        assert!(client.world.with(other, |_| ()).await.is_none());
    }).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
