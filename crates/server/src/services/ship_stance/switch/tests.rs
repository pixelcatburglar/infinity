//! Stance switching: pure deadline admission/duration bounds and the in-space live-pilot authority fence.
use super::*;

#[test]
fn ready_admits_at_the_deadline() {
    assert!(!ready(99, 100) && ready(100, 100) && ready(101, 100));
}

#[test]
fn deadline_converts_and_bounds_durations() {
    for ms in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, ms_from_filetime_ticks(i64::MAX) as f64 + 1.0] {
        assert!(deadline(0, ms).is_none());
    }
    assert!(deadline(i64::MAX, 1.0).is_none());
    assert_eq!(deadline(7, 10_000.0), Some(7 + 100_000_000));
}

async fn persisted(db: &Db, ship: i64) -> (Vec<i32>, Option<(Option<i64>, i64, i64)>) {
    let modes = db.query("SELECT type_id FROM game.items WHERE location_id=$1 AND flag_id=$2 ORDER BY item_id", &[&ship, &infinity_core::consts::FLAG_HIDDEN_MODIFIERS]).await.unwrap().iter().map(|r| r.get("type_id")).collect();
    (modes, db.query_opt("SELECT previous_stance_id,switched_at,ready_at FROM game.ship_stances WHERE ship_id=$1", &[&ship]).await.unwrap().map(|r| (r.get("previous_stance_id"), r.get("switched_at"), r.get("ready_at"))))
}
fn refused(result: CallResult) {
    assert!(matches!(&result, Err(CallError::User { dict, .. }) if dict == &vec![("notify".into(), Value::unicode(messages::ship_stance::INVALID_REQUEST))]), "expected INVALID_REQUEST, got {result:?}");
}

/// A live in-space caller switches; any captured-system, session or pilot-ball mismatch refuses without writes.
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn in_space_switch_requires_a_matching_live_pilot() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (sde, actor) = (crate::tests::shared_sde(&db).await, crate::services::tests::fixture::create(&db).await.unwrap());
    let (char_id, hull) = (actor.character, ship_stances::HULL_STANCES[0].hull_type);
    let (world, system) = (World::new(db.clone(), sde.clone(), 1), sde.map.systems().next().unwrap().solar_system_id);
    let ship = crate::dao::items::creation::positioned(&db, hull, char_id, system, [0.0; 3]).await.unwrap();
    selection::set_ship(&db, char_id, ship).await.unwrap();
    let ship_ball = crate::space::Ball { id: ship, type_id: hull, owner_id: char_id, corp_id: 1, alliance_id: -1, char_id: Some(char_id), signature_radius: None,
        mode: crate::space::MODE_STOP, radius: 1.0, mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: false, global: false, massive: false, interactive: false, warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() };
    world.add_ship(system, ship_ball.clone()).await.unwrap();
    let fit = Fit::load(&db, sde.clone(), char_id, ship).await.ok().expect("pilot fit");
    let condition = crate::services::combat::prepare_condition_with_fit(&db, ship, &fit).await.ok().expect("pilot condition");
    world.with(system, move |s| s.install_condition(ship, condition.into_condition(), fit)).await.expect("system actor");
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(char_id, tx);
    let session = infinity_core::session::OnlineSession { character_id: char_id, user_id: Some(actor.account), session_id: 1, ship_id: Some(ship),
        station_id: None, system_id: Some(system), corporation_id: Some(1), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 };
    world.set_online_session(char_id, Some(session.clone()));
    let pilot = Identity { char_id, ship_id: Some(ship), corp_id: 1, station_id: None, system_id: Some(system) };
    assert_eq!(set_stance(&pilot, &db, &world, &sde, actor.account, ship, 2, |_| Ok(())).await.ok().expect("admitted switch"), Value::None);
    let before = persisted(&db, ship).await;
    assert_eq!(before.0, vec![ship_stances::mode_type(hull, 2).unwrap()]);
    refused(set_stance(&Identity { system_id: Some(system + 1), ..pilot }, &db, &world, &sde, actor.account, ship, 3, |_| Ok(())).await);
    world.set_online_session(char_id, Some(infinity_core::session::OnlineSession { ship_id: Some(ship + 1), ..session.clone() }));
    refused(set_stance(&pilot, &db, &world, &sde, actor.account, ship, 3, |_| Ok(())).await);
    world.set_online_session(char_id, Some(session));
    world.add_ship(system, crate::space::Ball { id: ship + 1, type_id: 0, ..ship_ball }).await.unwrap();
    refused(set_stance(&pilot, &db, &world, &sde, actor.account, ship, 3, |_| Ok(())).await);
    assert_eq!(persisted(&db, ship).await, before);
    for id in [ship, ship + 1] { world.remove_ball(system, id).await; }
    crate::services::combat::forget_ship(ship);
    world.flush_positions().await.unwrap();
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
}
