//! Committed character-domain publication into resident actor fits.
use super::super::{advance_tx, finish_tx, handle, KIND};
use crate::client::filetime_now;
use crate::dao::characters::selection;
use crate::db::{outbox::Outbox, Db};
use crate::scheduler::{self, Job};
use crate::services::{db_err, CallError};
use crate::services::effects::online::{self, OnlineChanged};
use crate::services::fit::Key;
use crate::services::tests::{client, fixture};
use infinity_core::consts::{dogma::ATTR_ARMOR_HP, dogma::ATTR_SHIELD_CAPACITY, dogma::ATTR_SKILL_LEVEL,
    dogma::ATTR_STRUCTURE_HP, CATEGORY_SHIP, CATEGORY_SKILL};

const SYSTEM: i64 = 1;

/// One module-less hull admitted in space: a due natural completion and a GM
/// level change each republish the committed character domain through the real
/// finish entry and outbox, with no online-hull candidate in the reconcile
/// query and no client brain update. The next gated read observes the new
/// effective values; a producer transaction failing before commit publishes
/// nothing.
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn committed_finish_replaces_character_domain_on_resident_fit() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let world = c.world.clone();
    let hull = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_SHIELD_CAPACITY).is_some_and(|v| v > 0.0)).expect("published hull").type_id;
    let dogma = &sde.dogma;
    let candidates: Vec<(i32, i32)> = sde.types.iter()
        .filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SKILL))
        .flat_map(|t| [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP].into_iter()
            .filter_map(move |attr| t.modifiers(dogma, attr)
                .any(|(_, m)| m.func == "ItemModifier" && m.domain == "shipID").then_some((t.type_id, attr))))
        .collect();
    let ship = crate::dao::items::creation::positioned(&db, hull, f.character, SYSTEM, [0.0; 3]).await.unwrap();
    db.execute("UPDATE game.characters SET solar_system_id=$2, station_id=NULL WHERE character_id=$1", &[&f.character, &SYSTEM]).await.unwrap();
    selection::set_ship(&db, f.character, ship).await.unwrap();
    world.system(SYSTEM).await.unwrap();
    {
        let _market = crate::services::market::lock().await;
        let vessel = crate::dao::items::space_ships::FreeShip {
            item_id: ship, type_id: hull, owner_id: f.character, corporation_id: 1, alliance_id: -1, pos: [0.0; 3],
        };
        let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &vessel, f.character, 0.0).await.ok().expect("prepare ship");
        world.with(SYSTEM, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.expect("system actor");
    }
    let mut recipient = None;
    let scenario = crate::tests::catch_scenario(async {
    {
        let market = crate::services::market::lock().await;
        recipient = Some(fixture::LiveRecipient::cold(&world, f.character, Some(SYSTEM), &market).await);
    }
    let live = recipient.as_ref().unwrap();
    // Natural completion without any client: a due queue row completes through
    // the real job entry until an SDE ship modifier proves the publish moved a
    // fitted value.
    let mut proven = None;
    for &(skill, attr) in &candidates {
        let before = world.with(SYSTEM, move |s| s.with_combat_fit(ship, move |fit, _, _| fit.value(Key::Ship, attr))).await.flatten().expect("live fit");
        db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,0,0) ON CONFLICT DO NOTHING", &[&f.character, &skill]).await.unwrap();
        let sp = crate::services::skills::formulas::sp_for_level(crate::services::skills::training::skill_type(&sde, skill).rank, 2);
        db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,0,$2,2,0,$3,1,2)", &[&f.character, &skill, &sp]).await.unwrap();
        let job = Job { id: format!("{KIND}:{}", f.character), kind: KIND.into(), due_at: 2, payload: f.character.to_string() };
        handle(db.clone(), world.clone(), job).await.unwrap();
        let seen = world.with(SYSTEM, move |s| s.with_combat_fit(ship, move |fit, _, _|
            (fit.value(Key::Skill(skill), ATTR_SKILL_LEVEL), fit.value(Key::Ship, attr)))).await.flatten().expect("live fit");
        assert_eq!(seen.0, 2.0, "committed completion did not reach the resident fit");
        assert_eq!(live.read(move |fit|
            (fit.value(Key::Skill(skill), ATTR_SKILL_LEVEL), fit.value(Key::Ship, attr))).await, seen);
        if seen.1 != before { proven = Some((skill, attr, seen.1)); break; }
    }
    let (skill, attr, mut current) = proven.expect("no SDE ship modifier moved a fitted value");
    // A GM change reuses the same finish entry; the next gated read is new.
    crate::services::skills::gm::apply(&mut c, f.character, crate::services::skills::gm::Change::Set(vec![(skill, 5)])).await.ok().expect("gm set");
    let seen = world.with(SYSTEM, move |s| s.with_combat_fit(ship, move |fit, _, _|
        (fit.value(Key::Skill(skill), ATTR_SKILL_LEVEL), fit.value(Key::Ship, attr)))).await.flatten().expect("live fit");
    assert_eq!(seen.0, 5.0, "GM change did not reach the resident fit");
    assert_ne!(seen.1, current, "GM change did not move the gated value");
    assert_eq!(live.read(move |fit|
        (fit.value(Key::Skill(skill), ATTR_SKILL_LEVEL), fit.value(Key::Ship, attr))).await, seen);
    current = seen.1;
    // A producer transaction that fails before commit publishes nothing: the
    // resident fit keeps the last committed domain and receives no operation.
    let failed: Result<(), CallError> = {
        let _market = crate::services::market::lock().await;
        let fences = online::capture_online_fences(&world, &[f.character]).await;
        db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            let fences = fences.clone();
            Box::pin(async move {
                selection::lock_character(tx, f.character).await.map_err(db_err)?;
                let progress = advance_tx(tx, outbox, &sde, f.character, filetime_now()).await?;
                finish_tx(tx, outbox, &sde, &fences, f.character, filetime_now(), &progress, &[]).await?;
                tx.query_one("SELECT 1 / 0", &[]).await.map_err(|e| db_err(crate::db::Error::from(e)))?;
                Ok(())
            })
        }, |(), _| async { unreachable!("rolled-back outbox dispatched") }).await
    };
    fixture::assert_division_by_zero(failed);
    let seen = world.with(SYSTEM, move |s| s.with_combat_fit(ship, move |fit, _, _|
        (fit.value(Key::Skill(skill), ATTR_SKILL_LEVEL), fit.value(Key::Ship, attr)))).await.flatten().expect("live fit");
    assert_eq!(seen, (5.0, current), "rolled-back publish leaked into the resident fit");
    assert_eq!(live.read(move |fit|
        (fit.value(Key::Skill(skill), ATTR_SKILL_LEVEL), fit.value(Key::Ship, attr))).await, (5.0, current));
    crate::services::skills::gm::apply(&mut c, f.character, crate::services::skills::gm::Change::Remove(vec![skill])).await.ok().expect("gm remove");
    let removed = world.with(SYSTEM, move |s| s.with_combat_fit(ship, move |fit, _, _| fit.value(Key::Ship, attr))).await.flatten().unwrap();
    assert_eq!(live.read(move |fit|
        (fit.source_type(Key::Skill(skill)), fit.value(Key::Ship, attr))).await, (None, removed));
    assert_ne!(removed, current);
    crate::services::skills::gm::apply(&mut c, f.character, crate::services::skills::gm::Change::Set(vec![(skill, 5)])).await.ok().expect("restore skill");
    assert_eq!(live.read(move |fit|
        (fit.value(Key::Skill(skill), ATTR_SKILL_LEVEL), fit.value(Key::Ship, attr))).await, (5.0, current));
    }).await;
    if let Some(live) = recipient {
        live.close().await;
    }
    world.remove_ball(SYSTEM, ship).await;
    world.unregister_pilot_generation(f.character, None);
    crate::services::combat::forget_ship(ship);
    world.flush_positions().await.unwrap();
    scheduler::cancel(&db, &format!("{KIND}:{}", f.character)).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
}
