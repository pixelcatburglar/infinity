//! Fixed-output crate opening from an ordinary container with no free capacity.
use super::*;
use infinity_core::{consts::{FLAG_CARGO, FLAG_HANGAR}};
use crate::services::inventory::hold_storage;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn zero_capacity_source_opens_into_station_hangar() {
    open_from_box(false).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn overfull_nested_source_opens_into_station_hangar() {
    open_from_box(true).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn zero_capacity_source_opens_into_active_hull_cargo() {
    open_nested_in_space(false).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn overfull_nested_source_opens_into_active_hull_cargo() {
    open_nested_in_space(true).await;
}

fn ordinary_box_type(sde: &crate::sde::Sde, overfull: bool) -> i32 {
    sde.types.iter().find(|ty|
        hold_storage::is_ordinary_container(sde, ty.type_id)
            && hold_storage::static_capacity(sde, ty.type_id).is_ok_and(|capacity| if overfull { capacity > 0.0 } else { capacity == 0.0 }))
        .expect("ordinary container with requested capacity in loaded SDE").type_id
}

fn crate_quantity(sde: &crate::sde::Sde, overfull: bool, box_type: i32, crate_type: i32) -> i64 {
    if !overfull { return 2; }
    let unit = crate::services::item_volume::item_volume(sde, crate_type, false, 1, None).unwrap();
    let capacity = hold_storage::static_capacity(sde, box_type).unwrap();
    assert!(unit > 0.0, "positive crate volume required for overfull fixture");
    ((capacity / unit).floor() as i64).checked_add(2).unwrap()
}

async fn open_nested_in_space(overfull: bool) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let world = client.world.clone();
    let scenario_db = db.clone();
    let (space_tx, space_rx) = tokio::sync::oneshot::channel();
    let outcome = tokio::spawn(async move {
        let db = scenario_db;
        let mut client = client;
        let system = client.sde.map.systems().next().unwrap().solar_system_id;
        let box_type = ordinary_box_type(&client.sde, overfull);
        let definition = client.sde.crates.iter().find(|definition|
            definition.preview(false, None).is_ok_and(|rewards| !rewards.is_empty()))
            .expect("server-authored direct-list crate");
        let crate_type = definition.type_id;
        let expected = definition.preview(false, None).unwrap();
        let box_volume = crate::services::item_volume::item_volume(&client.sde, box_type, true, 1, None).unwrap();
        let reward_volume: f64 = expected.iter().map(|(type_id, quantity)|
            crate::services::item_volume::item_volume(&client.sde, *type_id, false, *quantity, None).unwrap()).sum();
        let needed = box_volume + reward_volume;
        let ship_type = client.sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP) && ty.capacity.is_some_and(|capacity| capacity >= needed)).expect("SDE must contain a ship whose capacity holds the box and rewards").type_id;
        let ship = transfers::insert(&db, ship_type, actor.character, system, 0, 1).await.unwrap();
        db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&ship]).await.unwrap();
        db.execute("UPDATE game.characters SET ship_id=$2, solar_system_id=$3, station_id=NULL WHERE character_id=$1", &[&actor.character, &ship, &system]).await.unwrap();
        client.world.register_pilot(actor.character, client.notify_tx.clone());
        client.world.add_ship(system, crate::space::Ball {
            id: ship, type_id: ship_type, owner_id: actor.character, corp_id: 1, alliance_id: -1,
            char_id: Some(actor.character), signature_radius: Some(1.0), mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
            max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
            pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3],
            old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
            follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: true,
            massive: true, warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
            cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
        }).await.unwrap();
        let _ = space_tx.send((system, ship));
        client.session.apply(vec![("stationid", Value::None), ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        let insert = "INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id";
        let outer: i64 = db.query_one(insert, &[&box_type, &actor.character, &ship, &FLAG_CARGO]).await.unwrap().get("item_id");
        let inner: i64 = db.query_one(insert, &[&box_type, &actor.character, &outer, &FLAG_CARGO]).await.unwrap().get("item_id");
        let child_type = client.sde.types.iter().find(|ty| ty.type_id != box_type && ty.type_id != crate_type).expect("unrelated child type").type_id;
        let child: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&child_type, &actor.character, &inner, &FLAG_CARGO]).await.unwrap().get("item_id");
        let quantity = crate_quantity(&client.sde, overfull, box_type, crate_type);
        let source = transfers::insert(&db, crate_type, actor.character, inner, FLAG_CARGO, quantity).await.unwrap();
        assert!(source > inner && inner > outer, "crate stack is the highest requested non-hull id");
        let (hull_capacity, hull_used, held) = hold_storage::capacity(&db, &client.sde, actor.character, ship, FLAG_CARGO).await.unwrap().expect("active ship cargo capacity");
        drop(held);
        assert!(hull_capacity - hull_used >= reward_volume, "active hull cargo must hold the reward volume");
        let rows_before: Vec<String> = db.query("SELECT row_to_json(i)::text AS value FROM game.items i WHERE item_id = ANY($1) ORDER BY item_id", &[&vec![outer, inner, child]]).await.unwrap().into_iter().map(|row| row.get("value")).collect();
        let owned = vec![outer, inner, child, source];
        let owned_before: Vec<String> = db.query("SELECT row_to_json(i)::text AS value FROM game.items i WHERE item_id = ANY($1) ORDER BY item_id", &[&owned]).await.unwrap().into_iter().map(|row| row.get("value")).collect();
        let config = crate::config::tests::test_database();
        let (mut connection, driver) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { driver.await.unwrap(); });
        let holder = connection.transaction().await.unwrap();
        let pid = crate::dao::items::parent_locks::tests::backend_pid(&holder).await;
        holder.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&source]).await.unwrap();
        let opening = async { rpc(&mut client, "crateService", "OpenCrate", &[Value::Int(source)], false).await };
        let release = async {
            crate::dao::items::parent_locks::tests::wait_blocked(&db, pid).await;
            let mut states = Vec::new();
            for (id, update) in [(ship, false), (inner, false), (outer, false), (outer, true)] {
                let (mut probe, probe_driver) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
                tokio::spawn(async move { probe_driver.await.unwrap(); });
                let probe_tx = probe.transaction().await.unwrap();
                let sql = if update {
                    "SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE NOWAIT"
                } else {
                    "SELECT item_id FROM game.items WHERE item_id=$1 FOR SHARE NOWAIT"
                };
                states.push(probe_tx.query_one(sql, &[&id]).await.err().and_then(|error| error.code().cloned()));
            }
            let blocked: Vec<String> = db.query("SELECT row_to_json(i)::text AS value FROM game.items i WHERE item_id = ANY($1) ORDER BY item_id", &[&owned]).await.unwrap().into_iter().map(|row| row.get("value")).collect();
            holder.commit().await.unwrap();
            (states, blocked)
        };
        let (response, (states, blocked)) = tokio::time::timeout(std::time::Duration::from_secs(30), async { tokio::join!(opening, release) }).await.unwrap();
        let response = response.unwrap();
        triples(&response);
        let locked = Some(tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE);
        let states_ok = states == vec![locked.clone(), locked.clone(), None, locked];
        let blocked_ok = blocked == owned_before;
        let remaining: i64 = db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().get("quantity");
        let response_items = response.items().unwrap();
        let rewards_ok = response_items.len() == expected.len() && response_items.iter().zip(expected.iter()).all(|(entry, (type_id, reward_quantity))| {
            let fields = entry.items().unwrap();
            fields[1].as_int() == Some(i64::from(*type_id)) && fields[2].as_int() == Some(*reward_quantity)
        });
        let mut placed = Vec::new();
        for entry in response_items {
            let id = entry.items().unwrap()[0].as_int().unwrap();
            let row = db.query_one("SELECT location_id,flag_id,owner_id FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
            let receipt = db.query_one("SELECT destination_location,destination_flag,crate_type_id FROM game.crate_rewards WHERE item_id=$1 AND character_id=$2", &[&id, &actor.character]).await.unwrap();
            placed.push((
                row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"), row.get::<_, i64>("owner_id"),
                receipt.get::<_, i64>("destination_location"), receipt.get::<_, i32>("destination_flag"), receipt.get::<_, i32>("crate_type_id"),
            ));
        }
        let rows_after: Vec<String> = db.query("SELECT row_to_json(i)::text AS value FROM game.items i WHERE item_id = ANY($1) ORDER BY item_id", &[&vec![outer, inner, child]]).await.unwrap().into_iter().map(|row| row.get("value")).collect();
        let outbox_len = client.outbox.len();
        let reward_ids = db.query("SELECT item_id FROM game.crate_rewards WHERE character_id=$1 AND crate_type_id=$2", &[&actor.character, &crate_type]).await.unwrap().into_iter().map(|row| row.get::<_, i64>("item_id")).collect::<Vec<_>>();
        db.execute("DELETE FROM game.crate_rewards WHERE item_id = ANY($1)", &[&reward_ids]).await.unwrap();
        db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&reward_ids]).await.unwrap();
        db.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
        (states_ok, blocked_ok, remaining, rewards_ok, placed, rows_after, outbox_len, rows_before, quantity, expected.len(), ship, crate_type)
    });
    let space = tokio::join!(space_rx, outcome);
    if let Ok((system, ship)) = space.0 { world.remove_ball(system, ship).await; }
    world.unregister_pilot_generation(actor.character, None);
    fixture::cleanup(&db, actor).await.unwrap();
    let (states_ok, blocked_ok, remaining, rewards_ok, placed, rows_after, outbox_len, rows_before, quantity, reward_count, ship, crate_type) = space.1.unwrap();
    assert!(states_ok, "ship and inner UPDATE, outer SHARE");
    assert!(blocked_ok, "owned rows unchanged while the crate row was held");
    assert_eq!(remaining, quantity - 1);
    assert!(rewards_ok, "opened rewards match the crate preview");
    assert!(!placed.is_empty());
    assert!(placed.iter().all(|row| *row == (ship, FLAG_CARGO, actor.character, ship, FLAG_CARGO, crate_type)));
    assert_eq!(rows_after, rows_before);
    assert_eq!(outbox_len, 1 + reward_count);
}

