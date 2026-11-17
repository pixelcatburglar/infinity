//! Database scenario for bulk permanent grants: rollback through the shared
//! transaction body, then the real slash route with live target notifications,
//! an idempotent repeat and an offline catch-up grant.
use super::*;
use crate::services::{slash::tests::support, tests::fixture};
use infinity_core::session::OnlineSession;

fn job_id(character: i64) -> String { format!("{}:{character}", expiry::KIND) }
async fn stored(db: &Db, character: i64, skin: i32) -> Option<Option<i64>> {
    ship_skins::get_stored_license(db, character, skin).await.unwrap().map(|row| row.expires_at)
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn grants_the_entire_set_once_and_catches_up_offline() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let gm = fixture::create(&db).await.unwrap();
    let target = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = crate::services::tests::client(test_db.clone(), gm.character).await;
        c.login.role = infinity_core::consts::GM_ROLES;
        let (mut tc, _tpeer) = crate::services::tests::client(test_db.clone(), target.character).await;
        let region = c.cfg.server.boot_region.clone();
        let candidates: Vec<i32> = c.sde.ship_skins.ordinary_skins(&region).map(|skin| skin.skin_id).collect();
        assert!(candidates.len() >= 4, "dedicated SDE must expose several ordinary SKINs");
        let now = filetime_now();
        for (skin, expiry) in [(candidates[0], None), (candidates[1], Some(now + infinity_core::consts::FILETIME_DAY)), (candidates[2], Some(now - 1))] {
            assert!(ship_skins::insert_license(&test_db, target.character, skin, expiry).await.unwrap());
        }
        assert!(support::rpc(&mut tc, "/allskins me").await.is_err(), "a non-GM caller changes nothing");
        assert_eq!(ship_skins::licensed(&test_db, target.character, now).await.unwrap().len(), 2);
        let mut rx = support::register_online(&c.world, OnlineSession::from_session(&tc.session).unwrap());
        let mut gm_rx = support::register_online(&c.world, OnlineSession::from_session(&c.session).unwrap());
        // The shared transaction body runs to its final schedule step; the forced
        // error rolls every grant, the expiry cleanup and the job write back.
        let targets = expiry::prepare(&test_db, &c.world, target.character).await.unwrap();
        let (sde, world) = (c.sde.clone(), c.world.clone());
        let outcome: Result<(Summary, bool), CallError> = test_db.transaction(move |tx, outbox: &mut Outbox| {
            let (sde, region, targets) = (sde.clone(), region.clone(), targets.clone());
            Box::pin(async move {
                grant_tx(tx, &sde, &region, target.character, now, &targets, outbox).await?;
                Err(CallError::Internal(anyhow::anyhow!("forced rollback")))
            })
        }, |value, outbox| async move { dispatch_world(value, outbox, &world) }).await;
        assert!(outcome.is_err());
        assert_eq!(stored(&test_db, target.character, candidates[0]).await, Some(None));
        assert!(matches!(stored(&test_db, target.character, candidates[1]).await, Some(Some(_))));
        assert_eq!(stored(&test_db, target.character, candidates[2]).await, Some(Some(now - 1)));
        assert_eq!(stored(&test_db, target.character, candidates[3]).await, None);
        assert!(crate::dao::jobs::lock_job(&test_db, &job_id(target.character)).await.unwrap().is_none());
        assert!(rx.try_recv().is_err() && gm_rx.try_recv().is_err());
        // The real slash route grants every remaining candidate permanently.
        let reply = support::rpc(&mut c, &format!("/allskins {}", target.character)).await.unwrap();
        assert_eq!(reply.as_text().unwrap(), infinity_core::messages::gm::gm_all_skins_done(target.character, candidates.len() as i64 - 1, 1));
        let mut notified = Vec::new();
        while let Ok((event, args)) = rx.try_recv() {
            assert_eq!(event, "OnSkinLicenseActivated");
            assert_eq!(args[1], Value::Int(target.character));
            notified.push(match &args[0] { Value::Int(skin) => *skin as i32, _ => panic!("skin id") });
        }
        notified.sort_unstable();
        assert_eq!(notified, candidates[1..], "the permanent row is skipped without a notice");
        assert!(gm_rx.try_recv().is_err());
        let args: [Value; 0] = []; let kwargs = Value::Dict(vec![]);
        let call = crate::services::Call { service: "shipCosmeticsMgr", method: "GetLicencedSkins", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let Ok(Value::List(rows)) = crate::services::dispatch(&mut tc, &call).await else { panic!("license list") };
        let mut owned = Vec::new();
        for row in &rows {
            let Value::Instance(_, state) = row else { panic!("keyval row") };
            assert_eq!(state.get("expires"), Some(&Value::None));
            assert_eq!(state.get("isSingleUse"), Some(&Value::Bool(false)));
            owned.push(match state.get("skinID") { Some(Value::Int(id)) => *id as i32, _ => panic!("skinID") });
        }
        owned.sort_unstable();
        assert_eq!(owned, candidates);
        assert!(ship_skins::licensed(&test_db, gm.character, filetime_now()).await.unwrap().is_empty());
        let reply = support::rpc(&mut c, &format!("/allskins {}", target.character)).await.unwrap();
        assert_eq!(reply.as_text().unwrap(), infinity_core::messages::gm::gm_all_skins_done(target.character, 0, candidates.len() as i64));
        assert!(rx.try_recv().is_err() && crate::dao::jobs::lock_job(&test_db, &job_id(target.character)).await.unwrap().is_none());
        c.world.unregister_pilot_generation(target.character, None);
        assert!(ship_skins::remove_license(&test_db, target.character, candidates[1], None).await.unwrap().is_some());
        let name: String = test_db.query_one("SELECT name FROM game.characters WHERE character_id=$1", &[&target.character]).await.unwrap().get("name");
        let reply = support::rpc(&mut c, &format!("/allskins \"{name}\"")).await.unwrap();
        assert_eq!(reply.as_text().unwrap(), infinity_core::messages::gm::gm_all_skins_done(target.character, 1, candidates.len() as i64 - 1));
        assert_eq!(stored(&test_db, target.character, candidates[1]).await, Some(None));
        assert_eq!(ship_skins::licensed(&test_db, target.character, filetime_now()).await.unwrap().len(), candidates.len());
        assert!(rx.try_recv().is_err());
    }).await;
    crate::scheduler::cancel(&db, &job_id(target.character)).await.unwrap();
    fixture::cleanup(&db, gm).await.unwrap();
    fixture::cleanup(&db, target).await.unwrap();
    result.unwrap();
}
