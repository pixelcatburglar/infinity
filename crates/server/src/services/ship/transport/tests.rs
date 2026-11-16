//! Real-database `/tr me <station>` regressions. Each scenario uses a real
//! account, a complete session and world presence so the real guards run.
use super::*;
use std::sync::Arc;
use infinity_core::consts::{FLAG_CARGO, FLAG_HANGAR, GM_ROLES};
use infinity_core::session::OnlineSession;
use marshal::Value;
use crate::services::identity::Identity;
use crate::services::slash::tests::support::rpc;


fn notify_text(error: crate::services::CallError) -> String {
    let crate::services::CallError::User { msg, dict } = error else { panic!("expected user refusal") };
    assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
    dict.into_iter().find(|(k, _)| k == "notify")
        .and_then(|(_, v)| match v { Value::Unicode(s) => Some(s), _ => None })
        .expect("notify text")
}

/// Two stations in one system and a third in another region, all with ancestry.
fn stations(sde: &crate::sde::Sde) -> ((i64, i64), i64, i64) {
    let (a, b) = {
        let mut pair = None;
        for system in sde.map.systems() {
            let local: Vec<i64> = sde.map.stations().filter(|s| s.solar_system_id == Some(system.solar_system_id)
                && s.constellation_id.is_some() && s.region_id.is_some()).map(|s| s.station_id).collect();
            if local.len() >= 2 { pair = Some((system.solar_system_id, local)); break; }
        }
        pair.map(|(system, local)| ((local[0], system), local[1])).expect("SDE must contain a system with two stations")
    };
    let a_region = sde.map.station(a.0).unwrap().region_id;
    let c = sde.map.stations().find(|s| s.region_id.is_some() && s.region_id != a_region
        && s.solar_system_id.is_some() && s.constellation_id.is_some()).map(|s| s.station_id).expect("another region");
    (a, b, c)
}

/// A complete online session: `station` set is docked (solarsystemid
/// cleared), `None` is in-space; ancestry fields mirror a real login.
async fn session(client: &mut crate::client::Client, account: i64, character: i64, ship: i64,
    station: Option<i64>, system: i64) {
    let row = client.sde.map.system(system).expect("source system");
    client.login.user_id = account;
    client.login.role = GM_ROLES;
    client.login.client_id = character;
    client.session.apply(vec![
        ("charid", Value::Int(character)), ("userid", Value::Int(account)),
        ("shipid", Value::Int(ship)), ("solarsystemid2", Value::Int(system)),
        ("constellationid", Value::Int(row.constellation_id.unwrap())),
        ("regionid", Value::Int(row.region_id.unwrap())),
        ("stationid", station.map(Value::Int).unwrap_or(Value::None)),
        ("solarsystemid", if station.is_some() { Value::None } else { Value::Int(system) }),
    ]);
    let session = OnlineSession::from_session(&client.session).expect("complete session");
    let identity = Identity::of_client(client);
    crate::services::presence::online(&identity, &client.world, &client.chat, client.notify_tx.clone(), None, session);
}

async fn hull_at(db: &crate::db::Db, ship: i64) -> (i64, i32, Option<f64>, Option<f64>) {
    db.query_one("SELECT location_id, flag_id, x, y FROM game.items WHERE item_id=$1", &[&ship]).await
        .map(|row| (row.get("location_id"), row.get("flag_id"), row.get("x"), row.get("y"))).unwrap()
}

