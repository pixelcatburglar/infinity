//! GM `/killmark`/`/killmarks` and the unbound `shipKillCounter` RPC through
//! real dispatch: read-only status, exact i64 wire, live slim publication and
//! transfer survival. Fixtures use real items and persistent ball installation;
//! no real client.
use super::*;
use crate::services::tests::{client, fixture};
use crate::services::slash::tests::support;
use infinity_core::session::OnlineSession;
use crate::dao::items::creation;

async fn killmarks(c: &mut Client, line: &str) -> Result<String, CallError> {
    support::rpc(c, line).await.map(|v| v.as_text().map(|t| t.into_owned()).unwrap_or_default())
}
async fn counter(c: &mut Client, args: &[Value]) -> CallResult {
    crate::services::dispatch(c, &Call { service: "shipKillCounter", method: "GetItemKillCountPlayer", args, kwargs: &Value::None, bound: false, bind_params: None }).await
}
async fn count(db: &crate::db::Db, ship: i64) -> Option<i64> {
    db.query_opt("SELECT player_kills FROM game.ship_kill_counts WHERE ship_item_id=$1", &[&ship]).await.unwrap().map(|row| row.get("player_kills"))
}
fn slim_kills(ball: &Ball) -> Option<i64> {
    ball.slim.iter().find(|(k, _)| *k == "kills").and_then(|(_, v)| v.as_int())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn killmarks_command_wire_rpc_live_and_transfer() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let d = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(d.clone(), owner.character).await;
        (c.login.user_id, c.login.role) = (owner.account, infinity_core::consts::GM_ROLES);
        let station = c.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap();
        let (station, system) = (station.station_id, station.solar_system_id.unwrap());
        let remote = c.sde.map.stations().find(|s| s.solar_system_id.is_some() && s.station_id != station).unwrap().station_id;
        let kind = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
        let nonship = c.sde.types.iter().find(|t| t.category_id.is_some() && t.category_id != Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
        let active = creation::rookie_ship(&d, kind, owner.character, station, FLAG_HANGAR).await.unwrap();
        let away = creation::rookie_ship(&d, kind, owner.character, remote, FLAG_HANGAR).await.unwrap();
        let object = creation::rookie_ship(&d, nonship, owner.character, station, FLAG_HANGAR).await.unwrap();
        let live = creation::positioned(&d, TYPE_CAPSULE, foreign.character, system, [0.0; 3]).await.unwrap();
        crate::dao::characters::selection::set_ship(&d, owner.character, active).await.unwrap();
        d.execute("UPDATE game.characters SET station_id=$2, solar_system_id=$3 WHERE character_id=$1", &[&owner.character, &station, &system]).await.unwrap();
        c.session.apply(vec![("userid", Value::Int(owner.account)), ("shipid", Value::Int(active)), ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system))]);
        let _rx = support::register_online(&c.world, OnlineSession::from_session(&c.session).unwrap());
        // Bare status only reads: no row is created for a fresh hull.
        assert_eq!(killmarks(&mut c, "/killmarks").await.unwrap(), messages::gm::gm_killmarks_status(active, 0));
        assert_eq!(count(&d, active).await, None);
        // i64::MAX round-trips exactly; the RPC answer marshals as a Python long.
        assert!(killmarks(&mut c, &format!("/killmark {} {active}", i64::MAX)).await.unwrap().contains(messages::gm::GM_DIRT_NOT_LOADED));
        let wire = marshal::dumps(&counter(&mut c, &[Value::Int(active)]).await.unwrap());
        assert_eq!((wire[5], marshal::loads(&wire).unwrap().as_int()), (marshal::tag::LONG, Some(i64::MAX)));
        // An owned hull at a remote station answers its real count; foreign,
        // non-ship and unknown IDs answer 0, and a malformed shape refuses.
        assert!(killmarks(&mut c, &format!("/killmarks 7 {away}")).await.is_ok());
        assert_eq!(counter(&mut c, &[Value::Int(away)]).await.unwrap().as_int(), Some(7));
        for id in [live, object, i64::MAX] { assert_eq!(counter(&mut c, &[Value::Int(id)]).await.unwrap().as_int(), Some(0), "{id}"); }
        assert!(counter(&mut c, &[Value::Bool(true)]).await.is_err());
        // The live foreign hull publishes its exact slim through one real
        // OnSlimItemChange, and the count patch keeps the stored dirtTime.
        admit_fixture_ship(&d, &c.world, system, foreign.character, live, false).await;
        let dirt = c.world.ball(system, live).await.unwrap().slim.into_iter().find(|(k, _)| *k == "dirtTime").map(|(_, v)| v);
        let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
        c.world.observe(system, owner.character, live, destiny).await.unwrap();
        while let Ok(delivery) = events.try_recv() {
            match delivery {
                crate::space::SceneDelivery::Destiny(_) => {}
                crate::space::SceneDelivery::Notify { event, .. } => assert_eq!(event, "OnFleetStateChange"),
            }
        }
        assert!(killmarks(&mut c, &format!("/killmarks {} {live}", i64::MAX)).await.unwrap().contains(messages::gm::GM_DIRT_PUBLISHED));
        let ball = c.world.ball(system, live).await.unwrap();
        assert_eq!((slim_kills(&ball), ball.slim.iter().find(|(k, _)| *k == "dirtTime").map(|(_, v)| v.clone())), (Some(i64::MAX), dirt));
        let event = std::iter::from_fn(|| events.try_recv().ok()).flat_map(|delivery| match delivery {
            crate::space::SceneDelivery::Destiny(batch) => batch,
            crate::space::SceneDelivery::Notify { event, .. } => { assert_eq!(event, "OnFleetStateChange"); Vec::new() }
        })
            .find(|v| v.items().and_then(|p| p.get(1)).and_then(Value::items).and_then(|call| call.first()).and_then(Value::as_text).is_some_and(|n| n == "OnSlimItemChange")).unwrap();
        let event = marshal::loads(&marshal::dumps(&event)).unwrap();
        let args = event.items().unwrap()[1].items().unwrap()[1].items().unwrap();
        let Value::Instance(_, state) = &args[1] else { panic!("expected slim item") };
        assert_eq!((args[0].as_int(), state.get("kills").and_then(Value::as_int)), (Some(live), Some(i64::MAX)));
        // An identical rewrite commits but broadcasts nothing.
        assert!(killmarks(&mut c, &format!("/killmark {} {live}", i64::MAX)).await.unwrap().contains(messages::gm::GM_DIRT_PUBLISHED));
        assert!(events.try_recv().is_err());
        // clear persists 0; a bad count refuses without touching the row.
        assert_eq!(killmarks(&mut c, &format!("/killmarks clear {active}")).await.unwrap(), messages::gm::gm_killmarks_changed(active, 0, messages::gm::GM_DIRT_NOT_LOADED));
        assert!(killmarks(&mut c, "/killmark -1").await.is_err());
        assert_eq!(count(&d, active).await, Some(0));
        // A real item transfer keeps the row on the hull; it is then foreign.
        let item = crate::services::items::load_items(&d, &c.sde, crate::dao::items::loading::ItemQuery::Id(away)).await.unwrap().pop().unwrap();
        assert!(crate::dao::items::inventory::move_item(&d, &item, remote, FLAG_HANGAR, foreign.character, true).await.unwrap());
        assert_eq!((count(&d, away).await, counter(&mut c, &[Value::Int(away)]).await.unwrap().as_int()), (Some(7), Some(0)));
        // GM status still reads the foreign hull after the transfer.
        assert_eq!(killmarks(&mut c, &format!("/killmarks status {away}")).await.unwrap(), messages::gm::gm_killmarks_status(away, 7));
        c.world.remove_ball(system, live).await;
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
