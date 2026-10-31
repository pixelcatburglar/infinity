use super::*;

fn item(id: i64, quantity: i32) -> Item {
    Item { item_id: id, type_id: 1, owner_id: 1, location_id: 1, flag_id: 1, quantity, singleton: false, group_id: 1, category_id: 1, custom_info: None }
}

#[test]
fn merge_plan_preserves_order_and_conserves_overlapping_stacks() {
    let items = vec![item(5, 4), item(20, 6), item(30, 3)];
    let plan = plan_merges(&items, &[(20, 30, 4), (5, 20, 3)]).ok().unwrap();
    assert_eq!(plan[&5].quantity, Some(1));
    assert_eq!(plan[&20].quantity, Some(5));
    assert_eq!(plan[&30].quantity, Some(7));
}

#[test]
fn merge_plan_deletes_sources_and_rejects_destination_overflow() {
    let items = vec![item(1, 2), item(2, i32::MAX - 2)];
    let plan = plan_merges(&items, &[(1, 2, 99)]).ok().unwrap();
    assert_eq!(plan[&1].quantity, None);
    assert_eq!(plan[&2].quantity, Some(i32::MAX));
    let overflow = vec![item(1, 2), item(2, i32::MAX - 1)];
    assert!(matches!(plan_merges(&overflow, &[(1, 2, 2)]), Err(CallError::User { .. })));
}

#[test]
fn capacity_uses_parent_and_matching_children_from_the_same_rows() {
    let mut parent = item(10, 0);
    parent.location_id = 10;
    let mut child = item(11, 3);
    child.location_id = 10;
    child.type_id = 2;
    let mut null_volume = item(12, -2);
    null_volume.location_id = 10;
    null_volume.type_id = 3;
    let unrelated = item(13, 7);
    let rows = [parent, child, null_volume, unrelated];
    let data = |id| match id {
        1 => (Some(20.0), Some(30.0), Some(2.0)),
        2 => (None, None, Some(4.0)),
        _ => (None, None, None),
    };
    assert_eq!(item_capacity(&rows, 10, data), (20.0, 14.0));
    assert_eq!(item_capacity(&rows, 99, data), (0.0, 0.0));
    let empty_children = item_capacity(&rows, 11, |_| (None, Some(9.0), None));
    assert_eq!(empty_children, (9.0, 0.0));
    assert_eq!(empty_children.1.to_bits(), 0.0_f64.to_bits());
    let empty_default = item_capacity(&rows, 11, |_| (None, None, None));
    assert_eq!(empty_default, (0.0, 0.0));
    assert_eq!(empty_default.1.to_bits(), 0.0_f64.to_bits());
    let empty_volume = item_capacity(&rows, 10, |_| (None, None, None));
    assert_eq!(empty_volume.1.to_bits(), 0.0_f64.to_bits());
}