async fn char_at(db: &crate::db::Db, character: i64) -> (Option<i64>, Option<i64>, Option<i64>, Option<i64>) {
    db.query_one("SELECT station_id, solar_system_id, constellation_id, region_id FROM game.characters WHERE character_id=$1", &[&character]).await
        .map(|row| (row.get("station_id"), row.get("solar_system_id"), row.get("constellation_id"), row.get("region_id"))).unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn station_transport_moves_hull_and_session_without_touching_unrelated_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ((station_a, system_a), station_b, station_c) = stations(&sde);
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let module_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_MODULE)).unwrap().type_id;
    let cargo_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)).unwrap().type_id;
    let hull = crate::dao::items::creation::rookie_ship(&db, ship_type, fixture.character, station_a, FLAG_HANGAR).await.unwrap();
    let module = crate::dao::items::creation::rookie_ship(&db, module_type, fixture.character, hull, infinity_core::consts::FLAG_HI_SLOT_FIRST).await.unwrap();
    let cargo = crate::dao::items::transfers::insert(&db, cargo_type, fixture.character, hull, FLAG_CARGO, 7).await.unwrap();
    let container = crate::dao::items::transfers::insert(&db, cargo_type, fixture.character, hull, FLAG_CARGO, 1).await.unwrap();
    let nested = crate::dao::items::transfers::insert(&db, cargo_type, fixture.character, container, FLAG_CARGO, 3).await.unwrap();
    let other = crate::dao::items::creation::rookie_ship(&db, ship_type, fixture.character, station_a, FLAG_HANGAR).await.unwrap();
    crate::dao::characters::selection::set_ship(&db, fixture.character, hull).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=$2, solar_system_id=$3, home_station_id=$4 WHERE character_id=$1",
        &[&fixture.character, &station_a, &system_a, &station_b]).await.unwrap();
    crate::dao::items::dogma::set_override(&db, hull, infinity_core::consts::dogma::ATTR_HULL_DAMAGE, 0.25).await.unwrap();
    let balance = crate::services::wallets::balance(&db, fixture.character).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), fixture.character).await;
        session(&mut client, fixture.account, fixture.character, hull, Some(station_a), system_a).await;
        // A non-GM caller is refused before any destination work.
        client.login.role = 0;
        assert!(rpc(&mut client, &format!("/tr me {station_b}")).await.is_err());
        assert_eq!(char_at(&test_db, fixture.character).await.0, Some(station_a));
        client.login.role = GM_ROLES;
        // Same-system move to a different station.
        rpc(&mut client, &format!("/tr me {station_b}")).await.expect("same-system transport");
        let dest_b = sde.map.station(station_b).unwrap();
        assert_eq!(char_at(&test_db, fixture.character).await, (Some(station_b), Some(system_a), dest_b.constellation_id, dest_b.region_id));
        assert_eq!(hull_at(&test_db, hull).await, (station_b, FLAG_HANGAR, None, None));
        assert_eq!(client.session.int("stationid"), Some(station_b));
        assert_eq!(client.session.int("solarsystemid"), None);
        assert_eq!(client.session.int("solarsystemid2"), Some(system_a));
        let live = client.world.online_session(fixture.character).expect("online snapshot");
        assert_eq!((live.station_id, live.system_id, live.ship_id), (Some(station_b), Some(system_a), Some(hull)));
        assert!(client.outbox.iter().any(|packet| packet.command == infinity_wire::macho::cmd::SESSIONCHANGENOTIFICATION));
        // Cross-region move.
        rpc(&mut client, &format!("/tr {station_c}")).await.expect("cross-region transport");
        let dest_c = sde.map.station(station_c).unwrap();
        assert_eq!(char_at(&test_db, fixture.character).await, (Some(station_c), dest_c.solar_system_id, dest_c.constellation_id, dest_c.region_id));
        assert_eq!(hull_at(&test_db, hull).await, (station_c, FLAG_HANGAR, None, None));
        let live = client.world.online_session(fixture.character).expect("online snapshot after cross-region");
        assert_eq!((live.station_id, live.system_id), (Some(station_c), Some(dest_c.solar_system_id.unwrap())));
        // Same-station request validates and changes nothing.
        let before = client.outbox.len();
        rpc(&mut client, &format!("/tr me {station_c}")).await.expect("same-station no-op");
        assert_eq!(client.outbox.len(), before, "no-op must not publish");
        // Unrelated state stays: hull contents, hangar items, home, wallet, damage.
        let rows = test_db.query_one("SELECT location_id, flag_id FROM game.items WHERE item_id=$1", &[&module]).await.unwrap();
        assert_eq!((rows.get::<_, i64>("location_id"), rows.get::<_, i32>("flag_id")), (hull, infinity_core::consts::FLAG_HI_SLOT_FIRST));
        let rows = test_db.query_one("SELECT location_id, flag_id, quantity FROM game.items WHERE item_id=$1", &[&cargo]).await.unwrap();
        assert_eq!((rows.get::<_, i64>("location_id"), rows.get::<_, i64>("quantity")), (hull, 7));
        let rows = test_db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&nested]).await.unwrap();
        assert_eq!(rows.get::<_, i64>("location_id"), container, "nested cargo keeps its container parent");
        let owner: i64 = test_db.query_one("SELECT owner_id FROM game.items WHERE item_id=$1", &[&hull]).await.unwrap().get("owner_id");
        assert_eq!(owner, fixture.character, "transport keeps the hull owner");
        assert_eq!(hull_at(&test_db, other).await, (station_a, FLAG_HANGAR, None, None));
        let row = test_db.query_one("SELECT home_station_id FROM game.characters WHERE character_id=$1", &[&fixture.character]).await.unwrap();
        assert_eq!(row.get::<_, Option<i64>>("home_station_id"), Some(station_b));
        assert_eq!(crate::services::wallets::balance(&test_db, fixture.character).await.unwrap(), balance);
        assert_eq!(crate::dao::items::dogma::overrides(&test_db, hull).await.unwrap().iter()
            .find(|r| r.attribute_id == infinity_core::consts::dogma::ATTR_HULL_DAMAGE).unwrap().value, 0.25);
    }).await;
    client_cleanup(&db, fixture.character, &[hull, module, cargo, container, nested, other]).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn station_transport_refusals_and_rollback_leave_everything_untouched() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ((station_a, system_a), station_b, _station_c) = stations(&sde);
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let hull = crate::dao::items::creation::rookie_ship(&db, ship_type, fixture.character, station_a, FLAG_HANGAR).await.unwrap();
    crate::dao::characters::selection::set_ship(&db, fixture.character, hull).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=$2, solar_system_id=$3 WHERE character_id=$1", &[&fixture.character, &station_a, &system_a]).await.unwrap();
    let partner = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), fixture.character).await;
        session(&mut client, fixture.account, fixture.character, hull, Some(station_a), system_a).await;
        // An unknown station is refused by the shared service entry point.
        assert_eq!(notify_text(relocate(&mut client, Destination::Station(i64::MAX)).await.unwrap_err()),
            infinity_core::messages::gm::GM_TRANSPORT_INVALID_STATION);
        // No active ship: the session no longer owns a hull.
        client.session.apply(vec![("shipid", Value::None)]);
        assert_eq!(notify_text(rpc(&mut client, &format!("/tr me {station_b}")).await.unwrap_err()),
            infinity_core::messages::gm::GM_TRANSPORT_NO_SHIP);
        client.session.apply(vec![("shipid", Value::Int(hull))]);
        // Stale snapshot: the world's online session no longer matches.
        let saved = OnlineSession::from_session(&client.session).expect("saved session");
        assert!(client.world.replace_online_session(fixture.character, Some(&saved), None));
        assert_eq!(notify_text(rpc(&mut client, &format!("/tr me {station_b}")).await.unwrap_err()),
            infinity_core::messages::gm::GM_TRANSPORT_CONFLICT);
        client.world.set_online_session(fixture.character, Some(saved));
        // An open direct trade blocks the move.
        let trade = crate::dao::trades::create(&test_db, station_a, fixture.character, partner.character, crate::client::filetime_now()).await.unwrap();
        assert_eq!(notify_text(rpc(&mut client, &format!("/tr me {station_b}")).await.unwrap_err()),
            infinity_core::messages::gm::GM_TRANSPORT_TRADE_OPEN);
        assert_eq!(char_at(&test_db, fixture.character).await.0, Some(station_a));
        test_db.execute("DELETE FROM game.trade_items WHERE trade_id=$1", &[&trade.id]).await.unwrap();
        test_db.execute("DELETE FROM game.trades WHERE trade_id=$1", &[&trade.id]).await.unwrap();
        // A transaction-inner failure after the hull write rolls both writes
        // back and publishes nothing.
        test_db.execute(&format!("ALTER TABLE game.characters ADD CONSTRAINT test_transport_rollback CHECK (station_id IS DISTINCT FROM {station_b}) NOT VALID"), &[]).await.unwrap();
        let before = client.outbox.len();
        assert!(rpc(&mut client, &format!("/tr me {station_b}")).await.is_err());
        test_db.execute("ALTER TABLE game.characters DROP CONSTRAINT test_transport_rollback", &[]).await.unwrap();
        assert_eq!(client.outbox.len(), before, "rolled-back transport must not publish");
        assert_eq!(char_at(&test_db, fixture.character).await.0, Some(station_a));
        assert_eq!(hull_at(&test_db, hull).await, (station_a, FLAG_HANGAR, None, None));
        let live = client.world.online_session(fixture.character).expect("online snapshot");
        assert_eq!(live.station_id, Some(station_a));
    }).await;
    db.execute("ALTER TABLE game.characters DROP CONSTRAINT IF EXISTS test_transport_rollback", &[]).await.unwrap();
    client_cleanup(&db, fixture.character, &[hull]).await;
    crate::services::tests::fixture::cleanup(&db, partner).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn space_transport_docks_with_dirty_condition_and_no_destination_actor() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some())
        .expect("SDE must contain a station with full ancestry");
    let (station_c, dest_system) = (station.station_id, station.solar_system_id.unwrap());
    let source = sde.map.systems().find(|s| s.solar_system_id != dest_system).map(|s| s.solar_system_id).expect("a second system");
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let hull = crate::dao::items::creation::positioned(&db, ship_type, fixture.character, source, [1.0, 2.0, 3.0]).await.unwrap();
    crate::dao::characters::selection::set_ship(&db, fixture.character, hull).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=NULL, solar_system_id=$2 WHERE character_id=$1", &[&fixture.character, &source]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), fixture.character).await;
        session(&mut client, fixture.account, fixture.character, hull, None, source).await;
        let world = client.world.clone();
        // Real pilot admission with a dirty (unsaved) combat condition.
        world.system(source).await.unwrap();
        let row = crate::dao::items::space_ships::FreeShip { item_id: hull, type_id: ship_type, owner_id: fixture.character,
            corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let prepared = super::super::balls::piloted(&test_db, sde.clone(), &row, fixture.character, 0.0).await.unwrap();
        world.with(source, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let damaged = world.with(source, move |s| { assert!(s.set_condition_fraction_mask(hull, 0b110, 0.5));
            s.combat_health_scalars(hull).unwrap().1 }).await.unwrap();
        rpc(&mut client, &format!("/tr me {station_c}")).await.expect("space-to-station transport");
        let dest = sde.map.station(station_c).unwrap();
        assert_eq!(char_at(&test_db, fixture.character).await, (Some(station_c), Some(dest_system), dest.constellation_id, dest.region_id));
        assert_eq!(hull_at(&test_db, hull).await, (station_c, FLAG_HANGAR, None, None));
        // The source hull retired and no destination actor was loaded.
        assert!(world.with(source, move |s| s.pilot_ball(fixture.character).is_none()).await.unwrap());
        assert!(world.with(dest_system, move |s| s.current_time()).await.is_none(), "destination actor must not be loaded");
        // The dirty condition was checkpointed, not lost.
        let rows = crate::dao::items::dogma::overrides(&test_db, hull).await.unwrap();
        let armor = rows.iter().find(|r| r.attribute_id == infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE).unwrap().value;
        assert!((armor - damaged[1]).abs() < 1e-8);
        let live = client.world.online_session(fixture.character).expect("online snapshot");
        assert_eq!((live.station_id, live.system_id), (Some(station_c), Some(dest_system)));
        assert_eq!(client.session.int("stationid"), Some(station_c));
        assert_eq!(client.session.int("solarsystemid"), None);
        assert_eq!(client.session.int("solarsystemid2"), Some(dest_system));
        // A dock arrival is not a gate jump: no jump or visit telemetry.
        assert!(crate::dao::map_telemetry::visits(&test_db, fixture.character).await.unwrap().is_empty());
    }).await;
    client_cleanup(&db, fixture.character, &[hull]).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

async fn client_cleanup(db: &crate::db::Db, character: i64, items: &[i64]) {
    db.execute("DELETE FROM game.item_attributes WHERE item_id=ANY($1)", &[&items]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=ANY($1) OR location_id=ANY($1)", &[&items]).await.unwrap();
    db.execute("DELETE FROM game.trade_items WHERE item_id=ANY($1)", &[&items]).await.unwrap();
    db.execute("DELETE FROM game.trades WHERE char_a=$1 OR char_b=$1", &[&character]).await.unwrap();
}