async fn open_from_box(overfull: bool) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let scenario_db = db.clone();
    let outcome = tokio::spawn(async move {
    let db = scenario_db;
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    client.session.apply(vec![("stationid", Value::Int(station))]);
    let box_type = ordinary_box_type(&client.sde, overfull);
    let definition = client.sde.crates.iter().find(|definition|
        definition.preview(false, None).is_ok_and(|rewards| !rewards.is_empty()))
        .expect("server-authored direct-list crate");
    let crate_type = definition.type_id;
    let expected = definition.preview(false, None).unwrap();
    let outer: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&box_type, &actor.character, &station, &FLAG_HANGAR]).await.unwrap().get("item_id");
    let parent: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&box_type, &actor.character, &outer, &FLAG_CARGO]).await.unwrap().get("item_id");
    let quantity = crate_quantity(&client.sde, overfull, box_type, crate_type);
    let source = transfers::insert(&db, crate_type, actor.character, parent, FLAG_CARGO, quantity).await.unwrap();
    let config = crate::config::tests::test_database();
    let (mut connection, driver) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { driver.await.unwrap(); });
    let holder = connection.transaction().await.unwrap();
    let pid = crate::dao::items::parent_locks::tests::backend_pid(&holder).await;
    holder.execute("UPDATE game.items SET quantity=quantity WHERE item_id=$1", &[&parent]).await.unwrap();
    let opening = async { rpc(&mut client, "crateService", "OpenCrate", &[Value::Int(source)], false).await };
    let release = async {
        crate::dao::items::parent_locks::tests::wait_blocked(&db, pid).await;
        holder.commit().await.unwrap();
    };
    let (response, ()) = tokio::time::timeout(std::time::Duration::from_secs(30), async { tokio::join!(opening, release) }).await.unwrap();
    let response = response.unwrap();
    triples(&response);
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&source]).await.unwrap().get::<_, i64>("quantity"), quantity - 1);
    assert_eq!(response.items().unwrap().len(), expected.len());
    for (entry, (type_id, quantity)) in response.items().unwrap().iter().zip(expected.iter()) {
        let fields = entry.items().unwrap();
        let id = fields[0].as_int().unwrap();
        assert_eq!(fields[1].as_int(), Some(i64::from(*type_id)));
        assert_eq!(fields[2].as_int(), Some(*quantity));
        let row = db.query_one("SELECT location_id,flag_id,owner_id FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
        assert_eq!((row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"), row.get::<_, i64>("owner_id")), (station, FLAG_HANGAR, actor.character));
        let receipt = db.query_one("SELECT destination_location,destination_flag,crate_type_id FROM game.crate_rewards WHERE item_id=$1 AND character_id=$2", &[&id, &actor.character]).await.unwrap();
        assert_eq!((receipt.get::<_, i64>("destination_location"), receipt.get::<_, i32>("destination_flag"), receipt.get::<_, i32>("crate_type_id")), (station, FLAG_HANGAR, crate_type));
    }
    assert!(!client.outbox.is_empty());
    }).await;
    db.execute("DELETE FROM game.crate_rewards WHERE character_id=$1", &[&actor.character]).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    outcome.unwrap();
}
