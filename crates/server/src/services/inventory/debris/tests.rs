//! GM debris cleanup: the real slash route, sampled-root selection, persisted
//! whole-tree destruction and the whole-batch protected refusal.

use super::*;
use crate::client::Client;
use crate::db::Db;
use crate::services::combat::groups_health::spawn_wreck;
use crate::services::slash::tests::support::{register_online, rpc, wire};
use crate::services::tests::{client, fixture::{self, Fixture}};
use crate::space::{Ball, WarpState, MODE_STOP, V3};
use infinity_core::consts::{CATEGORY_CHARGE, FLAG_CARGO, FLAG_FITTING_FIRST, GM_ROLES, GROUP_WRECK};
use infinity_core::session::OnlineSession;
use marshal::Value;

fn ball(id: i64, type_id: i32, owner: i64, character: Option<i64>, pos: V3, vel: V3, free: bool) -> Ball {
    Ball { id, type_id, owner_id: owner, corp_id: 1, alliance_id: -1, char_id: character,
        signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1e30, max_vel: 0.0, agility: 1.0,
        speed_fraction: 0.0, pos, vel, old_pos: pos, old_vel: vel, segment_acceleration: None, goto: [0.0; 3],
        follow_id: 0, follow_range: 0.0, free, global: false, massive: true, interactive: false,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

async fn space_fixture() -> (Db, Arc<Sde>, Client, tokio::net::TcpStream, Fixture, i64, i64, i32) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let f = fixture::create(&db).await.unwrap();
    let (mut c, peer) = client(db.clone(), f.character).await;
    c.login.role = GM_ROLES;
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let ship = crate::dao::items::creation::positioned(&db, ship_type, f.character, system, [0.0; 3]).await.unwrap();
    c.world.add_ship(system, ball(ship, ship_type, f.character, Some(f.character), [0.0; 3], [0.0; 3], false)).await.unwrap();
    c.session.apply(vec![("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
    (db, sde, c, peer, f, system, ship, ship_type)
}

async fn alive(db: &Db, ids: Vec<i64>) -> Vec<i64> {
    db.query("SELECT item_id FROM game.items WHERE item_id = ANY($1) ORDER BY item_id", &[&ids])
        .await.unwrap().iter().map(|row| row.get("item_id")).collect()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn slash_cleanup_selects_sampled_debris_and_persists_whole_trees() {
    let (db, sde, mut c, _peer, f, system, ship, ship_type) = space_fixture().await;
    let wreck_type = sde.types.iter().find(|t| t.group_id == Some(GROUP_WRECK)
        && !sde.typelists.is_untrashable(t.type_id, GROUP_WRECK, t.category_id.unwrap_or_default())).unwrap().type_id;
    let container_type = sde.types.iter().find(|t| crate::services::inventory::hold_storage::is_ordinary_container(&sde, t.type_id)
        && !sde.typelists.is_untrashable(t.type_id, t.group_id.unwrap_or_default(), t.category_id.unwrap_or_default())).unwrap().type_id;
    let loot_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)).unwrap().type_id;
    let foreign = fixture::create(&db).await.unwrap();
    let near = spawn_wreck(&db, &c.world, system, wreck_type, f.character, 1, [1_000.0, 0.0, 0.0], None, None).await.unwrap();
    let nested = crate::dao::items::creation::rookie_ship(&db, container_type, f.character, near, 0).await.unwrap();
    let loot_a = crate::dao::items::transfers::insert(&db, loot_type, f.character, near, 0, 1).await.unwrap();
    let loot_b = crate::dao::items::transfers::insert(&db, loot_type, foreign.character, nested, 0, 1).await.unwrap();
    let edge = spawn_wreck(&db, &c.world, system, container_type, foreign.character, 1, [20_000.0, 0.0, 0.0], None, None).await.unwrap();
    let outside = spawn_wreck(&db, &c.world, system, wreck_type, f.character, 1, [20_001.0, 0.0, 0.0], None, None).await.unwrap();
    let far = spawn_wreck(&db, &c.world, system, wreck_type, f.character, 1, [1e6, 0.0, 0.0], None, None).await.unwrap();
    let sentinel = crate::dao::items::creation::positioned(&db, ship_type, f.character, system, [2_000.0, 0.0, 0.0]).await.unwrap();
    c.world.add_ship(system, ball(sentinel, ship_type, f.character, None, [2_000.0, 0.0, 0.0], [0.0; 3], false)).await.unwrap();
    let other_system = sde.map.systems().nth(1).unwrap().solar_system_id;
    let other = spawn_wreck(&db, &c.world, other_system, wreck_type, f.character, 1, [0.0; 3], None, None).await.unwrap();
    let mut foreign_rx = register_online(&c.world, OnlineSession { character_id: foreign.character, user_id: Some(foreign.account),
        session_id: 2, ship_id: None, station_id: None, system_id: Some(system), corporation_id: None, alliance_id: None,
        faction_id: None, fleet_id: None, role: 0, corporation_role: 0 });
    c.login.role = 0;
    assert!(rpc(&mut c, "/testclear").await.is_err(), "non-GM must be refused");
    c.login.role = GM_ROLES;
    assert!(rpc(&mut c, "/testclear extra").await.is_err(), "extra arguments must be refused");
    assert_eq!(wire(rpc(&mut c, "/testclear").await), wire(Ok(Value::unicode(messages::gm::gm_debris_cleared(2, 5)))));
    assert_eq!(foreign_rx.try_recv().unwrap().0, "OnItemChange", "the foreign owner must keep its removal notification");
    assert_eq!(alive(&db, vec![near, edge, outside, far, sentinel, other, nested, loot_a, loot_b]).await, {
        let mut kept = vec![outside, far, sentinel, other]; kept.sort_unstable(); kept });
    assert!(c.world.ball(system, near).await.is_none() && c.world.ball(system, edge).await.is_none());
    assert!(c.world.ball(system, outside).await.is_some() && c.world.ball(system, far).await.is_some() && c.world.ball(system, sentinel).await.is_some());
    let mut s = System::default();
    s.insert(ball(1, wreck_type, 1, None, [0.0; 3], [1e7, 0.0, 0.0], true));
    s.tick(1_500);
    let (pos, sampled) = (s.ball(1).unwrap().pos, s.sample_position(1).unwrap());
    assert!(crate::space::dist(pos, sampled) > NEARBY_CLEAR_RADIUS_M, "tick position must diverge from the sampled one");
    let (mut static_ball, celestial) = (ball(2, wreck_type, 1, None, sampled, [0.0; 3], false), sde.map.all_celestials().next().unwrap().item_id);
    static_ball.global = true;
    s.insert(static_ball);
    s.insert(ball(celestial, wreck_type, 1, None, sampled, [0.0; 3], false));
    s.insert(ball(3, ship_type, f.character, Some(f.character), sampled, [0.0; 3], false));
    let identity = Identity { char_id: f.character, ship_id: Some(3), corp_id: 1, station_id: None, system_id: Some(system) };
    assert_eq!(select_roots(&s, &sde, &identity, CleanupScope::Nearby).unwrap().iter().map(|root| root.item_id).collect::<Vec<_>>(), vec![1]);
    assert_eq!(wire(rpc(&mut c, "/testclear").await), wire(Ok(Value::unicode(messages::gm::gm_debris_cleared(0, 0)))));
    c.world.remove_ball(system, ship).await;
    c.session.apply(vec![("stationid", Value::Int(sde.map.stations().next().unwrap().station_id)), ("solarsystemid", Value::None)]);
    assert!(rpc(&mut c, "/testclear").await.is_err(), "a docked caller cannot clear nearby debris");
    assert!(c.world.ball(system, outside).await.is_some() && c.world.ball(system, far).await.is_some());
    assert_eq!(wire(rpc(&mut c, "/sysjunkclear").await), wire(Ok(Value::unicode(messages::gm::gm_debris_cleared(2, 2)))));
    assert!(c.world.ball(system, sentinel).await.is_some() && c.world.ball(other_system, other).await.is_some());
    let cold = sde.map.systems().nth(2).unwrap().solar_system_id;
    c.session.apply(vec![("solarsystemid2", Value::Int(cold))]);
    assert_eq!(wire(rpc(&mut c, "/sysjunkclear").await), wire(Ok(Value::unicode(messages::gm::gm_debris_cleared(0, 0)))));
    assert!(c.world.with(cold, |_| ()).await.is_none(), "an absent actor must not be loaded");
    c.world.remove_ball(system, sentinel).await;
    c.world.remove_ball(other_system, other).await;
    fixture::cleanup(&db, f).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn cleanup_rejects_the_entire_batch_for_a_reserved_subtree() {
    let (db, sde, mut c, _peer, f, system, _ship, _ship_type) = space_fixture().await;
    let wreck_type = sde.types.iter().find(|t| t.group_id == Some(GROUP_WRECK)
        && !sde.typelists.is_untrashable(t.type_id, GROUP_WRECK, t.category_id.unwrap_or_default())).unwrap().type_id;
    let pick = |category: i32| sde.types.iter().find(|t| t.category_id == Some(category)
        && !sde.typelists.is_untrashable(t.type_id, t.group_id.unwrap_or_default(), category)).unwrap().type_id;
    let a = spawn_wreck(&db, &c.world, system, wreck_type, f.character, 1, [1_000.0, 0.0, 0.0], None, None).await.unwrap();
    let b = spawn_wreck(&db, &c.world, system, wreck_type, f.character, 1, [1_000.0, 0.0, 0.0], None, None).await.unwrap();
    let hull = crate::dao::items::creation::rookie_ship(&db, pick(CATEGORY_SHIP), f.character, b, 0).await.unwrap();
    let module = crate::dao::items::creation::rookie_ship(&db, pick(CATEGORY_MODULE), f.character, hull, FLAG_FITTING_FIRST).await.unwrap();
    let reload = crate::dao::items::pending_reloads::insert(&db, f.character, hull, module, FLAG_FITTING_FIRST, pick(CATEGORY_CHARGE), 1, FLAG_CARGO, hull).await.unwrap();
    let mut rx = register_online(&c.world, OnlineSession::from_session(&c.session).unwrap());
    let Err(CallError::User { msg, dict }) = rpc(&mut c, "/testclear").await else { panic!("a reserved subtree must refuse the batch") };
    assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
    assert!(dict.iter().any(|(key, value)| key == "notify" && matches!(value, Value::Unicode(t) if t.as_str() == messages::gm::GM_DEBRIS_REFUSED)));
    assert_eq!(alive(&db, vec![a, b, hull, module]).await.len(), 4, "a refused batch must leave every row");
    assert!(c.world.ball(system, a).await.is_some() && c.world.ball(system, b).await.is_some());
    assert!(rx.try_recv().is_err(), "no removal notification may be sent for a refused batch");
    crate::dao::items::pending_reloads::take(&db, reload).await.unwrap();
    c.world.remove_ball(system, a).await;
    c.world.remove_ball(system, b).await;
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&vec![a, b, hull, module]]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}
