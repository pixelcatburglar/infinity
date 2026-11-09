//! Space actor visibility and selected-pilot dirt RPC checks.
//! The ship client supplies IDs; fixtures use game items and actor balls.
//! No real client UI is exercised.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn dirt_space_visible_foreign_read_and_pilot_only_reset() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let d = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(d.clone(), owner.character).await;
        c.login.user_id = owner.account;
        let system = c.sde.map.stations().find_map(|s| s.solar_system_id).unwrap();
        let hull = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
        let own = crate::dao::items::creation::positioned(&d, hull, owner.character, system, [0.0; 3]).await.unwrap();
        let other = crate::dao::items::creation::positioned(&d, hull, foreign.character, system, [0.0; 3]).await.unwrap();
        crate::dao::characters::selection::set_ship(&d, owner.character, own).await.unwrap();
        crate::dao::characters::ship::undock(&d, owner.character, system).await.unwrap();
        c.session.apply(vec![("shipid", Value::Int(own)), ("stationid", Value::None), ("solarsystemid", Value::Int(system))]);
        let bind = Value::tuple([Value::Int(system), Value::Int(infinity_core::consts::GROUP_SOLAR_SYSTEM)]);
        c.world.system(system).await.unwrap();
        let mut pilot = ship(); pilot.id = own; pilot.owner_id = owner.character; pilot.char_id = Some(owner.character); pilot.type_id = hull;
        let mut target = ship(); target.id = other; target.owner_id = foreign.character; target.char_id = None; target.type_id = hull;
        c.world.with(system, move |s| { s.insert(pilot); s.insert(target); }).await.unwrap();
        let fit = crate::services::fit::Fit::load(&d, c.sde.clone(), owner.character, own).await.ok().unwrap();
        let condition = crate::services::combat::prepare_condition_with_fit(&d, own, &fit).await.ok().unwrap();
        c.world.with(system, move |s| s.install_condition(own, condition.into_condition(), fit)).await.unwrap();
        assert!(matches!(rpc(&mut c, "ship", "GetDirtTimestamp", &[Value::Int(other)], &bind).await.unwrap(), Value::Int(_)));
        refused(rpc(&mut c, "ship", "ResetDirtTimestamp", &[Value::Int(other)], &bind).await);
        let result = rpc(&mut c, "ship", "ResetDirtTimestamp", &[Value::Int(own)], &bind).await.unwrap();
        assert_eq!(rpc(&mut c, "ship", "GetDirtTimestamp", &[Value::Int(own)], &bind).await.unwrap(), result);
        let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = notify;
        for (service, method, args) in [
            ("ship", "LeaveShip", vec![]),
            ("ship", "SetShipPassword", vec![Value::Int(own), Value::str("fixture-password")]),
            ("dogmaIM", "AddTargetOBO", vec![Value::Int(own), Value::Int(other)]),
            ("dogmaIM", "RemoveTargetOBO", vec![Value::Int(own), Value::Int(other)]),
        ] {
            let before = rollback::fingerprint(&d, own).await;
            let session: Vec<_> = c.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
            let notices = c.outbox.len();
            let condition = c.world.with(system, move |s| {
                let v = s.combat_condition_snapshot(own).unwrap();
                (v.revision, v.condition.now, v.condition.stats.max)
            }).await.unwrap();
            assert!(events.try_recv().is_err());
            refused(rpc(&mut c, service, method, &args, &bind).await);
            assert_eq!(rollback::fingerprint(&d, own).await, before, "{method}");
            assert_eq!(c.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session, "{method}");
            assert_eq!(c.outbox.len(), notices, "{method}");
            assert!(events.try_recv().is_err(), "{method}");
            let after = c.world.with(system, move |s| {
                let v = s.combat_condition_snapshot(own).unwrap();
                (v.revision, v.condition.now, v.condition.stats.max)
            }).await.unwrap();
            assert_eq!(after, condition, "{method}");
        }
        c.world.with(system, move |s| { let mut target = ship(); target.id = other; target.owner_id = foreign.character; target.type_id = hull; target.cloak = 1; s.insert(target); }).await.unwrap();
        refused(rpc(&mut c, "ship", "GetDirtTimestamp", &[Value::Int(other)], &bind).await);
        c.world.remove_ball(system, own).await;
        c.world.remove_ball(system, other).await;
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
