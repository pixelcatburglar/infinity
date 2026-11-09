//! Real deployment, typed settlement and corporation-governance scenarios.
//! Synthetic endpoint observations exercise settlement guards, not Entosis warmup or scoring.
mod recovery;

use std::sync::Arc;

use infinity_core::{
    consts::{GROUP_SUN, OWNER_DED},
};
use crate::{
    client::filetime_now,
    dao::{items::loading::{self, ItemQuery}, sovereignty::{capture, policy, sources, structures, systems}},
    db::outbox::{dispatch_world, Outbox},
    services::{alliances, market, sovereignty::{deploy::{self, DeployRequest}, governance, lifecycle::{self, CaptureOutcome}, CaptureContribution, CaptureContributor, CaptureObservation}},
};
use super::fixture::Fixture;

async fn snapshot(f: &Fixture, system: i64) -> structures::HubRow {
    let rows = structures::snapshots(&f.resources.db, Some(system), None).await.unwrap();
    assert_eq!(rows.len(), 1);
    rows.into_iter().next().unwrap()
}

async fn item(f: &Fixture, id: i64) -> loading::GameItem {
    loading::load_items(&f.resources.db, ItemQuery::Id(id)).await.unwrap().into_iter().next().unwrap()
}

fn position(f: &Fixture, index: usize) -> [f64; 3] {
    let sde = &f.resources.sde;
    let star = sde.map.celestials(f.ids.systems[index]).find(|c| c.group_id == Some(i64::from(GROUP_SUN))).unwrap();
    [star.x.unwrap() + sde.sovereignty.hub().deploy.fixed_star_distance_m as f64, star.y.unwrap(), star.z.unwrap()]
}

async fn observation(f: &Fixture, system_index: usize, pilot: usize) -> CaptureObservation {
    let db = &f.resources.db;
    let system = systems::read(db, f.ids.systems[system_index]).await.unwrap().unwrap();
    let mut target = capture::for_system(db, system.system_id).await.unwrap().unwrap();
    let alliance = f.ids.alliances[usize::from(pilot == 2)];
    target.controlling_team = Some(alliance);
    target.scoring_team = Some(alliance);
    target.score = 1.0;
    target.rate = 1.0;
    target.blocked = false;
    let version = sources::versions(db, &[f.ids.characters[pilot]]).await.unwrap().pop().unwrap();
    let contributor = CaptureContributor { character_id: f.ids.characters[pilot], ship_id: f.ids.ships[pilot], module_id: f.ids.modules[pilot], corporation_id: f.ids.corps[pilot], alliance_id: alliance, source_revision: version.revision };
    CaptureObservation {
        contributors: vec![CaptureContribution { contributor: contributor.clone(), through: target.score_sim_time }],
        claimant: Some(contributor), target, system_revision: system.revision,
    }
}

/// Exercise settlement from a trusted endpoint; this does not test link scoring.
pub(super) async fn claim_for(f: &Fixture, hub: i64, pilot: usize) {
    let structure = structures::read(&f.resources.db, hub).await.unwrap().unwrap();
    let index = f.ids.systems.iter().position(|&system| system == structure.system_id).unwrap();
    let obs = observation(f, index, pilot).await;
    assert_eq!(obs.target.target_item_id, hub);
    assert!(matches!(lifecycle::claim(f.resources.clone(), obs).await.unwrap(), CaptureOutcome::Claimed));
    assert_eq!(snapshot(f, structure.system_id).await.owner_id, f.ids.corps[pilot]);
}
async fn notices(f: &Fixture, character: i64, kind: i32) -> Vec<serde_json::Value> {
    let rows = f.resources.db.query("SELECT sender_id,group_id,data FROM game.notifications WHERE receiver_id=$1 AND type_id=$2 ORDER BY notification_id", &[&character, &kind]).await.unwrap();

    rows.into_iter().map(|r| {
        assert_eq!(r.get::<_, i64>("sender_id"), OWNER_DED);
        assert_eq!(r.get::<_, i32>("group_id"), 6);
        serde_json::from_str(r.get::<_, &str>("data")).unwrap()
    }).collect()
}

