//! Durable chat moderation expiry regressions against a dedicated database.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use crate::{services::{CallError, chat::authority::RoomConfig, tests::fixture}, session::OnlineSession};

fn session(id: i64) -> OnlineSession {
    OnlineSession { character_id: id, user_id: None, session_id: id, ship_id: None, station_id: None, system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }
}

async fn job(db: &Db, room: &str, target: i64, kind: Restriction) -> Option<Job> {
    let row = db.query_opt("SELECT job_id, kind, due_at, payload FROM game.jobs WHERE job_id = $1", &[&job_id(room, target, kind)]).await.unwrap()?;
    Some(Job { id: row.get("job_id"), kind: row.get("kind"), due_at: row.get("due_at"), payload: row.get("payload") })
}

fn due_job(room: &str, target: i64, kind: Restriction, deadline: i64) -> Job {
    Job { id: job_id(room, target, kind), kind: KIND.into(), due_at: deadline,
        payload: serde_json::to_string(&Payload { room: room.into(), character: target, kind: kind.as_str().into(), expires_at: deadline }).unwrap() }
}

// Backdate through the real attachment and scheduler transaction; the explicit earlier
// fixture clock allows a due record without sleeping or changing the server clock.
async fn seed_due(db: &Db, room: &str, target: i64, kind: Restriction, deadline: i64) -> Job {
    let job = Arc::new(due_job(room, target, kind, deadline));
    let room: Arc<str> = Arc::from(room);
    db.transaction(|tx, _: &mut ()| { let job = Arc::clone(&job); let room = Arc::clone(&room); Box::pin(async move {
        assert!(dao::attach_expiry(tx, &room, target, kind, deadline, deadline - 1).await?);
        scheduler::schedule(tx, &job).await?;
        Ok::<_, db::Error>(())
    }) }, |(), ()| async { Ok(()) }).await.unwrap();
    Arc::try_unwrap(job).unwrap()
}

async fn setup() -> (Db, fixture::Fixture, fixture::Fixture, String) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let target = fixture::create(&db).await.unwrap();
    let room = authority::create_player(&db, owner.character, &format!("Expiry {}", owner.character)).await.unwrap();
    (db, owner, target, room)
}

async fn cleanup(db: &Db, owner: fixture::Fixture, target: fixture::Fixture, room: &str) {
    for kind in [Restriction::Mute, Restriction::Ban] { scheduler::cancel(db, &job_id(room, target.character, kind)).await.unwrap(); }
    db.execute("DELETE FROM game.chat_channels WHERE name = $1 OR owner_id = ANY($2)", &[&room, &vec![owner.character, target.character]]).await.unwrap();
    fixture::cleanup(db, target).await.unwrap();
    fixture::cleanup(db, owner).await.unwrap();
}

