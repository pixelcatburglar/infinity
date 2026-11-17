use super::*;
use std::sync::Arc;
use crate::{services::{ship_cosmetics_mgr::state, CallError}, space::{NpcBrain, NpcConfig, WarpState, MODE_STOP}};

fn ball(id: i64, type_id: i32, character: Option<i64>) -> Ball {
    Ball { id, type_id, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: character,
        signature_radius: Some(1.0), mode: MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
        follow_id: 0, follow_range: 0.0, free: true, global: false, massive: true,
        interactive: character.is_some(), warp: WarpState::default(), 
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE"]
async fn actor_visibility_and_npc_projection() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let entity = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ENTITY)).unwrap().type_id;
    let mut system = System::new(1, Arc::default(), 1000);
    system.projection_sde = Some(sde.clone());
    system.insert(ball(1, ship, Some(1)));
    system.insert(ball(2, ship, Some(2)));
    system.insert(ball(4, entity, None));
    system.insert(ball(i64::MAX, entity, None));
    assert!(fact(&system, system.ball(i64::MAX).unwrap()).is_none());
    let config = NpcConfig { fly_range: 1.0, aggro_range: 1.0, rate_of_fire_ms: 1.0, scan_resolution: 1.0, repairs: (0.0, 0.0, 0.0) };
    system.add_npc(i64::MAX, NpcBrain::new(None, config, Arc::new(|_, _| Some(Box::pin(async {})))));
    assert!(fact(&system, system.ball(i64::MAX).unwrap()).is_some());
    assert!(fact(&system, system.ball(4).unwrap()).is_none());
    let npc = fact(&system, system.ball(i64::MAX).unwrap()).unwrap();
    let states = state::prepare(&db, &sde, &[npc], filetime_now()).await.unwrap();
    assert_eq!(states[0].state.skin_id, None);
    assert_eq!(states[0].state.character_id, 0);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (completed, mut done) = tokio::sync::oneshot::channel();
    assert!(system.observe(1, 1, tx, completed));
    system.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(done.try_recv(), Ok(true));
    assert!(system.fleet_broadcast_visible(1, 2));
    assert!(!system.fleet_broadcast_visible(2, 1));
    let mut cloaked = ball(5, ship, Some(5)); cloaked.cloak = 1;
    system.insert(cloaked);
    assert!(!system.fleet_broadcast_visible(1, 5));
    assert!(!system.fleet_broadcast_visible(5, 5));
}

fn child(message: &prost_reflect::DynamicMessage, field: &str) -> prost_reflect::DynamicMessage {
    message.get_field_by_name(field).unwrap().as_message().unwrap().clone()
}

fn assert_state(state: &prost_reflect::DynamicMessage, ship: i64, character: i64, skin: Option<i32>) {
    assert_eq!(child(state, "ship").get_field_by_name("sequential").unwrap().as_u64(), Some(ship as u64));
    assert_eq!(child(state, "character").get_field_by_name("sequential").unwrap().as_u32(), Some(character as u32));
    let value = child(state, "skin");
    match skin {
        Some(skin) => {
            let id = child(&child(&value, "firstparty"), "identifier");
            assert_eq!(id.get_field_by_name("sequential").unwrap().as_u64(), Some(skin as u64));
            assert!(!value.has_field_by_name("no_skin"));
        }
        None => assert_eq!(value.get_field_by_name("no_skin").unwrap().as_bool(), Some(true)),
    }
}

fn snapshot_state(message: &prost_reflect::DynamicMessage, ship: i64) -> Option<prost_reflect::DynamicMessage> {
    message.get_field_by_name("state").unwrap().as_list().unwrap().iter()
        .filter_map(|value| value.as_message())
        .find(|state| child(state, "ship").get_field_by_name("sequential").unwrap().as_u64() == Some(ship as u64))
        .cloned()
}

const ALL: &str = "eve_public.cosmetic.ship.api.SetAllInBubbleNotice";
const SET: &str = "eve_public.cosmetic.ship.api.SetNotice";

