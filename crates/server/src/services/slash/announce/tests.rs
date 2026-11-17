use super::*;
use crate::services::slash::tests::support::{register_online, rpc, wire};
use crate::services::tests::{client, fixture};
use infinity_core::{consts::GM_ROLES, session::OnlineSession};
use tokio::sync::mpsc::error::TryRecvError;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn announce_targets_current_online_pilots_without_backfill() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let mut fixtures = Vec::new();
    let result = crate::tests::catch_scenario(async {
        fixtures.push(fixture::create(&db).await.unwrap());
        fixtures.push(fixture::create(&db).await.unwrap());
        let (mut c, _peer) = client(db.clone(), fixtures[0].character).await;
        c.login.role = GM_ROLES;
        assert_eq!(wire(rpc(&mut c, "/announce nobody").await), marshal::dumps(&Value::unicode(messages::gm::gm_announce_queued(0))));
        let session = |id| OnlineSession { character_id: id, user_id: None, session_id: id, ship_id: None, station_id: None, system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 };
        let (first, second) = (fixtures[0].character, fixtures[1].character);
        let mut old = register_online(&c.world, session(first));
        let mut a = register_online(&c.world, session(first));
        let mut b = register_online(&c.world, session(second));
        assert_eq!(wire(rpc(&mut c, "/announce \"hello world\" again").await), marshal::dumps(&Value::unicode(messages::gm::gm_announce_queued(2))));
        let notice = |text| ("OnRemoteMessage".to_string(), vec![Value::str(messages::MSG_CUSTOM_NOTIFY), Value::dict([("notify", Value::unicode(messages::gm::gm_announcement(text)))])]);
        assert_eq!(a.try_recv().unwrap(), notice("hello world again"));
        assert_eq!(b.try_recv().unwrap(), notice("hello world again"));
        assert_eq!(a.try_recv(), Err(TryRecvError::Empty)); assert_eq!(b.try_recv(), Err(TryRecvError::Empty)); assert!(old.try_recv().is_err());
        b.close(); c.world.unregister_pilot_generation(second, None);
        assert_eq!(wire(rpc(&mut c, "/announce next").await), marshal::dumps(&Value::unicode(messages::gm::gm_announce_queued(1))));
        assert_eq!(a.try_recv().unwrap(), notice("next")); assert_eq!(a.try_recv(), Err(TryRecvError::Empty));
        let mut b = register_online(&c.world, session(second));
        assert_eq!(b.try_recv(), Err(TryRecvError::Empty)); assert!(old.try_recv().is_err());
        assert_eq!(wire(rpc(&mut c, "/announce \"   \"").await), wire(Err(slash_error(messages::gm::GM_ANNOUNCE_USAGE.into()))));
        assert_eq!(a.try_recv(), Err(TryRecvError::Empty)); assert_eq!(b.try_recv(), Err(TryRecvError::Empty));
        c.login.role = 0;
        assert_eq!(wire(rpc(&mut c, "/announce denied").await), wire(Err(slash_error(messages::gm::GM_ROLE_REQUIRED.into()))));
        assert_eq!(a.try_recv(), Err(TryRecvError::Empty)); assert_eq!(b.try_recv(), Err(TryRecvError::Empty));
    }).await;
    for f in fixtures { fixture::cleanup(&db, f).await.unwrap(); }
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
