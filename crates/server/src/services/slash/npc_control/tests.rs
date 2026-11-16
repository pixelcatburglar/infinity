//! `/invu` through the real slash route: persisted flag, live remote-owner
//! drone projection and the cold-load restore path, plus one grammar refusal.
use super::*;
use crate::services::drones::tests::fixture::Fixture;
use crate::services::slash::tests::support::rpc;
use infinity_core::consts::{CATEGORY_DRONE, GM_ROLES};
use marshal::Value;

fn text(result: crate::services::CallResult) -> String {
    match result.unwrap() { Value::Unicode(text) => text, other => panic!("expected unicode, got {other:?}") }
}
fn reason(error: crate::services::CallResult) -> String {
    match error.unwrap_err() {
        CallError::User { dict, .. } => match &dict[0].1 { Value::Unicode(text) => text.clone(), other => panic!("expected unicode reason, got {other:?}") },
        other => panic!("expected user error, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn invu_persists_and_reaches_live_and_cold_drones() {
    let mut f = Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
        f.client.login.role = GM_ROLES;
        let (db, world, owner) = (&f.db, &f.world, f.owner);
        let drone_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)).unwrap().type_id;
        let get = |expected| async move {
            assert_eq!(crate::dao::characters::npc_control::get(db, owner).await.unwrap(), expected);
        };
        let remote = |system, drone, expected| async move {
            assert_eq!(world.with(system, move |s| s.npc_target_ignored(drone)).await, Some(expected));
        };
        assert_eq!(text(rpc(&mut f.client, "/invu status").await), messages::gm::gm_invu_status(false));
        // Owner-orphaned drone cold-restored in another loaded system with the default flag.
        let live = f.system + 1;
        let orphan = crate::dao::items::creation::positioned(&f.db, drone_type, f.owner, live, [0.0; 3]).await.unwrap();
        f.world.system(live).await.unwrap();
        remote(live, orphan, false).await;
        assert_eq!(text(rpc(&mut f.client, "/invu on").await), messages::gm::GM_INVU_ENABLED);
        get(Some(true)).await;
        remote(live, orphan, true).await;
        // A system cold-loaded after the commit restores the persisted flag.
        let cold = f.system + 2;
        let restored = crate::dao::items::creation::positioned(&f.db, drone_type, f.owner, cold, [0.0; 3]).await.unwrap();
        f.world.system(cold).await.unwrap();
        remote(cold, restored, true).await;
        assert_eq!(text(rpc(&mut f.client, "/invu").await), messages::gm::GM_INVU_DISABLED);
        get(Some(false)).await;
        remote(live, orphan, false).await;
        assert_eq!(reason(rpc(&mut f.client, "/invu bogus").await), messages::gm::GM_INVU_USAGE);
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