async fn frame(
    rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>, character: i64,
    name: &str, ship: Option<i64>, cloaked: Option<i64>,
) -> prost_reflect::DynamicMessage {
    use prost_reflect::ReflectMessage;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let bytes = rx.recv().await.expect("notice stream closed");
            let (notice, message) = crate::gateway::tests::decode_notice(&bytes);
            assert_eq!(notice.target_group.unwrap().target, Some(crate::gateway::protocol::target_group::Target::Character(u32::try_from(character).unwrap())));
            let actual = message.descriptor().full_name().to_owned();
            match actual.as_str() {
                SET => {
                    let state = child(&message, "state");
                    let id = child(&state, "ship").get_field_by_name("sequential").unwrap().as_u64().unwrap();
                    if let Some(cloaked) = cloaked {
                        assert_ne!(id, cloaked as u64, "cloaked ship leaked in incremental notice");
                    }
                    if name == SET && Some(id) == ship.map(|id| id as u64) {
                        return message;
                    }
                }
                ALL if name == ALL => {
                    if let Some(cloaked) = cloaked {
                        assert!(snapshot_state(&message, cloaked).is_none(), "cloaked ship leaked in snapshot");
                    }
                    return message;
                }
                _ => panic!("unexpected notice type: {actual}"),
            }
        }
    }).await.expect("notice timeout")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn deferred_actor_snapshots_uncloak_and_npc_registration() {
    use crate::services::tests::fixture;
    use infinity_core::session::OnlineSession;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let owner = fixture::create(&db).await.unwrap();
    let observer = fixture::create(&db).await.unwrap();
    let late = fixture::create(&db).await.unwrap();
    let (skin, hull) = sde.ship_skins.licenses().find_map(|license| {
        let skin = sde.ship_skins.skin(license.skin_id)?;
        if !license.is_permanent() || license.is_single_use || skin.is_structure_skin { return None; }
        let hull = *skin.type_ids.iter().find(|&&id| sde.types.get(id).is_some_and(|ty| ty.category_id == Some(CATEGORY_SHIP)))?;
        Some((skin.skin_id, hull))
    }).expect("permanent personal SKIN and ship hull in SDE");
    let entity = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ENTITY)).unwrap().type_id;
    let drone = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_DRONE)).unwrap().type_id;
    let system = sde.map.stations().find_map(|station| station.solar_system_id).unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    world.system(system).await.unwrap();
    let ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &owner.character, &system, &infinity_core::consts::FLAG_HANGAR]).await.unwrap().get("item_id");
    assert!(crate::dao::ship_skins::insert_license(&db, owner.character, skin, None).await.unwrap());
    db.execute("INSERT INTO game.ship_skin_applications (ship_item_id,character_id,skin_id) VALUES ($1,$2,$3)", &[&ship, &owner.character, &skin]).await.unwrap();
    assert_eq!(crate::dao::ship_skins::states_for_ships(&db, &[ship], filetime_now()).await.unwrap()[0].skin_id, Some(skin));
    let mut notify_receivers = Vec::new();
    for (who, ego) in [(owner, ship), (observer, observer.character), (late, late.character)] {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        notify_receivers.push(rx);
        world.register_pilot(who.character, tx);
        world.set_online_session(who.character, Some(OnlineSession {
            character_id: who.character, user_id: Some(who.account), session_id: 1,
            ship_id: Some(ego), station_id: None, system_id: Some(system), corporation_id: None,
            alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
        }));
    }
    let mut owner_stream = crate::gateway::tests::notice_stream(&world, [1; 16], owner.character, 32);
    let mut observer_stream = crate::gateway::tests::notice_stream(&world, [2; 16], observer.character, 32);
    let mut late_stream = crate::gateway::tests::notice_stream(&world, [3; 16], late.character, 32);
    let mut cloaked = ball(ship, hull, Some(owner.character));
    cloaked.cloak = 1;
    world.add_ship(system, cloaked).await.unwrap();
    world.add_ship(system, ball(observer.character, hull, Some(observer.character))).await.unwrap();
    world.add_ship(system, ball(late.character, hull, Some(late.character))).await.unwrap();
    let (owner_tx, _owner_destiny) = tokio::sync::mpsc::unbounded_channel();
    let (observer_tx, _observer_destiny) = tokio::sync::mpsc::unbounded_channel();
    let (late_tx, _late_destiny) = tokio::sync::mpsc::unbounded_channel();
    world.observe(system, owner.character, ship, owner_tx).await.unwrap();
    let own = frame(&mut owner_stream.rx, owner.character, ALL, None, None).await;
    assert_state(&snapshot_state(&own, ship).unwrap(), ship, owner.character, Some(skin));
    world.observe(system, observer.character, observer.character, observer_tx).await.unwrap();
    let first = frame(&mut observer_stream.rx, observer.character, ALL, None, Some(ship)).await;
    assert!(snapshot_state(&first, ship).is_none());
    world.with(system, move |s| {
        let stamp = s.current_time();
        s.apply_uncloak_with_collisions(ship, stamp, false);
    }).await.unwrap();
    let restored = frame(&mut observer_stream.rx, observer.character, SET, Some(ship), None).await;
    assert_state(&child(&restored, "state"), ship, owner.character, Some(skin));
    world.observe(system, late.character, late.character, late_tx).await.unwrap();
    let snapshot = frame(&mut late_stream.rx, late.character, ALL, None, None).await;
    assert_state(&snapshot_state(&snapshot, ship).unwrap(), ship, owner.character, Some(skin));
    // A real bubble out/back re-presents the stored SKIN to the returner
    // through the reveal path exactly once.
    world.with(system, move |s| {
        let home = s.ball(observer.character).unwrap().pos;
        assert!(s.scene_has(s.observer_token(observer.character).unwrap(), s.ball_token(ship).unwrap()));
        s.set_ball_position(observer.character, [1e12, 0.0, 0.0], |_, _| true);
        s.apply_scene_prefix(crate::space::collision::enabled());
        assert!(!s.scene_has(s.observer_token(observer.character).unwrap(), s.ball_token(ship).unwrap()));
        s.set_ball_position(observer.character, home, |_, _| true);
        s.apply_scene_prefix(crate::space::collision::enabled());
        assert!(s.scene_has(s.observer_token(observer.character).unwrap(), s.ball_token(ship).unwrap()));
    }).await.unwrap();
    let npc = i64::MAX;
    let unrelated = i64::MAX - 1;
    let drone_id = i64::MAX - 2;
    world.add_npc_ship(system, ball(npc, entity, None)).await.unwrap();
    world.add_ship(system, ball(unrelated, entity, None)).await.unwrap();
    world.add_ship(system, ball(drone_id, drone, None)).await.unwrap();
    let config = NpcConfig { fly_range: 1.0, aggro_range: 1.0, rate_of_fire_ms: 1.0, scan_resolution: 1.0, repairs: (0.0, 0.0, 0.0) };
    assert!(world.with(system, move |s| s.add_npc(npc, NpcBrain::new(None, config, Arc::new(|_, _| Some(Box::pin(async {})))))).await.unwrap());
    // The ordered stream itself bounds the fence at the npc registration SET:
    // every notice before it is judged, and the reveal's ship SET is counted.
    let mut ship_notice = None;
    let mut ship_sets = 0;
    let npc_notice = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        use prost_reflect::ReflectMessage;
        loop {
            let bytes = observer_stream.rx.recv().await.expect("notice stream closed");
            let (notice, message) = crate::gateway::tests::decode_notice(&bytes);
            assert_eq!(notice.target_group.unwrap().target,
                Some(crate::gateway::protocol::target_group::Target::Character(u32::try_from(observer.character).unwrap())));
            let actual = message.descriptor().full_name().to_owned();
            assert_eq!(actual, SET, "unexpected notice type: {actual}");
            let state = child(&message, "state");
            let id = child(&state, "ship").get_field_by_name("sequential").unwrap().as_u64().unwrap();
            if id == ship as u64 { ship_sets += 1; ship_notice = Some(message); continue; }
            if id == npc as u64 { break message; }
        }
    }).await.expect("notice timeout");
    assert_eq!(ship_sets, 1, "the reveal presents the ship's SKIN exactly once");
    assert_state(&child(&ship_notice.unwrap(), "state"), ship, owner.character, Some(skin));
    assert_state(&child(&npc_notice, "state"), npc, 0, None);
    let owner_npc = frame(&mut owner_stream.rx, owner.character, SET, Some(npc), None).await;
    assert_state(&child(&owner_npc, "state"), npc, 0, None);
    let late_npc = frame(&mut late_stream.rx, late.character, SET, Some(npc), None).await;
    assert_state(&child(&late_npc, "state"), npc, 0, None);
    let fence = crate::space::ship_cosmetics::capture(&world, system, ship).await.unwrap();
    db.execute("DELETE FROM game.ship_skin_applications WHERE ship_item_id=$1", &[&ship]).await.unwrap();
    let prepared = state::prepare(&db, &sde, &[fence.fact().clone()], filetime_now()).await.unwrap().into_iter().next().unwrap();
    let mut outbox = crate::db::outbox::Outbox::default();
    outbox.ship_cosmetics(fence.committed(prepared));
    crate::db::outbox::dispatch_world((), outbox, &world).unwrap();
    let (again_tx, _again_destiny) = tokio::sync::mpsc::unbounded_channel();
    world.observe(system, late.character, late.character, again_tx).await.unwrap();
    let current = frame(&mut late_stream.rx, late.character, ALL, None, None).await;
    assert_state(&snapshot_state(&current, ship).unwrap(), ship, owner.character, None);
    assert!(snapshot_state(&current, unrelated).is_none());
    assert!(snapshot_state(&current, drone_id).is_none());
    assert_state(&snapshot_state(&current, npc).unwrap(), npc, 0, None);
    assert_eq!(crate::dao::ship_skins::states_for_ships(&db, &[ship], filetime_now()).await.unwrap()[0].skin_id, None);
    let collision: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &owner.character, &system, &infinity_core::consts::FLAG_HANGAR]).await.unwrap().get("item_id");
    db.execute("INSERT INTO game.ship_skin_applications (ship_item_id,character_id,skin_id) VALUES ($1,$2,$3)", &[&collision, &owner.character, &skin]).await.unwrap();
    assert_eq!(crate::dao::ship_skins::states_for_ships(&db, &[collision], filetime_now()).await.unwrap()[0].skin_id, Some(skin));
    let guard = crate::services::market::lock().await;
    world.add_npc_ship(system, ball(collision, hull, None)).await.unwrap();
    let before = world.with(system, move |s| (s.is_npc(collision), s.ball(collision).map(|b| fact(s, b).is_some_and(|f| f.is_npc)), s.skins.entries.get(&collision).map(|e| (e.token, e.version, e.fact.clone(), e.state.clone())))).await.unwrap();
    let (token, version, fact0, state0) = before.2.unwrap();
    assert_eq!((before.0, before.1, &state0), (false, Some(true), &state::State { ship_id: collision, character_id: 0, skin_id: None }));
    let collision_notice = frame(&mut observer_stream.rx, observer.character, SET, Some(collision), None).await;
    assert_state(&child(&collision_notice, "state"), collision, 0, None);
    let bad = PreparedSkin { fact: fact0.clone(), state: state::State { ship_id: collision, character_id: 0, skin_id: Some(skin) }, expires_at: None };
    assert!(!world.with(system, move |s| s.install_skin(token, bad)).await.unwrap());
    assert!(world.with(system, move |s| s.add_npc(collision, NpcBrain::new(None, NpcConfig { fly_range: 1.0, aggro_range: 1.0, rate_of_fire_ms: 1.0, scan_resolution: 1.0, repairs: (0.0, 0.0, 0.0) }, Arc::new(|_, _| Some(Box::pin(async {})))))).await.unwrap());
    world.with(system, move |s| { let e = &s.skins.entries[&collision]; assert!(s.is_npc(collision) && e.token == token && e.version == version && e.fact == fact0 && e.state == state0); }).await.unwrap();
    assert_eq!(crate::dao::ship_skins::states_for_ships(&db, &[collision], filetime_now()).await.unwrap()[0].skin_id, Some(skin));
    drop(guard);
    // expires_at == now stays valid; the next tick reads no_skin, and only the
    // matching version and deadline fire an expiry. Live fences apply; dead,
    // foreign, wrong-fact and incarnation-stale identities are refused.
    world.add_ship(system, ball(9, hull, Some(9))).await.unwrap();
    world.with(system, move |s| {
        let token = s.capture_ball(9).unwrap();
        assert!(s.install_skin(token, prepared_projection(9, hull, 9, Some(7), Some(filetime_now()))));
        let (version, deadline) = (s.skins.entries[&9].version, s.skins.entries[&9].expires_at.unwrap());
        assert_eq!(s.skins.entries[&9].public_state(deadline).skin_id, Some(7));
        assert_eq!(s.skins.entries[&9].public_state(deadline + 1).skin_id, None);
        let later = deadline + 2 * infinity_core::consts::FILETIME_SECOND;
        let token = s.capture_ball(9).unwrap();
        assert!(s.install_skin(token, prepared_projection(9, hull, 9, Some(7), Some(later))));
        let current = s.skins.entries[&9].version;
        s.expire_skin(9, version, deadline); s.expire_skin(9, current, deadline);
        assert_eq!((s.skins.entries[&9].expires_at, s.skins.entries[&9].state.skin_id), (Some(later), Some(7)));
        s.expire_skin(9, current, later);
        assert_eq!((s.skins.entries[&9].expires_at, s.skins.entries[&9].state.skin_id), (None, None));
        let gate = s.snapshot_context.as_ref().unwrap().gate.clone();
        let token = s.capture_ball(9).unwrap();
        let fence = SkinFence { system, actor_gate: std::sync::Arc::downgrade(&gate), token, fact: fact(s, s.ball(9).unwrap()).unwrap() };
        assert!(s.apply_skin(fence.committed(prepared_projection(9, hull, 9, Some(7), None))));
        let other = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        let dead = SkinFence { actor_gate: std::sync::Weak::new(), ..fence.clone() };
        let foreign = SkinFence { actor_gate: std::sync::Arc::downgrade(&other), ..fence.clone() };
        assert!(!s.apply_skin(dead.committed(prepared_projection(9, hull, 9, Some(9), None))));
        assert!(!s.apply_skin(foreign.committed(prepared_projection(9, hull, 9, Some(9), None))));
        let wrong = PreparedSkin { fact: Ship { ship_id: 9, type_id: hull, character_id: Some(10), is_npc: false }, ..prepared_projection(9, hull, 9, Some(9), None) };
        assert!(!s.apply_skin(fence.committed(wrong)));
        s.take(9); s.insert(ball(9, hull, Some(9)));
        assert!(!s.apply_skin(fence.committed(prepared_projection(9, hull, 9, Some(9), None))));
        // A superseded observer ticket can never receive its epoch.
        let (first, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (completed, mut done) = tokio::sync::oneshot::channel();
        assert!(s.observe(1, 9, first, completed));
        s.apply_scene_prefix(crate::space::collision::enabled());
        assert_eq!(done.try_recv(), Ok(true));
        let old = s.observer_token(1).unwrap();
        let (next, _rx2) = tokio::sync::mpsc::unbounded_channel();
        let (completed, mut done) = tokio::sync::oneshot::channel();
        assert!(s.observe(1, 9, next, completed));
        s.apply_scene_prefix(crate::space::collision::enabled());
        assert_eq!(done.try_recv(), Ok(true));
        assert!(s.observer_character(old).is_none() && !s.flush_observer_pending(old));
    }).await.unwrap();
    // A rolled-back transaction never publishes its committed projection.
    let fence = crate::space::ship_cosmetics::capture(&world, system, 9).await.unwrap();
    let result: Result<(), CallError> = db.transaction(|_tx, outbox: &mut crate::db::outbox::Outbox| {
        let change = fence.committed(prepared_projection(9, hull, 9, Some(9), None));
        Box::pin(async move { outbox.ship_cosmetics(change); Err(CallError::Internal(anyhow::anyhow!("forced rollback"))) })
    }, |value, outbox| { let world = &world; async move { crate::db::outbox::dispatch_world(value, outbox, world) } }).await;
    assert!(result.is_err() && world.with(system, move |s| s.skins.entries.get(&9).is_none()).await.unwrap());
    world.remove_ball(system, collision).await;
    for id in [ship, observer.character, late.character, npc, unrelated, drone_id] { world.remove_ball(system, id).await; }
    for who in [owner, observer, late] { world.unobserve(who.character); world.unregister_pilot_generation(who.character, None); }
    drop((owner_stream, observer_stream, late_stream, notify_receivers, world));
    for who in [owner, observer, late] { fixture::cleanup(&db, who).await.unwrap(); }
}

fn prepared_projection(ship: i64, hull: i32, owner: i64, skin: Option<i32>, expires_at: Option<i64>) -> PreparedSkin {
    PreparedSkin { fact: Ship { ship_id: ship, type_id: hull, character_id: Some(owner), is_npc: false },
        state: state::State { ship_id: ship, character_id: owner, skin_id: skin }, expires_at }
}

