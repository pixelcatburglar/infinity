//! Starter-reset regressions: the committed apply restores the race seed
//! exactly, and an injected failure rolls back every write it made.
use super::*;

/// The creation seed: the first starter row fixes each type's target level
/// and exact skill points.
fn seed_map(sde: &Sde, race: i32) -> BTreeMap<i32, (i32, i64)> {
    let mut seed = BTreeMap::new();
    for (type_id, level) in starting_levels(sde, race) {
        seed.entry(type_id).or_insert((level, sp_for_level(skill_type(sde, type_id).rank, level as i64)));
    }
    seed
}

async fn trained(db: &crate::db::Db, character: i64) -> BTreeMap<i32, (i32, i64)> {
    training::current(db, character).await.unwrap().into_iter().map(|skill| (skill.type_id, (skill.level, skill.skill_points))).collect()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL and loaded SDE"]
async fn reset_to_starters_restores_seed_and_discards_queue() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.expect("database pool");
    let fixture = crate::services::tests::fixture::create(&db).await.expect("character fixture");
    let character = fixture.character;
    let result = crate::tests::catch_scenario(async {
        let (mut client, _peer) = crate::services::tests::client(db.clone(), character).await;
        let sde = client.sde.clone();
        let seed = seed_map(&sde, training::race(&db, character).await.unwrap().unwrap());
        assert!(seed.len() >= 2, "fixture race needs a starter template");
        let mut types = seed.keys();
        let (raised, missing) = (*types.next().unwrap(), *types.next().unwrap());
        let nonstarter = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SKILL) && !seed.contains_key(&t.type_id)).expect("nonstarter skill").type_id;
        // Disturb the seed: one starter raised above its target, one starter
        // missing, one level-0 nonstarter, an overdue/active/future queue and
        // free points. An active expert installation and spent respec must
        // survive the reset untouched.
        for (&type_id, &(level, sp)) in &seed {
            if type_id != missing && type_id != raised { training::set_level(&db, character, type_id, level, sp).await.unwrap(); }
        }
        let &(level, sp) = &seed[&raised];
        training::set_level(&db, character, raised, (level % 5) + 1, sp + 1).await.unwrap();
        training::set_level(&db, character, nonstarter, 0, 0).await.unwrap();
        let now = filetime_now();
        for (position, (type_id, start, end)) in [(missing, now - 30, now - 20), (raised, now - 20, now + 1_000_000_000), (nonstarter, now + 2_000_000_000, now + 3_000_000_000)].into_iter().enumerate() {
            queue::insert(&db, character, &entry(position as i32, type_id, Some(start), Some(end))).await.unwrap();
        }
        db.execute("UPDATE game.characters SET free_skill_points = $2 WHERE character_id = $1", &[&character, &7_i64]).await.unwrap();
        let expert = sde.expert_systems.iter().next().expect("expert definition");
        let expires = now + expert.duration_filetime;
        assert!(crate::dao::skills::expert_systems::insert(&db, character, expert.type_id, now, expires).await.unwrap());
        let respec = crate::dao::characters::attributes::initialize_respec(&db, character, 2).await.unwrap();
        assert!(crate::dao::characters::attributes::save_respec(&db, character, &[4, 8, 15, 16, 23], &respec, 1, 11, 22).await.unwrap());

        apply(&mut client, character, Change::ResetToStarters).await.expect("starter reset");

        assert_eq!(trained(&db, character).await, seed, "trained set must equal the first-occurrence race seed");
        assert!(queue::list(&db, character).await.unwrap().is_empty(), "every old interval is discarded");
        assert_eq!(crate::dao::characters::attributes::free_points(&db, character).await.unwrap(), 0);
        let installs = crate::dao::skills::expert_systems::all(&db, character).await.unwrap();
        assert_eq!(installs.iter().map(|i| (i.type_id, i.installed_at, i.expires_at)).collect::<Vec<_>>(), [(expert.type_id, now, expires)], "expert installation preserved");
        let remap = crate::dao::characters::attributes::respec(&db, character).await.unwrap().unwrap();
        assert_eq!((remap.bases.as_deref(), remap.free_respecs, remap.last_respec_date, remap.next_timed_respec), (Some(&[4, 8, 15, 16, 23][..]), 1, Some(11), Some(22)), "respec entitlements preserved");
        let job_id = format!("{}:{character}", completion::KIND);
        let due: i64 = db.query_one("SELECT due_at FROM game.jobs WHERE job_id = $1", &[&job_id]).await.unwrap().get("due_at");
        assert_eq!(due, expires, "the completion job keeps the expert expiry");
        let history = crate::dao::skills::history::list(&db, character, 1000).await.unwrap();
        assert!(history.iter().any(|row| row.skill_type_id == nonstarter && row.event_type_id == super::super::super::history::REMOVED));
        assert!(history.iter().all(|row| row.event_type_id != super::super::super::history::TRAINING_COMPLETE), "the reset records no natural completion");

        // Replaying the armed completion job and reapplying the reset must
        // resurrect no old interval and append no further history.
        let job = crate::scheduler::Job { id: job_id.clone(), kind: completion::KIND.into(), due_at: due, payload: character.to_string() };
        completion::handle(db.clone(), client.world.clone(), job).await.expect("completion replay");
        apply(&mut client, character, Change::ResetToStarters).await.expect("idempotent reset");
        assert_eq!(trained(&db, character).await, seed);
        assert!(queue::list(&db, character).await.unwrap().is_empty());
        let replay_due: i64 = db.query_one("SELECT due_at FROM game.jobs WHERE job_id = $1", &[&job_id]).await.unwrap().get("due_at");
        assert_eq!(replay_due, expires, "expert expiry still drives the job");
        let after = crate::dao::skills::history::list(&db, character, 1000).await.unwrap();
        assert_eq!(after.len(), history.len(), "replay and idempotent reset append no history");
    }).await;
    crate::scheduler::cancel(&db, &format!("{}:{character}", completion::KIND)).await.expect("cancel completion job");
    crate::services::tests::fixture::cleanup(&db, fixture).await.expect("cleanup fixture");
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL and loaded SDE"]
async fn reset_to_starters_rolls_back_every_write() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.expect("database pool");
    let fixture = crate::services::tests::fixture::create(&db).await.expect("character fixture");
    let character = fixture.character;
    let result = crate::tests::catch_scenario(async {
        let sde = crate::tests::shared_sde(&db).await;
        let seed = seed_map(&sde, training::race(&db, character).await.unwrap().unwrap());
        let nonstarter = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SKILL) && !seed.contains_key(&t.type_id)).expect("nonstarter skill").type_id;
        training::set_level(&db, character, nonstarter, 3, 999).await.unwrap();
        queue::insert(&db, character, &entry(0, nonstarter, Some(1), Some(2))).await.unwrap();
        db.execute("UPDATE game.characters SET free_skill_points = 5 WHERE character_id = $1", &[&character]).await.unwrap();
        let expert = sde.expert_systems.iter().next().expect("expert definition");
        assert!(crate::dao::skills::expert_systems::insert(&db, character, expert.type_id, 1, filetime_now() + expert.duration_filetime).await.unwrap());
        let before = crate::services::tests::fixture::skill_fingerprint(&db, character).await.unwrap();
        let world = crate::space::World::new(db.clone(), sde.clone(), 1);
        let fences = online::capture_online_fences(&world, &[character]).await;
        let _market = crate::services::market::lock().await;
        let dispatched = std::sync::atomic::AtomicBool::new(false);
        let failed: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = sde.clone();
            let fences = fences.clone();
            Box::pin(async move {
                reset_to_starters(tx, outbox, &sde, &fences, character, filetime_now()).await?;
                Err(CallError::Internal(anyhow::anyhow!("injected reset failure")))
            })
        }, |(), _| async { dispatched.store(true, std::sync::atomic::Ordering::SeqCst); Ok::<(), CallError>(()) }).await;
        assert!(matches!(failed, Err(CallError::Internal(_))), "injected failure must surface");
        assert!(!dispatched.load(std::sync::atomic::Ordering::SeqCst), "a rolled-back reset must not dispatch");
        assert_eq!(crate::services::tests::fixture::skill_fingerprint(&db, character).await.unwrap(), before, "skill and queue rows restored");
        assert_eq!(crate::dao::characters::attributes::free_points(&db, character).await.unwrap(), 5);
        assert!(crate::dao::skills::history::list(&db, character, 1000).await.unwrap().is_empty(), "history restored");
        let jobs: i64 = db.query_one("SELECT count(*) AS count FROM game.jobs WHERE job_id = $1", &[&format!("{}:{character}", completion::KIND)]).await.unwrap().get("count");
        assert_eq!(jobs, 0, "the rescheduled completion job rolled back with it");
    }).await;
    crate::scheduler::cancel(&db, &format!("{}:{character}", completion::KIND)).await.expect("cancel completion job");
    crate::services::tests::fixture::cleanup(&db, fixture).await.expect("cleanup fixture");
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
