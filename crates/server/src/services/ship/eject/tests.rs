//! Bound `ship.Eject` and the follow-up `ship.Board` against a dedicated
//! initialized database.
use infinity_core::consts::{AU, GROUP_SOLAR_SYSTEM, TYPE_CAPSULE};
use crate::dao::characters::selection;
use crate::dao::items::loading::{load_items, ItemQuery};
use crate::dao::items::space_ships::FreeShip;
use crate::services::{Call, CallResult, drones::tests::fixture::Fixture};
use crate::tests::catch_scenario;
use marshal::Value;

async fn fixture() -> Fixture {
    let mut f = Fixture::new().await;
    let home = f.sde.map.systems().find(|s| s.constellation_id.is_some() && s.region_id.is_some()).expect("SDE system hierarchy");
    crate::dao::characters::ship::location(&f.db, f.owner, f.system, home.constellation_id.unwrap(), home.region_id.unwrap()).await.unwrap();
    assert!(crate::dao::items::dogma::set_override(&f.db, f.ship, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, 1.0).await.unwrap());
    f.world.remove_ball(f.system, f.ship).await;
    f.client.session.apply(vec![("userid", Value::Int(f.account)), ("solarsystemid2", Value::Int(f.system))]);
    f.world.unregister_pilot_generation(f.owner, None);
    crate::space::session_change::test_support::register_elapsed(&f.world, f.owner, f.client.notify_tx.clone());
    let online = infinity_core::session::OnlineSession::from_session(&f.client.session).expect("fixture online session");
    f.world.set_online_session(f.owner, Some(online));
    let kind = load_items(&f.db, ItemQuery::Id(f.ship)).await.unwrap().pop().unwrap().type_id;
    let levels = crate::services::skills::requirements::requirements(&f.sde, kind).filter(|&(_, level)| level > 0).collect();
    let owner = f.owner;
    crate::services::skills::gm::apply(&mut f.client, owner, crate::services::skills::gm::Change::Set(levels)).await.unwrap();
    let row = FreeShip { item_id: f.ship, type_id: kind, owner_id: f.owner, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
    let pilot = super::super::balls::piloted(&f.db, f.sde.clone(), &row, f.owner, 0.0).await.unwrap();
    f.world.with(f.system, move |s| pilot.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    f.world.observe(f.system, f.client.login.client_id, f.ship, f.client.destiny_tx.clone()).await.unwrap();
    f
}

async fn call(f: &mut Fixture, method: &str, args: &[Value]) -> CallResult {
    let kwargs = Value::None;
    let params = Value::tuple([Value::Int(f.system), Value::Int(GROUP_SOLAR_SYSTEM)]);
    crate::services::dispatch(&mut f.client, &Call {
        service: "ship", method, args, kwargs: &kwargs, bound: true, bind_params: Some(&params),
    }).await
}

async fn eject(f: &mut Fixture) -> CallResult { call(f, "Eject", &[]).await }

/// DB rows, session attributes and the actor's pilot index; a refusal must
/// leave every component unchanged.
async fn snapshot(f: &Fixture) -> String {
    let owner = f.owner;
    let row: String = f.db.query_one("SELECT row_to_json(t)::text AS snapshot FROM (SELECT type_id,owner_id,location_id,flag_id,singleton,quantity,x,y,z FROM game.items WHERE item_id=$1) t", &[&f.ship]).await.unwrap().get("snapshot");
    let session: Vec<_> = f.client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
    let pilot = f.world.with(f.system, move |s| s.pilot_ball(owner).map(|b| b.id)).await;
    let selected = selection::ship_id(&f.db, owner).await.unwrap();
    format!("{row} {session:?} {pilot:?} {selected:?}")
}

/// Reinstall a mutated ball while keeping the hull's live combat condition.
async fn replace_ball(f: &Fixture, ball: crate::space::Ball) {
    let fit = crate::services::fit::Fit::load(&f.db, f.sde.clone(), f.owner, f.ship).await.unwrap();
    let ship = f.ship;
    f.world.with(f.system, move |s| {
        let condition = s.combat_condition_snapshot(ship).expect("live condition");
        s.add_ship(ball);
        let generation = s.persistent_generation(ship).expect("persistent membership");
        assert!(s.transfer_combat_state(ship, generation, condition, fit, crate::services::fit::Key::Ship));
    }).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn eject_switches_to_capsule_leaves_hull_unpiloted_and_reboards() {
    let mut f = fixture().await;
    let result = catch_scenario(async {
        let (old, owner, db) = (f.ship, f.owner, f.db.clone());
        let (pos, armor) = f.world.with(f.system, move |s| {
            (s.sampled_ball(old).map(|b| b.pos), s.ship_condition(old).map(|c| c.1))
        }).await.unwrap();
        let pos = pos.expect("pilot ball sampled");
        crate::dao::items::parent_locks::tests::character_before_items(&db, owner, &[old], async { eject(&mut f).await.unwrap() }).await;
        let capsule = f.client.session.int("shipid").unwrap();
        assert_ne!(capsule, old);
        assert_eq!(selection::ship_id(&f.db, owner).await.unwrap(), Some(capsule));
        let row = f.db.query_one("SELECT type_id,owner_id,location_id,flag_id,singleton,quantity,x,y,z FROM game.items WHERE item_id=$1", &[&capsule]).await.unwrap();
        assert_eq!((row.get::<_, i32>("type_id"), row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")),
            (TYPE_CAPSULE, owner, f.system, 0));
        assert_eq!((row.get::<_, bool>("singleton"), row.get::<_, i64>("quantity")), (true, 1));
        assert_eq!([row.get::<_, f64>("x"), row.get::<_, f64>("y"), row.get::<_, f64>("z")], pos);
        let row = f.db.query_one("SELECT owner_id,location_id,flag_id,singleton,quantity,x,y,z FROM game.items WHERE item_id=$1", &[&old]).await.unwrap();
        assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (owner, f.system, 0));
        assert_eq!((row.get::<_, bool>("singleton"), row.get::<_, i64>("quantity")), (true, 1));
        assert_eq!([row.get::<_, f64>("x"), row.get::<_, f64>("y"), row.get::<_, f64>("z")], pos);
        let after = f.world.with(f.system, move |s| {
            assert_eq!(s.pilot_ball(owner).map(|b| b.id), Some(capsule));
            let hull = s.ball(old).expect("unpiloted hull remains in the scene");
            assert!(hull.char_id.is_none() && !hull.interactive);
            s.ship_condition(old).map(|c| c.1)
        }).await.unwrap();
        assert_eq!(after, armor, "armor damage must carry to the unpiloted avatar");
        let cooled = snapshot(&f).await;
        let early = call(&mut f, "Board", &[Value::Int(old), Value::Int(capsule)]).await;
        assert!(early.as_ref().is_err_and(crate::space::session_change::test_support::ship_change_cooldown));
        assert_eq!(snapshot(&f).await, cooled);
        crate::space::session_change::test_support::wait_for_ship_change().await;
        call(&mut f, "Board", &[Value::Int(old), Value::Int(capsule)]).await.unwrap();
        assert_eq!(f.client.session.int("shipid"), Some(old));
        assert_eq!(selection::ship_id(&f.db, owner).await.unwrap(), Some(old));
        f.world.with(f.system, move |s| assert_eq!(s.pilot_ball(owner).map(|b| b.id), Some(old))).await.unwrap();
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn eject_refuses_capsule_warp_timer_and_station_atomically() {
    let mut f = fixture().await;
    let result = catch_scenario(async {
        let old = f.ship;
        eject(&mut f).await.unwrap();
        let committed = snapshot(&f).await;
        assert!(eject(&mut f).await.is_err(), "ejecting a capsule must refuse");
        assert_eq!(snapshot(&f).await, committed);
        let capsule = f.client.session.int("shipid").unwrap();
        let cooled = snapshot(&f).await;
        let early = call(&mut f, "Board", &[Value::Int(old), Value::Int(capsule)]).await;
        assert!(early.as_ref().is_err_and(crate::space::session_change::test_support::ship_change_cooldown));
        assert_eq!(snapshot(&f).await, cooled);
        crate::space::session_change::test_support::wait_for_ship_change().await;
        call(&mut f, "Board", &[Value::Int(old), Value::Int(capsule)]).await.unwrap();
        crate::space::session_change::test_support::wait_for_ship_change().await;
        let original = f.world.ball(f.system, old).await.unwrap();
        let mut warped = original.clone();
        assert_eq!(warped.warp_to([10.0 * AU, 0.0, 0.0], 0.0, 1), crate::space::WarpOutcome::Align);
        replace_ball(&f, warped).await;
        let committed = snapshot(&f).await;
        assert!(eject(&mut f).await.is_err(), "ejecting in warp must refuse");
        assert_eq!(snapshot(&f).await, committed);
        replace_ball(&f, original).await;
        crate::services::crimewatch::weapons_activate(&f.world, f.owner);
        let committed = snapshot(&f).await;
        assert!(eject(&mut f).await.is_err(), "ejecting under a weapons timer must refuse");
        assert_eq!(snapshot(&f).await, committed);
        f.client.session.apply(vec![("stationid", Value::Int(i64::MAX))]);
        let committed = snapshot(&f).await;
        assert!(eject(&mut f).await.is_err(), "ejecting on a station binding must refuse");
        assert_eq!(snapshot(&f).await, committed);
        f.client.session.apply(vec![("stationid", Value::None)]);
        assert_eq!(selection::ship_id(&f.db, f.owner).await.unwrap(), Some(old));
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