#[test]
fn gm_fit_sources_must_belong_to_target_and_be_in_cargo_or_hangar() {
    let mut source = item(10, 2);
    source.owner_id = 42;
    source.location_id = 100;
    source.flag_id = FLAG_CARGO;
    assert!(gm::available(&source, 42, 1, 100, Some(200)));
    assert!(!gm::available(&source, 43, 1, 100, Some(200)));
    source.location_id = 200;
    source.flag_id = FLAG_HANGAR;
    assert!(gm::available(&source, 42, 1, 100, Some(200)));
    assert!(!gm::available(&source, 42, 1, 100, None));
    source.flag_id = FLAG_CARGO;
    assert!(!gm::available(&source, 42, 1, 100, Some(200)));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL and an existing item"]
async fn stale_move_snapshot_is_refused_without_changing_item() {
    let config = crate::config::tests::test_database();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
    tokio::spawn(async move { connection.await.expect("connection"); });
    let tx = client.transaction().await.expect("transaction");
    let row = tx.query_one("SELECT item_id, type_id, owner_id, location_id, flag_id, quantity, singleton FROM game.items ORDER BY item_id LIMIT 1 FOR UPDATE", &[]).await.expect("existing item");
    let mut snapshot = Item {
        item_id: row.get("item_id"), type_id: row.get("type_id"), owner_id: row.get("owner_id"),
        location_id: row.get("location_id"), flag_id: row.get("flag_id"), quantity: row.get("quantity"),
        singleton: row.get("singleton"), group_id: 0, category_id: 0, custom_info: None,
    };
    let original_owner = snapshot.owner_id;
    snapshot.owner_id = original_owner.wrapping_add(1);
    assert!(!inventory_dao::move_item(&tx, &snapshot, snapshot.location_id, snapshot.flag_id, snapshot.owner_id, snapshot.singleton).await.expect("guarded move"));
    let row = tx.query_one("SELECT owner_id FROM game.items WHERE item_id = $1", &[&snapshot.item_id]).await.expect("item remains");
    assert_eq!(row.get::<_, i64>("owner_id"), original_owner);
    tx.rollback().await.expect("rollback");
}

#[test]
fn remote_item_notifications_route_to_target_not_gm() {
    use crate::db::outbox::Notification;
    let event = Notification::DirectClient { event: "OnItemChange".into(), args: vec![] };
    assert!(matches!(gm::route_target(event, 42, Some(10)), Notification::Pilot { char_id: 42, .. }));
    let event = Notification::DirectClient { event: "OnItemChange".into(), args: vec![] };
    assert!(matches!(gm::route_target(event, 42, Some(42)), Notification::DirectClient { .. }));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL, loaded SDE and a fitted module with charges"]
async fn charge_unload_and_module_move_roll_back_together() {
    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = crate::db::Db::connect(&config).await.expect("database pool");
    let sde = Sde::load(&db).await.expect("loaded SDE");
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
    tokio::spawn(async move { connection.await.expect("connection"); });
    let tx = client.transaction().await.expect("transaction");
    let row = tx.query_one("SELECT i.item_id, i.owner_id, i.location_id, i.flag_id, c.type_id, c.quantity FROM game.items i JOIN game.module_charges c ON c.ship_id = i.location_id AND c.flag_id = i.flag_id WHERE c.quantity > 0 ORDER BY i.item_id LIMIT 1 FOR UPDATE OF i", &[]).await.expect("fitted module with charge");
    let id: i64 = row.get("item_id");
    let owner: i64 = row.get("owner_id");
    let ship: i64 = row.get("location_id");
    let flag: i32 = row.get("flag_id");
    let charge_type: i32 = row.get("type_id");
    let charge_quantity: i32 = row.get("quantity");
    let module = load_items(&tx, &sde, ItemQuery::Id(id)).await.expect("load module").pop().expect("module");
    let mut outbox: Outbox = Outbox::default();
    assert!(super::super::combat::targeting_charges::unload(&tx, &sde, &mut outbox, owner, ship, &module, ship, FLAG_CARGO).await.is_ok());
    assert!(move_core(&tx, &sde, &mut outbox, &module, owner, ship, FLAG_CARGO, None).await.is_ok());
    tx.rollback().await.expect("rollback");
    let row = client.query_one("SELECT i.owner_id, i.location_id, i.flag_id, c.type_id, c.quantity FROM game.items i JOIN game.module_charges c ON c.ship_id = i.location_id AND c.flag_id = i.flag_id WHERE i.item_id = $1", &[&id]).await.expect("original module and charge");
    assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (owner, ship, flag));
    assert_eq!((row.get::<_, i32>("type_id"), row.get::<_, i32>("quantity")), (charge_type, charge_quantity));
}

#[tokio::test]
#[ignore]
async fn reserved_shared_move_rolls_back_prior_writes_and_notifications() {
    use std::sync::{Arc, atomic::{AtomicBool, AtomicI64, Ordering}};
    use crate::{config::deployment::DatabaseConfig, dao::{items::transfers, trades}, db::Db};

    let cfg = DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() };
    let db = Db::connect(&cfg).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let type_id = sde.types.iter().next().expect("SDE types required").type_id;
    let written_id = Arc::new(AtomicI64::new(0));
    let dispatched = Arc::new(AtomicBool::new(false));
    let result: Result<(), CallError> = db.transaction(
        {
            let sde = sde.clone();
            let written_id = written_id.clone();
            move |tx, outbox: &mut Outbox| {
                let sde = sde.clone();
                let written_id = written_id.clone();
                Box::pin(async move {
                    let trade = trades::create(tx, 1, 2, 3, 1).await.map_err(db_err)?;
                    let staged = transfers::insert(tx, type_id, 2, trade.id, FLAG_HANGAR, 2).await.map_err(db_err)?;
                    let free = transfers::insert(tx, type_id, 2, 1, FLAG_HANGAR, 2).await.map_err(db_err)?;
                    written_id.store(free, Ordering::SeqCst);
                    let staged_item = load_items(tx, &sde, ItemQuery::Id(staged)).await.map_err(db_err)?.pop().unwrap();
                    let free_item = load_items(tx, &sde, ItemQuery::Id(free)).await.map_err(db_err)?.pop().unwrap();
                    for take in [None, Some(1)] {
                        let mut rejected: Outbox = Outbox::default();
                        assert!(matches!(move_core(tx, &sde, &mut rejected, &staged_item, 2, 1, FLAG_HANGAR, take).await,
                            Err(CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(messages::trade::TRADE_ITEM_RESERVED))]));
                        assert!(rejected.into_entries().is_empty());
                    }
                    let mut rejected: Outbox = Outbox::default();
                    assert!(matches!(move_core(tx, &sde, &mut rejected, &free_item, 2, trade.id, FLAG_HANGAR, None).await,
                        Err(CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(messages::trade::TRADE_ITEM_RESERVED))]));
                    assert!(rejected.into_entries().is_empty());
                    move_core(tx, &sde, outbox, &free_item, 2, 3, FLAG_HANGAR, None).await?;
                    move_core(tx, &sde, outbox, &staged_item, 2, 1, FLAG_HANGAR, None).await?;
                    Ok(())
                })
            }
        },
        {
            let dispatched = dispatched.clone();
            move |(), _outbox| async move {
                dispatched.store(true, Ordering::SeqCst);
                Ok(())
            }
        },
    ).await;
    assert!(matches!(result, Err(CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(messages::trade::TRADE_ITEM_RESERVED))]));
    assert!(!dispatched.load(Ordering::SeqCst));
    assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id = $1", &[&written_id.load(Ordering::SeqCst)]).await.unwrap().is_none());
}