pub(super) async fn run(f: &Fixture) {
    let ids = f.ids;
    let r = &f.resources;
    // Refusals must not consume either source stack.
    for request in [
        DeployRequest { source_item_id: ids.sources[0], quantity: 0, whose_behalf: None },
        DeployRequest { source_item_id: ids.sources[0], quantity: 2, whose_behalf: None },
        DeployRequest { source_item_id: ids.sources[0], quantity: 1, whose_behalf: Some(ids.characters[1]) },
        DeployRequest { source_item_id: ids.sources[1], quantity: 1, whose_behalf: None },
    ] {
        assert!(deploy::deploy(r.clone(), f.identity(0), request).await.is_err());
    }
    assert_eq!(item(f, ids.sources[0]).await.quantity, 1);
    assert_eq!(item(f, ids.sources[1]).await.quantity, 2);
    let requests = ids.sources.map(|source_item_id| DeployRequest { source_item_id, quantity: 1, whose_behalf: None });
    let hub0 = deploy::deploy(r.clone(), f.identity(0), requests[0]).await.unwrap().item_id;
    let hub1 = deploy::deploy(r.clone(), f.identity(1), requests[1]).await.unwrap().item_id;
    assert_eq!(hub0, ids.sources[0]);
    assert_ne!(hub1, ids.sources[1]);
    assert_eq!(item(f, ids.sources[1]).await.quantity, 1);
    assert_eq!(deploy::deploy(r.clone(), f.identity(0), requests[0]).await.unwrap().item_id, hub0);
    assert!(deploy::deploy(r.clone(), f.identity(1), requests[0]).await.is_err());
    for (i, hub) in [hub0, hub1].into_iter().enumerate() {
        let row = snapshot(f, ids.systems[i]).await;
        assert_eq!(row.structure.item_id, hub);
        assert_eq!(row.structure.deployer_character_id, ids.characters[i]);
        assert_eq!(row.structure.deployer_corporation_id, ids.corps[i]);
        assert_eq!(row.owner_id, r.sde.sovereignty.hub().deploy.default_owner);
        assert_eq!(row.system.claim_corporation_id, None);
        assert_eq!(row.system.claim_time, None);
        assert_eq!([row.x.unwrap(), row.y.unwrap(), row.z.unwrap()], position(f, i));
        assert_eq!(row.structure.self_destruct_due_at, None);
    }

    // Stale typed endpoint: actual generation and immutable invalidation history.
    let stale = observation(f, 0, 0).await;
    let before = snapshot(f, ids.systems[0]).await;
    sources::invalidate(&r.db, &[ids.characters[0]], filetime_now()).await.unwrap();
    assert!(matches!(lifecycle::claim(r.clone(), stale).await.unwrap(), CaptureOutcome::Rejected { .. }));
    assert_eq!(snapshot(f, ids.systems[0]).await, before);
    assert!(notices(f, ids.characters[0], 44).await.is_empty());
    let mut first_capital = None;
    let authored: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/lua/content/sovereignty/data/policy.json"))).unwrap();
    let cooldown = crate::client::filetime_ticks_from_seconds(authored["capital_cooldown_seconds"].as_i64().unwrap());
    for i in 0..2 {
        let obs = observation(f, i, i).await;
        assert!(matches!(lifecycle::claim(r.clone(), obs).await.unwrap(), CaptureOutcome::Claimed));
        let row = snapshot(f, ids.systems[i]).await;
        assert_eq!(row.owner_id, ids.corps[i]);
        assert_eq!(row.system.alliance_id, Some(ids.alliances[0]));
        assert!(row.system.claim_time.is_some());
        assert_eq!(row.system.ownership_epoch, 1);
        let acquired = notices(f, ids.characters[i], 44).await;
        assert_eq!(acquired, vec![serde_json::json!({"solarSystemID":ids.systems[i],"corpID":ids.corps[i],"allianceID":ids.alliances[0]})]);
        let capital = policy::read(&r.db, ids.alliances[0]).await.unwrap().unwrap();
        assert_eq!(capital.current_capital_system, Some(ids.systems[0]));
        assert!(capital.capital_change_not_before.is_some());
        if let Some(first) = &first_capital {
            assert_eq!(&capital, first);
        } else {
            assert_eq!(capital.capital_change_not_before, Some(capital.last_capital_changed_at.unwrap() + cooldown));
            first_capital = Some(capital);
        }
    }

    let [hub0, hub1] = recovery::run(f, [hub0, hub1]).await;
    let before = snapshot(f, ids.systems[1]).await;
    let p = [before.x.unwrap(), before.y.unwrap(), before.z.unwrap()];
    f.install_pilot(1, 1, p).await;
    f.install_pilot(2, 1, p).await;
    assert!(lifecycle::transfer(r.clone(), f.identity(2), hub1, ids.corps[1], ids.corps[0]).await.is_err());
    assert!(lifecycle::transfer(r.clone(), f.identity(1), hub1, ids.corps[0], ids.corps[1]).await.is_err());
    assert!(lifecycle::transfer(r.clone(), f.identity(1), hub1, ids.corps[1], ids.corps[2]).await.is_err());
    assert_eq!(snapshot(f, ids.systems[1]).await, before);
    lifecycle::transfer(r.clone(), f.identity(1), hub1, ids.corps[1], ids.corps[0]).await.unwrap();
    let after = snapshot(f, ids.systems[1]).await;
    assert_eq!(after.owner_id, ids.corps[0]);
    assert_eq!(after.system.ownership_epoch, before.system.ownership_epoch);
    assert_eq!(after.system.claim_time, before.system.claim_time);
    assert_eq!((after.x, after.y, after.z), (before.x, before.y, before.z));
    assert_eq!(snapshot(f, ids.systems[0]).await.structure.item_id, hub0);

    // Prepare before taking the market lock; the no-Hub transaction must not acquire it.
    let no_hub_now = filetime_now();
    let prepared = governance::prepare(&r.db, ids.corps[1], no_hub_now).await.unwrap();
    assert!(matches!(prepared, governance::PreparedAllianceChange::NoOwnershipChange));
    let guard = market::lock().await;
    let admitted = Arc::new(governance::admit(&r.world, prepared, Some(ids.characters[1])).unwrap());
    tokio::time::timeout(std::time::Duration::from_secs(5), r.db.transaction(|tx, outbox: &mut Outbox| {
        let admitted = Arc::clone(&admitted);
        Box::pin(async move { alliances::set_alliance(tx, outbox, ids.corps[1], None, no_hub_now, &admitted).await })
    }, |value, outbox| async move { dispatch_world(value, outbox, &r.world) })).await.expect("no-Hub change acquired market").unwrap();
    drop(guard);
    assert_eq!(systems::for_corporation(&r.db, ids.corps[0]).await.unwrap().len(), 2);

    // Restart the facade: both the cold load and subsequent publications use this World.
    crate::space::sovereignty::close(&r.world);
    let live = crate::space::World::new(r.db.clone(), r.sde.clone(), 16);
    crate::space::sovereignty::open(&live);
    live.system(ids.systems[0]).await.unwrap();

    // A forced failure after the real organization write rolls back both Hubs.
    let now = filetime_now();
    let prepared = governance::prepare(&r.db, ids.corps[0], now).await.unwrap();
    let _guard = market::lock().await;
    let mut cold = Box::pin(live.system(ids.systems[1]));
    std::future::poll_fn(|cx| {
        use std::future::Future;
        assert!(cold.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    }).await;
    let admitted = Arc::new(governance::admit(&live, prepared, Some(ids.characters[0])).unwrap());
    let before0 = snapshot(f, ids.systems[0]).await;
    let before1 = snapshot(f, ids.systems[1]).await;
    let capital_before = policy::read(&r.db, ids.alliances[0]).await.unwrap().unwrap();
    let versions_before = sources::versions(&r.db, &[ids.characters[0]]).await.unwrap();
    let losses_before = notices(f, ids.characters[0], 42).await;
    let result: Result<(), crate::services::CallError> = r.db.transaction(|tx, outbox: &mut Outbox| {
        let admitted = Arc::clone(&admitted);
        Box::pin(async move {
            alliances::set_alliance(tx, outbox, ids.corps[0], None, now, &admitted).await?;
            Err(crate::services::db_err(anyhow::anyhow!("intentional transaction rollback")))
        })
    }, |value, outbox| { let world = &live; async move { dispatch_world(value, outbox, world) } }).await;
    assert!(result.is_err());
    assert_eq!(snapshot(f, ids.systems[0]).await, before0);
    assert_eq!(snapshot(f, ids.systems[1]).await, before1);
    assert_eq!(policy::read(&r.db, ids.alliances[0]).await.unwrap().unwrap(), capital_before);
    assert_eq!(sources::versions(&r.db, &[ids.characters[0]]).await.unwrap(), versions_before);
    assert_eq!(notices(f, ids.characters[0], 42).await, losses_before);
    assert_eq!(crate::dao::corporations::registry::corporation(&r.db, ids.corps[0]).await.unwrap().unwrap().alliance_id, Some(ids.alliances[0]));
    r.db.transaction(|tx, outbox: &mut Outbox| {
        let admitted = Arc::clone(&admitted);
        Box::pin(async move { alliances::set_alliance(tx, outbox, ids.corps[0], None, now, &admitted).await })
    }, |value, outbox| { let world = &live; async move { dispatch_world(value, outbox, world) } }).await.unwrap();
    drop(_guard);
    cold.await.unwrap();
    // Mailbox delivery queues the live Hub update for the next destiny boundary.
    // Wait for the actual owner, not merely for the publication callback.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            assert!(live.after(ids.systems[0], 0).await, "live system stopped");
            let owner = live.with(ids.systems[0], move |s| s.ball(hub0).map(|ball| ball.owner_id)).await.flatten();
            if owner == Some(r.sde.sovereignty.hub().deploy.default_owner) { break; }
        }
    }).await.expect("live Hub ownership did not reach the committed projection");
    for (system, hub) in [(ids.systems[0], hub0), (ids.systems[1], hub1)] {
        let owner = live.with(system, move |s| s.ball(hub).map(|ball| ball.owner_id)).await.flatten();
        assert_eq!(owner, Some(r.sde.sovereignty.hub().deploy.default_owner));
    }
    crate::space::sovereignty::close(&live);
    let capital_after = policy::read(&r.db, ids.alliances[0]).await.unwrap().unwrap();
    assert_eq!(capital_after.current_capital_system, None);
    assert!(capital_after.capital_lost_at.is_some());
    assert!(capital_after.capital_change_not_before >= capital_before.capital_change_not_before);
    let losses = notices(f, ids.characters[0], 42).await;
    assert_eq!(losses.len(), losses_before.len() + 2);
    for system in ids.systems {
        assert_eq!(losses.iter().filter(|payload| **payload == serde_json::json!({"solarSystemID":system})).count(), losses_before.iter().filter(|payload| **payload == serde_json::json!({"solarSystemID":system})).count() + 1);
    }
    for (index, old) in [before0, before1].into_iter().enumerate() {
        let row = snapshot(f, ids.systems[index]).await;
        assert_eq!(row.owner_id, r.sde.sovereignty.hub().deploy.default_owner);
        assert_eq!(row.system.claim_corporation_id, None);
        assert_eq!(row.system.alliance_id, None);
        assert_eq!(row.system.ownership_epoch, old.system.ownership_epoch + 1);
    }
}