async fn public(db: &Db, hub: &Hub, owner: &OnlineSession, room: &str) {
    authority::configure(db, hub, owner, room, RoomConfig { display_name: format!("Expiry {}", owner.character_id), public: true, moderated: false, members_by_default: false }).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn independent_records_replacement_removal_and_rollback() {
    let (db, owner, target, room) = setup().await;
    let (test_db, test_room) = (db.clone(), room.clone());
    let result = tokio::spawn(async move {
        let (client, _peer) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (hub, world, sde) = (&client.chat, client.world.clone(), &client.sde);
        let (o, t) = (session(owner.character), session(target.character));
        let r = test_room.as_str();
        public(&test_db, hub, &o, r).await;
        assert!(set(&test_db, hub, &o, r, target.character, Restriction::Mute, 10).await.is_err());
        assert!(job(&test_db, r, target.character, Restriction::Mute).await.is_none());
        authority::set_role(&test_db, hub, &o, r, target.character, "visitor").await.unwrap();
        assert_eq!(authority::admit(&test_db, sde, &t, r).await.unwrap().unwrap().role, "visitor");
        for duration in [0, -1, i64::MAX] {
            assert!(set(&test_db, hub, &o, r, target.character, Restriction::Mute, duration).await.is_err());
        }
        assert!(set(&test_db, hub, &t, r, target.character, Restriction::Mute, 10).await.is_err());
        assert!(set(&test_db, hub, &session(0), r, target.character, Restriction::Mute, 10).await.is_err());
        let private = authority::create_private(&test_db, owner.character, target.character).await.unwrap();
        assert!(set(&test_db, hub, &o, &private, target.character, Restriction::Mute, 10).await.is_err());
        assert!(set(&test_db, hub, &o, "help_zh", target.character, Restriction::Mute, 10).await.is_err());
        assert!(job(&test_db, r, target.character, Restriction::Mute).await.is_none());
        assert!(dao::member(&test_db, r, target.character).await.unwrap().unwrap().voice_expires_at.is_none());
        set(&test_db, hub, &o, r, target.character, Restriction::Mute, 315_360_000).await.unwrap();
        let initial = job(&test_db, r, target.character, Restriction::Mute).await.unwrap();
        assert_eq!(dao::member(&test_db, r, target.character).await.unwrap().unwrap().voice_expires_at, Some(initial.due_at));
        assert_eq!(dao::member(&test_db, r, target.character).await.unwrap().unwrap().voice, dao::Voice::Default);
        assert_eq!(authority::admit(&test_db, sde, &t, r).await.unwrap().unwrap().role, "visitor");
        assert_eq!(initial.id, job_id(r, target.character, Restriction::Mute));
        authority::set_role(&test_db, hub, &o, r, target.character, "participant").await.unwrap();
        assert_eq!(authority::admit(&test_db, sde, &t, r).await.unwrap().unwrap().role, "visitor");
        set(&test_db, hub, &o, r, target.character, Restriction::Mute, 100).await.unwrap();
        let replacement = job(&test_db, r, target.character, Restriction::Mute).await.unwrap();
        assert_eq!(replacement.id, initial.id);
        assert_ne!(replacement.due_at, initial.due_at);
        handle(test_db.clone(), world.clone(), initial.clone()).await.unwrap();
        assert_eq!(job(&test_db, r, target.character, Restriction::Mute).await.unwrap().payload, replacement.payload);
        assert_eq!(dao::member(&test_db, r, target.character).await.unwrap().unwrap().voice_expires_at, Some(replacement.due_at));
        let stale_due = seed_due(&test_db, r, target.character, Restriction::Mute, filetime_now() - 1).await;
        authority::set_role(&test_db, hub, &o, r, target.character, "visitor").await.unwrap();
        set(&test_db, hub, &o, r, target.character, Restriction::Mute, 100).await.unwrap();
        authority::set_role(&test_db, hub, &o, r, target.character, "participant").await.unwrap();
        let current = job(&test_db, r, target.character, Restriction::Mute).await.unwrap();
        handle(test_db.clone(), world.clone(), stale_due).await.unwrap();
        assert_eq!(job(&test_db, r, target.character, Restriction::Mute).await.unwrap().payload, current.payload);
        assert_eq!(dao::member(&test_db, r, target.character).await.unwrap().unwrap().voice_expires_at, Some(current.due_at));
        let due = seed_due(&test_db, r, target.character, Restriction::Mute, filetime_now() - 1).await;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        world.register_pilot(target.character, sender);
        handle(test_db.clone(), world.clone(), due.clone()).await.unwrap();
        assert_eq!(receiver.try_recv().unwrap().0, PERMISSIONS_REFRESH);
        assert_eq!(authority::admit(&test_db, sde, &t, r).await.unwrap().unwrap().role, "participant");
        handle(test_db.clone(), world.clone(), due).await.unwrap();
        assert!(receiver.try_recv().is_err());
        scheduler::cancel(&test_db, &replacement.id).await.unwrap();

        authority::set_affiliation(&test_db, hub, &o, r, target.character, "outcast").await.unwrap();
        assert!(authority::admit(&test_db, sde, &t, r).await.unwrap().is_none());
        set(&test_db, hub, &o, r, target.character, Restriction::Ban, 100).await.unwrap();
        assert_eq!(dao::member(&test_db, r, target.character).await.unwrap().unwrap().affiliation, dao::Affiliation::None);
        assert!(authority::admit(&test_db, sde, &t, r).await.unwrap().is_none());
        authority::set_affiliation(&test_db, hub, &o, r, target.character, "none").await.unwrap();
        assert!(authority::admit(&test_db, sde, &t, r).await.unwrap().is_none());
        assert!(authority::invite(&test_db, hub, &o, r, target.character).await.is_err());
        let due = seed_due(&test_db, r, target.character, Restriction::Ban, filetime_now() - 1).await;
        handle(test_db.clone(), world.clone(), due).await.unwrap();
        assert!(authority::admit(&test_db, sde, &t, r).await.unwrap().is_some());
        scheduler::cancel(&test_db, &job_id(r, target.character, Restriction::Ban)).await.unwrap();
        while receiver.try_recv().is_ok() {}

        authority::set_role(&test_db, hub, &o, r, target.character, "visitor").await.unwrap();
        set(&test_db, hub, &o, r, target.character, Restriction::Mute, 100).await.unwrap();
        authority::set_affiliation(&test_db, hub, &o, r, target.character, "outcast").await.unwrap();
        set(&test_db, hub, &o, r, target.character, Restriction::Ban, 100).await.unwrap();
        let mute = job(&test_db, r, target.character, Restriction::Mute).await.unwrap();
        let ban = job(&test_db, r, target.character, Restriction::Ban).await.unwrap();
        let dispatched = AtomicBool::new(false);
        let rollback_room: Arc<str> = Arc::from(r);
        let due_at = mute.due_at;
        let character = target.character;
        let forced: Result<(), CallError> = test_db.transaction(|tx, outbox: &mut Outbox| { let room = Arc::clone(&rollback_room); Box::pin(async move {
            assert!(dao::attach_expiry(tx, &room, character, Restriction::Mute, due_at + 1, filetime_now()).await?);
            scheduler::schedule(tx, &due_job(&room, character, Restriction::Mute, due_at + 1)).await?;
            outbox.notify_pilot(character, PERMISSIONS_REFRESH, vec![Value::str(room.as_ref())]);
            Err(notify_error("forced rollback"))
        }) }, |(), _| async { dispatched.store(true, Ordering::SeqCst); Ok(()) }).await;
        assert!(forced.is_err());
        assert!(!dispatched.load(Ordering::SeqCst));
        assert_eq!(job(&test_db, r, target.character, Restriction::Mute).await.unwrap().payload, mute.payload);
        assert_eq!(dao::member(&test_db, r, target.character).await.unwrap().unwrap().voice_expires_at, Some(mute.due_at));
        let expired_mute = seed_due(&test_db, r, target.character, Restriction::Mute, filetime_now() - 1).await;
        handle(test_db.clone(), world.clone(), expired_mute).await.unwrap();
        let parallel = dao::member(&test_db, r, target.character).await.unwrap().unwrap();
        assert_eq!(parallel.ban_expires_at, Some(ban.due_at));
        assert!(parallel.voice_expires_at.is_none());
        assert_eq!(job(&test_db, r, target.character, Restriction::Ban).await.unwrap().payload, ban.payload);
        authority::set_affiliation(&test_db, hub, &o, r, target.character, "none").await.unwrap();
        authority::set_role(&test_db, hub, &o, r, target.character, "visitor").await.unwrap();
        set(&test_db, hub, &o, r, target.character, Restriction::Mute, 100).await.unwrap();
        let mute = job(&test_db, r, target.character, Restriction::Mute).await.unwrap();
        authority::set_affiliation(&test_db, hub, &o, r, target.character, "none").await.unwrap();
        remove(&test_db, hub, &o, r, target.character, Restriction::Ban).await.unwrap();
        assert!(job(&test_db, r, target.character, Restriction::Ban).await.is_none());
        assert_eq!(dao::member(&test_db, r, target.character).await.unwrap().unwrap().voice_expires_at, Some(mute.due_at));
        assert_eq!(authority::admit(&test_db, sde, &t, r).await.unwrap().unwrap().role, "visitor");
        while receiver.try_recv().is_ok() {}
        handle(test_db.clone(), world.clone(), ban).await.unwrap();
        assert!(receiver.try_recv().is_err());
        assert!(authority::admit(&test_db, sde, &t, r).await.unwrap().is_some());
        remove(&test_db, hub, &o, r, target.character, Restriction::Ban).await.unwrap();
        authority::set_role(&test_db, hub, &o, r, target.character, "participant").await.unwrap();
        remove(&test_db, hub, &o, r, target.character, Restriction::Mute).await.unwrap();
        assert!(job(&test_db, r, target.character, Restriction::Mute).await.is_none());
        assert_eq!(authority::admit(&test_db, sde, &t, r).await.unwrap().unwrap().role, "participant");
        while receiver.try_recv().is_ok() {}
        handle(test_db.clone(), world.clone(), mute).await.unwrap();
        assert!(receiver.try_recv().is_err());
        assert_eq!(authority::admit(&test_db, sde, &t, r).await.unwrap().unwrap().role, "participant");
        remove(&test_db, hub, &o, r, target.character, Restriction::Mute).await.unwrap();

        authority::set_role(&test_db, hub, &o, r, target.character, "visitor").await.unwrap();
        set(&test_db, hub, &o, r, target.character, Restriction::Mute, 100).await.unwrap();
        authority::set_affiliation(&test_db, hub, &o, r, target.character, "outcast").await.unwrap();
        set(&test_db, hub, &o, r, target.character, Restriction::Ban, 100).await.unwrap();
        authority::set_affiliation(&test_db, hub, &o, r, target.character, "owner").await.unwrap();
        assert!(job(&test_db, r, target.character, Restriction::Mute).await.is_none());
        assert!(job(&test_db, r, target.character, Restriction::Ban).await.is_none());
        authority::set_role(&test_db, hub, &t, r, owner.character, "visitor").await.unwrap();
        set(&test_db, hub, &t, r, owner.character, Restriction::Mute, 100).await.unwrap();
        authority::destroy(&test_db, hub, &t, r).await.unwrap();
        assert!(job(&test_db, r, owner.character, Restriction::Mute).await.is_none());
    }).await;
    for kind in [Restriction::Mute, Restriction::Ban] { scheduler::cancel(&db, &job_id(&room, owner.character, kind)).await.unwrap(); }
    cleanup(&db, owner, target, &room).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn persisted_due_job_runs_after_restart_and_drains() {
    let (db, owner, target, room) = setup().await;
    let (test_db, test_room) = (db, room.clone());
    let result = tokio::spawn(async move {
        let (client, peer) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let o = session(owner.character);
        public(&test_db, &client.chat, &o, &test_room).await;
        authority::set_role(&test_db, &client.chat, &o, &test_room, target.character, "visitor").await.unwrap();
        set(&test_db, &client.chat, &o, &test_room, target.character, Restriction::Mute, 100).await.unwrap();
        seed_due(&test_db, &test_room, target.character, Restriction::Mute, filetime_now() - 1).await;
        drop(client); drop(peer); drop(test_db);
        let reopened = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let sde = Arc::new(crate::sde::Sde::load(&reopened).await.unwrap());
        let world = World::new(reopened.clone(), sde.clone(), 1);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        world.register_pilot(target.character, sender);
        let (shutdown, watch) = tokio::sync::watch::channel(false);
        let mut runner = scheduler::runner::start(reopened.clone(), world, crate::services::scheduler_registrations(), crate::config::scheduler::SchedulerConfig::default(), watch).unwrap();
        let notification = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some((event, _)) = receiver.recv().await { if event == PERMISSIONS_REFRESH { return true; } }
            false
        }).await;
        let _ = shutdown.send(true);
        let drained = tokio::time::timeout(std::time::Duration::from_secs(10), &mut runner).await;
        if drained.is_err() { runner.abort(); let _ = runner.await; }
        assert!(matches!(notification, Ok(true)), "runner did not deliver permission refresh");
        drained.expect("runner did not drain").unwrap();
        assert!(job(&reopened, &test_room, target.character, Restriction::Mute).await.is_none());
        assert!(dao::member(&reopened, &test_room, target.character).await.unwrap().unwrap().voice_expires_at.is_none());
        assert_eq!(authority::admit(&reopened, &sde, &session(target.character), &test_room).await.unwrap().unwrap().role, "participant");
    }).await;
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    cleanup(&db, owner, target, &room).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn later_plain_restrictions_survive_remove_due_and_stale_jobs() {
    let (db, owner, target, room) = setup().await;
    let (test_db, test_room) = (db.clone(), room.clone());
    let result = tokio::spawn(async move {
        let (client, _peer) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (hub, world, sde) = (&client.chat, client.world.clone(), &client.sde);
        let (o, t, r) = (session(owner.character), session(target.character), test_room.as_str());
        public(&test_db, hub, &o, r).await;
        for kind in [Restriction::Mute, Restriction::Ban] {
            for via_due in [false, true] {
                match kind {
                    Restriction::Mute => authority::set_role(&test_db, hub, &o, r, target.character, "visitor").await.unwrap(),
                    Restriction::Ban => authority::set_affiliation(&test_db, hub, &o, r, target.character, "outcast").await.unwrap(),
                }
                let old = if via_due {
                    seed_due(&test_db, r, target.character, kind, filetime_now() - 1).await
                } else {
                    set(&test_db, hub, &o, r, target.character, kind, 100).await.unwrap();
                    job(&test_db, r, target.character, kind).await.unwrap()
                };
                // This assignment must follow attachment, which normalizes the original plain state.
                match kind {
                    Restriction::Mute => authority::set_role(&test_db, hub, &o, r, target.character, "visitor").await.unwrap(),
                    Restriction::Ban => authority::set_affiliation(&test_db, hub, &o, r, target.character, "outcast").await.unwrap(),
                }
                if via_due {
                    handle(test_db.clone(), world.clone(), old.clone()).await.unwrap();
                } else {
                    remove(&test_db, hub, &o, r, target.character, kind).await.unwrap();
                }
                let member = dao::member(&test_db, r, target.character).await.unwrap().unwrap();
                match kind {
                    Restriction::Mute => {
                        assert!(member.voice_expires_at.is_none());
                        assert_eq!(member.voice, dao::Voice::Muted);
                        assert_eq!(authority::admit(&test_db, sde, &t, r).await.unwrap().unwrap().role, "visitor");
                    }
                    Restriction::Ban => {
                        assert!(member.ban_expires_at.is_none());
                        assert_eq!(member.affiliation, dao::Affiliation::Outcast);
                        assert!(authority::admit(&test_db, sde, &t, r).await.unwrap().is_none());
                    }
                }
                // A newer attachment followed by another plain assignment must survive the old job.
                set(&test_db, hub, &o, r, target.character, kind, 100).await.unwrap();
                let current = job(&test_db, r, target.character, kind).await.unwrap();
                match kind {
                    Restriction::Mute => authority::set_role(&test_db, hub, &o, r, target.character, "visitor").await.unwrap(),
                    Restriction::Ban => authority::set_affiliation(&test_db, hub, &o, r, target.character, "outcast").await.unwrap(),
                }
                handle(test_db.clone(), world.clone(), old).await.unwrap();
                assert_eq!(job(&test_db, r, target.character, kind).await.unwrap().payload, current.payload);
                let member = dao::member(&test_db, r, target.character).await.unwrap().unwrap();
                match kind {
                    Restriction::Mute => { assert_eq!(member.voice, dao::Voice::Muted); assert_eq!(member.voice_expires_at, Some(current.due_at)); }
                    Restriction::Ban => { assert_eq!(member.affiliation, dao::Affiliation::Outcast); assert_eq!(member.ban_expires_at, Some(current.due_at)); }
                }
                remove(&test_db, hub, &o, r, target.character, kind).await.unwrap();
                match kind {
                    Restriction::Mute => authority::set_role(&test_db, hub, &o, r, target.character, "participant").await.unwrap(),
                    Restriction::Ban => authority::set_affiliation(&test_db, hub, &o, r, target.character, "none").await.unwrap(),
                }
                scheduler::cancel(&test_db, &job_id(r, target.character, kind)).await.unwrap();
            }
        }
    }).await;
    cleanup(&db, owner, target, &room).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn restored_restricted_admin_cannot_manage_until_lift_or_expiry() {
    let (db, owner, target, room) = setup().await;
    let victim = fixture::create(&db).await.unwrap();
    let (test_db, test_room) = (db.clone(), room.clone());
    let result = tokio::spawn(async move {
        let (client, _peer) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (hub, world, sde) = (&client.chat, client.world.clone(), &client.sde);
        let (o, t, r) = (session(owner.character), session(target.character), test_room.as_str());
        public(&test_db, hub, &o, r).await;
        authority::set_affiliation(&test_db, hub, &o, r, victim.character, "member").await.unwrap();
        authority::set_role(&test_db, hub, &o, r, victim.character, "visitor").await.unwrap();
        for kind in [Restriction::Mute, Restriction::Ban] {
            for via_due in [false, true] {
                authority::set_affiliation(&test_db, hub, &o, r, target.character, "member").await.unwrap();
                match kind {
                    Restriction::Mute => authority::set_role(&test_db, hub, &o, r, target.character, "visitor").await.unwrap(),
                    Restriction::Ban => authority::set_affiliation(&test_db, hub, &o, r, target.character, "outcast").await.unwrap(),
                }
                set(&test_db, hub, &o, r, target.character, kind, 100).await.unwrap();
                authority::set_affiliation(&test_db, hub, &o, r, target.character, "admin").await.unwrap();
                match kind {
                    Restriction::Mute => assert_eq!(authority::admit(&test_db, sde, &t, r).await.unwrap().unwrap().role, "visitor"),
                    Restriction::Ban => assert!(authority::admit(&test_db, sde, &t, r).await.unwrap().is_none()),
                }
                let before = job(&test_db, r, target.character, kind).await.unwrap();
                let config = authority::room_config(&test_db, &o, r).await.unwrap();
                let metadata = dao::get(&test_db, r).await.unwrap().unwrap();
                assert!(authority::configure(&test_db, hub, &t, r, RoomConfig { display_name: format!("Forbidden {}", owner.character), public: false, moderated: true, members_by_default: true }).await.is_err());
                assert!(authority::room_config(&test_db, &t, r).await.is_err());
                assert!(authority::set_subject(&test_db, hub, &t, r, "forbidden").await.is_err());
                assert!(authority::set_affiliation(&test_db, hub, &t, r, victim.character, "outcast").await.is_err());
                assert!(authority::set_affiliation(&test_db, hub, &t, r, victim.character, "none").await.is_err());
                assert!(authority::set_role(&test_db, hub, &t, r, victim.character, "participant").await.is_err());
                assert!(authority::invite(&test_db, hub, &t, r, victim.character).await.is_err());
                assert!(authority::destroy(&test_db, hub, &t, r).await.is_err());
                assert!(set(&test_db, hub, &t, r, victim.character, Restriction::Mute, 100).await.is_err());
                assert!(remove(&test_db, hub, &t, r, target.character, kind).await.is_err());
                for search in [Restriction::Mute, Restriction::Ban] {
                    assert!(list(&test_db, &t, r, search).await.is_err());
                    assert!(list(&test_db, &o, r, search).await.is_ok());
                }
                let after = authority::room_config(&test_db, &o, r).await.unwrap();
                assert_eq!((after.display_name, after.public, after.moderated, after.members_by_default), (config.display_name, config.public, config.moderated, config.members_by_default));
                let unchanged = dao::get(&test_db, r).await.unwrap().unwrap();
                assert_eq!(unchanged.owner_id, metadata.owner_id);
                assert_eq!(unchanged.motd, metadata.motd);
                let restricted = dao::member(&test_db, r, target.character).await.unwrap().unwrap();
                match kind { Restriction::Mute => assert_eq!(restricted.voice_expires_at, Some(before.due_at)), Restriction::Ban => assert_eq!(restricted.ban_expires_at, Some(before.due_at)) }
                assert_eq!(job(&test_db, r, target.character, kind).await.unwrap().payload, before.payload);
                assert!(job(&test_db, r, victim.character, Restriction::Mute).await.is_none());
                let victim_row = dao::member(&test_db, r, victim.character).await.unwrap().unwrap();
                assert_eq!(victim_row.affiliation, dao::Affiliation::Member);
                assert_eq!(victim_row.voice, dao::Voice::Muted);
                assert!(!victim_row.invited);
                if via_due {
                    let due = seed_due(&test_db, r, target.character, kind, filetime_now() - 1).await;
                    handle(test_db.clone(), world.clone(), due.clone()).await.unwrap();
                    handle(test_db.clone(), world.clone(), due).await.unwrap();
                } else {
                    remove(&test_db, hub, &o, r, target.character, kind).await.unwrap();
                    handle(test_db.clone(), world.clone(), before).await.unwrap();
                }
                let restored = dao::member(&test_db, r, target.character).await.unwrap().unwrap();
                assert_eq!(restored.affiliation, dao::Affiliation::Admin);
                match kind { Restriction::Mute => assert!(restored.voice_expires_at.is_none()), Restriction::Ban => assert!(restored.ban_expires_at.is_none()) }
                authority::set_subject(&test_db, hub, &t, r, "restored").await.unwrap();
                if kind == Restriction::Mute && !via_due {
                    authority::set_affiliation(&test_db, hub, &o, r, target.character, "member").await.unwrap();
                    authority::set_role(&test_db, hub, &o, r, target.character, "visitor").await.unwrap();
                    authority::set_affiliation(&test_db, hub, &o, r, target.character, "admin").await.unwrap();
                    assert!(authority::room_config(&test_db, &t, r).await.is_err());
                    authority::set_affiliation(&test_db, hub, &o, r, target.character, "member").await.unwrap();
                    authority::set_role(&test_db, hub, &o, r, target.character, "participant").await.unwrap();
                    authority::set_affiliation(&test_db, hub, &o, r, target.character, "admin").await.unwrap();
                    authority::room_config(&test_db, &t, r).await.unwrap();
                }
                scheduler::cancel(&test_db, &job_id(r, target.character, kind)).await.unwrap();
            }
        }
    }).await;
    cleanup(&db, owner, target, &room).await;
    fixture::cleanup(&db, victim).await.unwrap();
    result.unwrap();
}