async fn move_fingerprint(db: &crate::db::Db) -> Vec<String> {
    let mut result = Vec::new();
    for table in ["items", "module_charges", "weapon_banks", "pending_reloads"] {
        let query = format!("SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY row_to_json(t)::text), '')) AS fingerprint FROM game.{table} t");
        result.push(db.query_one(&query, &[]).await.unwrap().get("fingerprint"));
    }
    result
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn move_item_authorizes_sources_and_destinations() {
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let type_id = sde.types.iter().find(|t| t.category_id != Some(crate::consts::CATEGORY_MODULE) && t.category_id != Some(crate::consts::CATEGORY_SUBSYSTEM)).unwrap().type_id;
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let wreck_type = sde.types.iter().find(|t| t.group_id == Some(crate::consts::GROUP_WRECK)).unwrap().type_id;
    let container_type = sde.types.iter().find(|t| t.group_id == Some(crate::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('inventory-auth-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let mut chars = Vec::new();
    for _ in 0..2 {
        let row = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, corp_roles) VALUES ($1, 'inventory-auth-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2) RETURNING character_id", &[&account, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
        chars.push(row.get::<_, i64>("character_id"));
    }
    let (me, other) = (chars[0], chars[1]);
    let station = -100_i64;
    let system = -101_i64;
    let insert = |type_id, owner, location, flag| {
        let db = &db;
        async move { crate::dao::items::transfers::insert(db, type_id, owner, location, flag, 1).await.unwrap() }
    };
    let own_ship = insert(ship_type, me, station, FLAG_HANGAR).await;
    let foreign_ship = insert(ship_type, other, station, FLAG_HANGAR).await;
    let own_container = insert(container_type, me, station, FLAG_HANGAR).await;
    let foreign_container = insert(container_type, other, station, FLAG_HANGAR).await;
    let office = insert(type_id, 1, station, FLAG_HANGAR).await;
    crate::dao::offices::insert(&db, office, 1, station, 1).await.unwrap();
    let foreign_office = insert(type_id, 2, station, FLAG_HANGAR).await;
    crate::dao::offices::insert(&db, foreign_office, 2, station, 1).await.unwrap();
    let wreck = insert(wreck_type, other, system, FLAG_HANGAR).await;
    let can = insert(container_type, other, system, FLAG_HANGAR).await;
    db.execute("UPDATE game.items SET location_id = $2, x = 0, y = 0, z = 0 WHERE item_id = ANY($1)", &[&&[wreck, can, foreign_ship][..], &system]).await.unwrap();
    let mut foreign = Vec::new();
    for (location, flag) in [(foreign_ship, FLAG_CARGO), (foreign_ship, FLAG_FITTING_FIRST), (foreign_ship, crate::consts::FLAG_DRONE_BAY), (foreign_container, FLAG_HANGAR), (station, FLAG_HANGAR)] {
        foreign.push(insert(type_id, other, location, flag).await);
    }
    let own = insert(type_id, me, station, FLAG_HANGAR).await;
    let corp_item = insert(type_id, 1, office, FLAG_CORP_SAG1).await;
    let personal_office_item = insert(type_id, me, office, FLAG_CORP_SAG1).await;
    let loot = insert(type_id, other, wreck, FLAG_CARGO).await;
    let can_loot = insert(type_id, other, can, FLAG_CARGO).await;
    let cfg = Arc::new(crate::config::Config::default());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    world.system(system).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let login = crate::handshake::Login { user_id: account, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    let chat = crate::chat::Hub::new(&cfg.chat.domain);
    let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, db.clone(), sde, chat, world.clone(), login);
    client.session.apply(vec![("charid", Value::Int(me)), ("corpid", Value::Int(1)), ("stationid", Value::Int(station)), ("shipid", Value::Int(own_ship))]);
    let hangar = Container::Hangar { station_id: station, owner_id: me };
    let mut rejected = Vec::new();
    for &id in &foreign { rejected.push((hangar.clone(), id, None)); }
    rejected.push((hangar.clone(), foreign[4], Some(station)));
    rejected.push((Container::Item(foreign_ship), own, None));
    rejected.push((Container::Item(foreign_container), own, None));
    rejected.push((Container::Item(-999), own, None));
    rejected.push((Container::Hangar { station_id: station, owner_id: other }, own, None));
    rejected.push((Container::Hangar { station_id: station - 1, owner_id: me }, own, None));
    rejected.push((hangar.clone(), own, Some(station - 1)));
    for (destination, id, source) in rejected {
        let before = move_fingerprint(&db).await;
        client.outbox.clear();
        assert!(matches!(move_item(&mut client, &destination, id, source, None, None).await, Err(CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))]));
        assert_eq!(move_fingerprint(&db).await, before);
        assert!(client.outbox.is_empty());
    }
    let before = move_fingerprint(&db).await;
    client.outbox.clear();
    assert!(matches!(move_item(&mut client, &Container::Item(foreign_office), own, None, None, None).await, Err(CallError::User { msg, dict }) if msg == messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(messages::NOT_YOUR_CORPORATION_HANGAR))]));
    assert_eq!(move_fingerprint(&db).await, before);
    assert!(client.outbox.is_empty());
    db.execute("UPDATE game.characters SET corp_roles = 0 WHERE character_id = $1", &[&me]).await.unwrap();
    for id in [corp_item, personal_office_item] {
        let before = move_fingerprint(&db).await;
        client.outbox.clear();
        assert!(matches!(move_item(&mut client, &hangar, id, None, None, None).await, Err(CallError::User { msg, dict }) if msg == messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(messages::NO_CORPORATION_HANGAR_TAKE_ROLE))]));
        assert_eq!(move_fingerprint(&db).await, before);
        assert!(client.outbox.is_empty());
    }
    db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&me, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
    for (destination, location, flag) in [(Container::Item(own_ship), own_ship, FLAG_CARGO), (Container::Item(own_container), own_container, FLAG_HANGAR), (hangar.clone(), station, FLAG_HANGAR)] {
        assert!(move_item(&mut client, &destination, own, None, None, None).await.is_ok());
        let moved = load_items(&db, &client.sde, ItemQuery::Id(own)).await.unwrap().pop().unwrap();
        assert_eq!((moved.owner_id, moved.location_id, moved.flag_id), (me, location, flag));
    }
    assert!(move_item(&mut client, &hangar, corp_item, None, None, None).await.is_ok());
    let moved = load_items(&db, &client.sde, ItemQuery::Id(corp_item)).await.unwrap().pop().unwrap();
    assert_eq!((moved.owner_id, moved.location_id, moved.flag_id), (me, station, FLAG_HANGAR));
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&own_ship]).await.unwrap();
    let before = move_fingerprint(&db).await;
    client.outbox.clear();
    assert!(move_item(&mut client, &Container::Item(own_ship), own, None, Some(FLAG_FITTING_FIRST), None).await.is_err());
    assert_eq!(move_fingerprint(&db).await, before);
    assert!(client.outbox.is_empty());
    client.session.apply(vec![("stationid", Value::None), ("solarsystemid", Value::Int(system))]);
    let ball = |id, x| crate::space::Ball {
        id, type_id: 1, owner_id: me, corp_id: 1, alliance_id: 0, char_id: None,
        mode: crate::space::MODE_STOP, radius: 50.0, signature_radius: None, mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: vec![],
    };
    let (vessel, distant, positioned_ship, cargo_can) = (ball(own_ship, 0.0), ball(wreck, 2601.0), ball(foreign_ship, 0.0), ball(can, 2600.0));
    world.with(system, move |s| { s.insert(vessel); s.insert(distant); s.insert(positioned_ship); s.insert(cargo_can); }).await.unwrap();
    for id in [loot, foreign[0]] {
        let before = move_fingerprint(&db).await;
        client.outbox.clear();
        assert!(matches!(move_item(&mut client, &Container::Item(own_ship), id, None, None, None).await, Err(CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))]));
        assert_eq!(move_fingerprint(&db).await, before);
        assert!(client.outbox.is_empty());
    }
    let nearby = ball(wreck, 2600.0);
    world.with(system, move |s| { s.insert(nearby); }).await.unwrap();
    for id in [loot, can_loot] {
        assert!(move_item(&mut client, &Container::Item(own_ship), id, None, None, None).await.is_ok());
        let moved = load_items(&db, &client.sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap();
        assert_eq!((moved.owner_id, moved.location_id, moved.flag_id), (me, own_ship, FLAG_CARGO));
    }
    crate::dao::offices::delete(&db, office).await.unwrap();
    crate::dao::offices::delete(&db, foreign_office).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id = ANY($1) OR item_id = ANY($2)", &[&&chars[..], &&[office, foreign_office, corp_item][..]]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE user_id = $1", &[&account]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}
