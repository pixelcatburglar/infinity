//! Direct Board refusals against real game rows, SDE data and system actors.
//! Foreign custody is intentionally unsupported.
use super::*;
use crate::dao::items::{creation, space_ships::FreeShip};
use crate::services::ship::balls;
use crate::space::MODE_WARP;

// The periodic native position save may advance only the warping hull's coordinates.
async fn motion_fingerprint(db: &Db, warp: i64, start: [f64; 3], end: [f64; 3], phase: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut snapshot = fingerprint(db).await;
    let row = db.query_one("SELECT md5((to_jsonb(t) - 'x' - 'y' - 'z')::text) AS hash, x, y, z FROM game.items t WHERE item_id=$1", &[&warp]).await.unwrap();
    let pos = [row.get::<_, f64>("x"), row.get::<_, f64>("y"), row.get::<_, f64>("z")];
    assert_warp_path(pos, start, end, phase);
    let items = snapshot.iter_mut().find(|(table, _)| table == "items").unwrap();
    let item = items.1.iter_mut().find(|(key, _)| key == &warp.to_string()).expect("warp item in fingerprint");
    item.1 = row.get("hash");
    snapshot
}

fn assert_warp_path(pos: [f64; 3], start: [f64; 3], end: [f64; 3], phase: &str) {
    assert!(pos.iter().all(|v| v.is_finite()), "{phase}: non-finite warp position {pos:?}, start {start:?}, end {end:?}");
    // Native integration reconstructs position as (position * friction²) * (1 / friction²);
    // its rounding can place this fixture one representable value beyond an endpoint.
    assert!((start[0].next_down()..=end[0].next_up()).contains(&pos[0]), "{phase}: warp position outside trajectory: pos {pos:?}, start {start:?}, end {end:?}");
    assert_eq!([pos[1], pos[2]], [start[1], start[2]], "{phase}: warp position off trajectory: pos {pos:?}, start {start:?}, end {end:?}");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn rejected_board_requests_leave_database_actor_session_and_notifications_unchanged() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().expect("SDE system").solar_system_id;
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true))
        .filter(|t| balls::radius(&sde, t.type_id).is_ok_and(|r| r > 0.0 && f64::from(r) < super::super::MAX_BOARDING_DISTANCE / 16.0)).map(|t| t.type_id).min().expect("published small ship");
    let skilled = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true))
        .filter(|t| balls::radius(&sde, t.type_id).is_ok_and(|r| r > 0.0 && f64::from(r) < super::super::MAX_BOARDING_DISTANCE / 16.0))
        .filter(|t| ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS).any(|(&skill, level)| {
            let base = |attr| crate::services::fit::base_attribute(Some(t), sde.dogma.attribute(attr), attr);
            base(skill) > 0.0 && base(level) > 0.0
        })).map(|t| t.type_id).min().expect("published ship requiring a skill");
    let nonship_type = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true)).expect("published nonship").type_id;
    let before = fingerprint(&db).await;
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let foreign_db = db.clone();
    let foreign_sde = sde.clone();
    let foreign = tokio::spawn(async move { fixture(&foreign_db, &foreign_sde, system, None).await }).await;
    let (foreign_account, foreign_owner) = match foreign {
        Ok(fixture) => fixture,
        Err(error) => {
            cleanup(&db, account, owner).await;
            std::panic::resume_unwind(error.into_panic());
        }
    };
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [100_000_000.0, 0.0, 0.0];
        let radius = f64::from(balls::radius(&sde, kind).unwrap().max(balls::radius(&sde, skilled).unwrap()));
        let spacing = 2.0 * radius + 100.0;
        let at = |x: f64, y: f64, z: f64| [pos[0] + x * spacing, pos[1] + y * spacing, pos[2] + z * spacing];
        let occupied_pos = at(0.0, 1.0, 0.0);
        let warp_pos = at(1.0, 0.0, 0.0);
        let cloak_pos = at(0.0, -1.0, 0.0);
        let unskilled_pos = at(-1.0, 0.0, 0.0);
        let warp_destination = [200_000_000.0, warp_pos[1], warp_pos[2]];
        let old = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let row = |id, type_id, at| FreeShip { item_id: id, type_id, owner_id: owner, corporation_id: 1, alliance_id: 0, pos: at };
        let pilot = balls::piloted(&test_db, sde.clone(), &row(old, kind, pos), owner, 0.0).await.unwrap();
        world.system(system).await.unwrap();
        world.with(system, move |s| pilot.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let foreign = creation::positioned(&test_db, kind, foreign_owner, system, pos).await.unwrap();
        let nonship = creation::positioned(&test_db, nonship_type, owner, system, pos).await.unwrap();
        let packaged = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        test_db.execute("UPDATE game.items SET singleton=false WHERE item_id=$1", &[&packaged]).await.unwrap();
        let occupied = creation::positioned(&test_db, kind, owner, system, occupied_pos).await.unwrap();
        let warp = creation::positioned(&test_db, kind, owner, system, warp_pos).await.unwrap();
        let cloak = creation::positioned(&test_db, kind, owner, system, cloak_pos).await.unwrap();
        let far_pos = [200_000_000.0; 3];
        let far = creation::positioned(&test_db, kind, owner, system, far_pos).await.unwrap();
        let unskilled = creation::positioned(&test_db, skilled, owner, system, unskilled_pos).await.unwrap();
        for (id, ty, at, state) in [(occupied, kind, occupied_pos, 1), (warp, kind, warp_pos, 2), (cloak, kind, cloak_pos, 3), (far, kind, far_pos, 0), (unskilled, skilled, unskilled_pos, 0)] {
            let mut ship = balls::unpiloted(&test_db, sde.clone(), &row(id, ty, at)).await.unwrap();
            match state {
                1 => ship.ball.interactive = true,
                2 => assert_eq!(ship.ball.warp_to(warp_destination, 0.0, 1), crate::space::WarpOutcome::Align),
                3 => ship.ball.cloak = 1,
                _ => (),
            }
            let market = crate::services::market::lock().await;
            world.add_persistent_ship_locked(system, ship, &market).await.unwrap();
        }
        let positions = [pos, occupied_pos, warp_pos, cloak_pos, unskilled_pos];
        for (index, a) in positions.iter().enumerate() {
            for b in &positions[index + 1..] {
                assert!(crate::space::dist(*a, *b) > 2.0 * radius, "fixture hulls must not overlap");
            }
            if index != 0 {
                assert!(crate::space::dist(pos, *a) <= super::super::MAX_BOARDING_DISTANCE, "fixture target must start in boarding range");
            }
        }
        let mut c = client(test_db.clone(), sde, world.clone(), account).await;
        c.session.apply(vec![("userid", Value::Int(account)), ("charid", Value::Int(owner)), ("shipid", Value::Int(old)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        world.register_pilot(owner, c.notify_tx.clone());
        world.set_online_session(owner, infinity_core::session::OnlineSession::from_session(&c.session));
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        world.observe(system, c.login.client_id, old, events).await.unwrap();
        assert!(world.after(system, 1_100).await);
        while received.try_recv().is_ok() {}
        let actor = || async {
            world.with(system, move |s| {
                let mut rows: Vec<_> = s.balls().filter(|b| [old, occupied, warp, cloak, far, unskilled].contains(&b.id))
                    .map(|b| (b.id, b.owner_id, b.char_id, b.interactive, b.mode, b.cloak,
                        (b.goto, b.follow_id, b.follow_range, b.speed_fraction),
                        (s.persistent_generation(b.id), s.combat_membership_generation(b.id)),
                        (b.id != warp).then_some((b.pos, b.vel)), b.health)).collect();
                rows.sort_by_key(|r| r.0);
                let ball = s.ball(warp).expect("warp actor ball");
                (s.pilot_ball(owner).map(|b| b.id), rows, (ball.pos, ball.mode, ball.effect_stamp, ball.is_warping()))
            }).await.unwrap()
        };
        for (label, target, context) in [("missing", i64::MAX, Some(old)), ("nonship", nonship, Some(old)),
            ("foreign", foreign, Some(old)), ("unassembled", packaged, Some(old)), ("occupied", occupied, Some(old)),
            ("warp", warp, Some(old)), ("cloak", cloak, Some(old)), ("far", far, Some(old)),
            ("stale old", unskilled, Some(old + 1)), ("missing skill", unskilled, Some(old))] {
            let frame = if label == "missing skill" {
                let db = test_db.clone();
                let frame = world.with(system, move |s| {
                    let generation = s.combat_membership_generation(old).unwrap();
                    let facts = s.prepare_combat_source(old, None, None, None,
                        crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
                    let hit = s.apply_damage(old, [1.0, 0.0, 0.0, 0.0],
                        crate::services::combat::damage::DamageSource { facts: Some(facts), ..Default::default() }, false).unwrap().unwrap();
                    assert!(hit.dealt > 0.0 && !hit.lethal);
                    let (_, receipt) = crate::services::combat::damage::complete_damage_outcome(s, &db, system, old, hit).unwrap();
                    assert!(receipt.is_none());
                    // Keep the genuine damage fact and dirty revision, but remove passive
                    // shield recharge from the exact before/after actor comparison.
                    let max_shield = s.combat_condition_snapshot(old).unwrap().condition.stats.max[0];
                    assert!(crate::services::combat::damage::repair_target_in_system(
                        s, old, 0, max_shield, crate::space::combat::RepairSource::Local).unwrap());
                    let condition = s.combat_condition_snapshot(old).unwrap().condition;
                    assert_eq!(condition.now[0], condition.stats.max[0]);
                    crate::space::snapshot::capture_fact_checkpoint(s).unwrap().unwrap()
                }).await.unwrap();
                while received.try_recv().is_ok() {}
                Some(frame)
            } else { None };
            let snapshot = motion_fingerprint(&test_db, warp, warp_pos, warp_destination, &format!("{label} before database")).await;
            let state = actor().await;
            assert_warp_path(state.2.0, warp_pos, warp_destination, &format!("{label} before actor mode={} stamp={} active={}", state.2.1, state.2.2, state.2.3));
            let queued = c.outbox.len();
            match label {
                "foreign" => {
                    let row = test_db.query_one("SELECT owner_id FROM game.items WHERE item_id=$1", &[&target]).await.unwrap();
                    assert_ne!(row.get::<_, i64>("owner_id"), owner, "foreign target must have a different database owner");
                }
                "occupied" | "warp" | "cloak" | "far" => {
                    world.with(system, move |s| {
                        let ball = s.ball(target).expect("target actor ball");
                        match label {
                            "occupied" => assert!(ball.interactive || ball.char_id.is_some(), "target must be occupied"),
                            "warp" => assert_eq!(ball.mode, MODE_WARP, "target must be in warp mode"),
                            "cloak" => assert_ne!(ball.cloak, 0, "target must be cloaked"),
                            "far" => {
                                let old_ball = s.sampled_ball(old).expect("old sampled ball");
                                let target_ball = s.sampled_ball(target).expect("target sampled ball");
                                let gap = crate::space::dist(old_ball.pos, target_ball.pos)
                                    - f64::from(old_ball.radius) - f64::from(target_ball.radius);
                                assert!(gap > super::super::MAX_BOARDING_DISTANCE, "target surface gap must exceed boarding range");
                            }
                            _ => unreachable!(),
                        }
                    }).await.expect("system actor");
                }
                _ => (),
            }
            let requirements = if label == "missing skill" {
                use crate::services::fit::{Fit, Key};
                let fit = Fit::load(&test_db, c.sde.clone(), owner, target).await.expect("load target fit for prerequisite diagnostic");
                ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS).filter_map(|(&skill_attr, level_attr)| {
                    let base = |attr| crate::services::fit::base_attribute(c.sde.types.get(skilled), c.sde.dogma.attribute(attr), attr);
                    let skill = fit.value(Key::Ship, skill_attr) as i32;
                    let level = fit.value(Key::Ship, level_attr) as i32;
                    (skill > 0 || base(skill_attr) > 0.0).then(|| (base(skill_attr) as i32, base(level_attr) as i32,
                        skill, level, fit.value(Key::Skill(skill), infinity_core::consts::dogma::ATTR_SKILL_LEVEL)))
                }).collect::<Vec<_>>()
            } else { Vec::new() };
            let refusal = super::super::board(&mut c, target, context).await;
            let target_type = if target == i64::MAX { None } else if target == nonship { Some(nonship_type) }
                else if target == unskilled { Some(skilled) } else { Some(kind) };
            let actual_refusal: String = format!("{refusal:?}").chars().take(2048).collect();
            assert!(matches!(&refusal, Err(crate::services::CallError::User { .. })), "{label}: target_type={target_type:?}, actual_refusal={actual_refusal}, requirements=(base_skill, base_level, effective_skill, effective_required_level, effective_pilot_level) {requirements:?}; must be a normal refusal");
            if label == "missing skill" {
                assert!(matches!(refusal, Err(crate::services::CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_FITTING_HAS_SKILL_PREREQUISITES));
            }
            if let Some(frame) = frame {
                assert!(!frame.is_committed(&test_db).await.unwrap());
                assert!(world.with(system, move |s| {
                    let state = s.combat_condition_snapshot(old).unwrap();
                    state.revision != state.saved_revision
                }).await.unwrap());
            }
            assert_eq!(motion_fingerprint(&test_db, warp, warp_pos, warp_destination, &format!("{label} after database")).await, snapshot, "{label}");
            let after = actor().await;
            assert_warp_path(after.2.0, warp_pos, warp_destination, &format!("{label} after actor mode={} stamp={} active={}", after.2.1, after.2.2, after.2.3));
            assert_eq!((&after.0, &after.1), (&state.0, &state.1), "{label}");
            assert_eq!(c.session.int("shipid"), Some(old), "{label}");
            assert_eq!(c.outbox.len(), queued, "{label}");
            assert!(received.try_recv().is_err(), "{label} emitted a destiny notification");
        }
    }).await;
    cleanup(&db, foreign_account, foreign_owner).await;
    cleanup(&db, account, owner).await;
    assert_eq!(fingerprint(&db).await, before);
    result.unwrap();
}
