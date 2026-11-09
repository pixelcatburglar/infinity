//! Real Hub destruct, transfer and restart recovery scenarios.
//! Durable jobs use the scheduler handler; overdue deadlines are fixture state,
//! not a replacement clock or a simulated successful detonation.
use crate::{
    client::filetime_now,
    dao::{jobs as job_dao, sovereignty::{capture, policy, sources, structures}},
    lua::content::{PolicyInput, PolicyPlan, Rule},
    scheduler::{self, Job},
    services::sovereignty::{deploy::{self, DeployRequest}, jobs::{self, DestructPayload}, lifecycle, recovery},
};
use super::Fixture;

async fn job(f: &Fixture, item: i64) -> Job {
    job_dao::lock_job(&f.resources.db, &jobs::destruct_id(item)).await.unwrap().unwrap()
}

async fn handle(f: &Fixture, job: Job) {
    (jobs::registration().handler)(f.resources.db.clone(), f.resources.world.clone(), job).await.unwrap();
}

// Replace the request's persisted deadline and its matching scheduler payload.
// This is deliberately overdue fixture state, while all production guards remain active.
async fn overdue(f: &Fixture, item: i64) -> Job {
    let db = &f.resources.db;
    let original = job(f, item).await;
    let mut payload: DestructPayload = serde_json::from_str(&original.payload).unwrap();
    let previous = payload.due;
    payload.due = filetime_now() - 1;
    assert!(structures::set_destruct(db, item, Some(previous), Some(f.ids.characters[usize::from(payload.system == f.ids.systems[1])]), Some(payload.due)).await.unwrap());
    let replacement = Job { due_at: payload.due, payload: serde_json::to_string(&payload).unwrap(), ..original };
    scheduler::schedule(db, &replacement).await.unwrap();
    replacement
}

async fn count(f: &Fixture, receiver: i64, kind: i32) -> i64 {
    f.resources.db.query_one("SELECT count(*) AS n FROM game.notifications WHERE receiver_id=$1 AND type_id=$2", &[&receiver, &kind]).await.unwrap().get("n")
}

pub(super) async fn run(f: &Fixture, mut hubs: [i64; 2]) -> [i64; 2] {
    let ids = f.ids;
    let r = &f.resources;
    for i in 0..2 {
        let hub = super::snapshot(f, ids.systems[i]).await;
        f.install_pilot(i, i, [hub.x.unwrap(), hub.y.unwrap(), hub.z.unwrap()]).await;
    }
    let initial = super::snapshot(f, ids.systems[0]).await;
    assert!(crate::lua::retained_content(&initial.structure.content_namespace, &initial.structure.content_version).await.is_ok());
    assert!(crate::lua::retained_content(&initial.structure.content_namespace, "missing-fixture-version").await.is_err());
    assert_eq!(super::snapshot(f, ids.systems[0]).await, initial);

    // The request timestamp is the history timestamp used by the production policy.
    let notices = count(f, ids.characters[0], 167).await;
    lifecycle::destruct(r.clone(), f.identity(0), hubs[0], true).await.unwrap();
    let requested = super::snapshot(f, ids.systems[0]).await;
    assert!(!r.world.is_online(ids.characters[0]));
    let due = requested.structure.self_destruct_due_at.unwrap();
    let at: i64 = r.db.query_one("SELECT at FROM game.sov_history WHERE hub_item_id=$1 AND reason='destruct_requested' ORDER BY history_id DESC LIMIT 1", &[&hubs[0]]).await.unwrap().get("at");
    let pin = crate::lua::content_policy("sovereignty").unwrap();
    let plan = pin.evaluate(Rule::SelfDestruct, &PolicyInput { now: at, system: ids.systems[0], current_system: None, first_appointment: None, last_loss: None, not_before: None, owned_count: 0 }).unwrap();
    assert!(matches!(plan, PolicyPlan::SelfDestruct { due: expected } if expected == due));
    let first_job = job(f, hubs[0]).await;
    let first_payload: DestructPayload = serde_json::from_str(&first_job.payload).unwrap();
    assert_eq!((first_payload.item, first_payload.system, first_payload.due), (hubs[0], ids.systems[0], due));
    assert_eq!(count(f, ids.characters[0], 167).await, notices + 1);
    lifecycle::destruct(r.clone(), f.identity(0), hubs[0], true).await.unwrap();
    assert_eq!(count(f, ids.characters[0], 167).await, notices + 1);
    lifecycle::destruct(r.clone(), f.identity(0), hubs[0], false).await.unwrap();
    assert!(job_dao::lock_job(&r.db, &jobs::destruct_id(hubs[0])).await.unwrap().is_none());
    assert_eq!(super::snapshot(f, ids.systems[0]).await.structure.self_destruct_due_at, None);

    // Transfer cancels the live request; a previously leased matching overdue payload
    // is then delivered to the real handler after ownership has changed.
    lifecycle::destruct(r.clone(), f.identity(0), hubs[0], true).await.unwrap();
    let stale = overdue(f, hubs[0]).await;
    lifecycle::transfer(r.clone(), f.identity(0), hubs[0], ids.corps[0], ids.corps[1]).await.unwrap();
    let transferred = super::snapshot(f, ids.systems[0]).await;
    assert_eq!(transferred.owner_id, ids.corps[1]);
    assert_eq!(transferred.structure.self_destruct_due_at, None);

    assert!(job_dao::lock_job(&r.db, &jobs::destruct_id(hubs[0])).await.unwrap().is_none());
    handle(f, stale).await;
    assert_eq!(super::snapshot(f, ids.systems[0]).await, transferred);
    f.install_pilot(1, 0, [transferred.x.unwrap(), transferred.y.unwrap(), transferred.z.unwrap()]).await;
    lifecycle::transfer(r.clone(), f.identity(1), hubs[0], ids.corps[1], ids.corps[0]).await.unwrap();
    let second = super::snapshot(f, ids.systems[1]).await;
    f.install_pilot(1, 1, [second.x.unwrap(), second.y.unwrap(), second.z.unwrap()]).await;
    // Poll the public future once while market is held. Its first poll starts
    // the independently owned operation; dropping the caller cannot cancel it.
    let market = crate::services::market::lock().await;
    {
        let operation = lifecycle::destruct(r.clone(), f.identity(0), hubs[0], true);
        tokio::pin!(operation);
        tokio::select! {
            biased;
            result = &mut operation => panic!("operation unexpectedly completed under market lock: {result:?}"),
            _ = tokio::task::yield_now() => {}
        }
    }
    drop(market);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if structures::read(&r.db, hubs[0]).await.unwrap().unwrap().self_destruct_due_at.is_some() { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("caller-dropped destruct did not commit");
    lifecycle::destruct(r.clone(), f.identity(0), hubs[0], false).await.unwrap();

    // Recovery scans the dedicated domain, including a claimed overdue job.
    lifecycle::destruct(r.clone(), f.identity(0), hubs[0], true).await.unwrap();
    let stale = overdue(f, hubs[0]).await;
    let token: i64 = r.db.query_one("UPDATE game.jobs SET state='running', lease_token=nextval('game.jobs_lease_seq'), lease_until=$2 WHERE job_id=$1 RETURNING lease_token", &[&stale.id, &filetime_now()]).await.unwrap().get("lease_token");
    let mut target = capture::read(&r.db, hubs[0]).await.unwrap().unwrap();
    target.score = 0.5;
    target.rate = 0.25;
    target.scoring_team = Some(ids.alliances[0]);
    assert!(capture::checkpoint(&r.db, target.revision, &target).await.unwrap());
    let before_target = capture::read(&r.db, hubs[0]).await.unwrap().unwrap();
    let versions = sources::versions(&r.db, &ids.characters).await.unwrap();
    let capital_notices = count(f, ids.characters[0], 165).await;
    let cancelled = count(f, ids.characters[0], 168).await;
    let lower = filetime_now();
    recovery::recover(r).await.unwrap();
    let upper = filetime_now();
    crate::space::sovereignty::open(&r.world);
    let after_target = capture::read(&r.db, hubs[0]).await.unwrap().unwrap();
    assert_eq!(after_target.score, before_target.score);
    assert_eq!(after_target.rate, 0.0);
    assert_eq!(after_target.scoring_team, None);
    assert!(!after_target.blocked);
    assert_eq!(after_target.revision, before_target.revision + 1);
    assert!((lower..=upper).contains(&after_target.checkpoint_wall_time));
    assert_eq!(after_target.score_sim_time, after_target.checkpoint_wall_time);
    let newer = sources::versions(&r.db, &ids.characters).await.unwrap();
    for (old, new) in versions.iter().zip(&newer) { assert_eq!(new.revision, old.revision + 1); }
    assert_eq!(count(f, ids.characters[0], 168).await, cancelled + 1);
    assert_eq!(count(f, ids.characters[0], 165).await, capital_notices);
    assert_eq!(job_dao::ack(&r.db, &stale.id, token).await.unwrap(), 0);
    let recovered = super::snapshot(f, ids.systems[0]).await;
    assert_eq!(recovered.structure.self_destruct_due_at, None);
    handle(f, stale).await;
    assert_eq!(super::snapshot(f, ids.systems[0]).await, recovered);

    // Fixture capital placement makes the destruct exercise a real capital loss;
    // ordinary capital changes are not implemented by the service.
    let mut capital = policy::read(&r.db, ids.alliances[0]).await.unwrap().unwrap();
    assert_eq!(capital.current_capital_system, Some(ids.systems[0]));
    capital.current_capital_system = Some(ids.systems[1]);
    assert!(policy::replace(&r.db, capital.revision, &capital).await.unwrap());
    let capital_notices = count(f, ids.characters[0], 165).await;
    // The remaining source is deployed and claimed after the real detonation.
    lifecycle::destruct(r.clone(), f.identity(1), hubs[1], true).await.unwrap();
    let final_job = overdue(f, hubs[1]).await;
    let finished_notices = count(f, ids.characters[1], 169).await;
    handle(f, final_job.clone()).await;
    assert_eq!(policy::read(&r.db, ids.alliances[0]).await.unwrap().unwrap().current_capital_system, None);
    assert_eq!(count(f, ids.characters[0], 165).await, capital_notices);
    assert!(structures::read(&r.db, hubs[1]).await.unwrap().is_none());
    assert!(capture::read(&r.db, hubs[1]).await.unwrap().is_none());
    assert_eq!(count(f, ids.characters[1], 169).await, finished_notices + 1);
    handle(f, final_job).await;
    assert_eq!(count(f, ids.characters[1], 169).await, finished_notices + 1);
    scheduler::cancel(&r.db, &jobs::destruct_id(hubs[1])).await.unwrap();
    hubs[1] = deploy::deploy(r.clone(), f.identity(1), DeployRequest { source_item_id: ids.sources[1], quantity: 1, whose_behalf: None }).await.unwrap().item_id;
    super::claim_for(f, hubs[1], 1).await;
    for i in 0..2 {
        let hub = super::snapshot(f, ids.systems[i]).await;
        assert_eq!(hub.structure.item_id, hubs[i]);
        assert_eq!(hub.owner_id, ids.corps[i]);
        assert_eq!(hub.system.claim_corporation_id, Some(ids.corps[i]));
        assert_eq!(hub.structure.self_destruct_due_at, None);
        assert!(job_dao::lock_job(&r.db, &jobs::destruct_id(hubs[i])).await.unwrap().is_none());
    }
    hubs
}
