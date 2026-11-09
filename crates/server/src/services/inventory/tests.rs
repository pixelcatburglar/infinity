//! Inventory regressions, including isolated database fixtures for guarded moves and rollback.
//! Guarded-move and charge-rollback tests own their rows; the latter requires loaded SDE types.

use super::{gm, CONTAINER_HANGAR, INVENTORY};
use super::access::Container;
use super::inv_inventory::{inventory, item_capacity_reply};
use super::movement::{move_core, move_item};
use super::destination::{Custody, Destination};
use super::stacks::plan_merges;
use crate::client::Client;
use infinity_core::consts::{CATEGORY_SHIP, FLAG_CARGO, FLAG_CORP_SAG1, FLAG_FITTING_FIRST, FLAG_HANGAR, ROLE_DIRECTOR, ROLE_HANGAR_CAN_TAKE_1, TYPE_PLASTIC_WRAP};
use infinity_core::consts::dogma::ATTR_CAPACITY;
use crate::dao::items::{inventory as inventory_dao, loading::{Item, ItemQuery}};
use crate::db::outbox::Outbox;
use infinity_core::messages;
use crate::sde::Sde;
use crate::services::{db_err, items::load_items, keyval, Call, CallError};
use marshal::Value;
mod reachability;
mod volume;
mod holds;
mod container_capacity;
mod container_crossing;
mod container_races;
mod reads;
mod validation;
mod read_access;
mod read_custody;
mod quantity_display;

pub(super) fn item(id: i64, quantity: i64) -> Item {
    Item { item_id: id, type_id: 1, owner_id: 1, location_id: 1, flag_id: 1, quantity, singleton: false, group_id: 1, category_id: 1, custom_info: None }
}

pub(super) async fn prepared_move<X: Send>(
    tx: &(impl crate::db::executor::Executor + ?Sized), sde: &std::sync::Arc<Sde>, outbox: &mut Outbox<X>,
    item: &Item, destination: &Destination, take: Option<i64>, custody: Custody, actor: i64,
) -> Result<Option<i64>, CallError> {
    use crate::dao::items::{deliveries, parent_locks::{LockRequest, LockStrength}};
    use super::destination::{lock_items, parent_requests};
    use super::super::combat::reload_recovery::TakePreparation;
    let source = deliveries::ancestry(tx, item.location_id).await.map_err(db_err)?;
    let target = deliveries::ancestry(tx, destination.location).await.map_err(db_err)?;
    let mut preparation = if custody == Custody::TakeSubtree && take.is_none() {
        Some(TakePreparation::capture(tx, sde, item.item_id).await?)
    } else { None };
    let mut requests = parent_requests(sde, source.iter().chain(&target).map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share)?;
    requests.extend(source.iter().chain(&target)
        .filter(|row| (row.item_id == item.location_id || row.item_id == destination.location)
            && super::hold_storage::is_ordinary_container(sde, row.type_id))
        .map(|row| LockRequest { item_id: row.item_id, strength: LockStrength::Update }));
    requests.push(LockRequest { item_id: item.item_id, strength: LockStrength::Update });
    let mut optional = Vec::new();
    if let Some(prep) = &preparation {
        requests.extend(prep.closure().iter().chain(prep.ancestors()).map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }));
        let mut mandatory: Vec<_> = requests.iter().map(|request| request.item_id).collect();
        mandatory.extend(prep.refund().mandatory_ids());
        optional = prep.refund().candidate_ids().into_iter().filter(|id| !mandatory.contains(id)).collect();
        requests.extend_from_slice(prep.refund().requests());
    }
    let locks = lock_items(tx, sde, &requests, &optional).await?;
    if deliveries::ancestry(tx, item.location_id).await.map_err(db_err)? != source
        || deliveries::ancestry(tx, destination.location).await.map_err(db_err)? != target {
        return Err(crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
    }
    if let Some(prep) = &preparation {
        prep.revalidate(tx).await?;
        prep.refund().revalidate(tx, sde, &locks).await?;
    }
    move_core(tx, sde, outbox, item, destination, take, custody, &locks, preparation.as_mut(), None, actor).await
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
fn merge_plan_deletes_sources_and_leaves_overflow_in_source() {
    let items = vec![item(1, 2), item(2, i64::MAX - 2)];
    let plan = plan_merges(&items, &[(1, 2, 99)]).ok().unwrap();
    assert_eq!(plan[&1].quantity, None);
    assert_eq!(plan[&2].quantity, Some(i64::MAX));
    let overflow = vec![item(1, 2), item(2, i64::MAX - 1)];
    let plan = plan_merges(&overflow, &[(1, 2, 2)]).unwrap();
    assert_eq!(plan[&1].quantity, Some(1));
    assert_eq!(plan[&2].quantity, Some(i64::MAX));
}

#[test]
fn merge_plan_refuses_cross_owner_stacks() {
    let first = item(1, 2);
    let mut second = item(2, 3);
    second.owner_id = 2;
    assert!(matches!(plan_merges(&[first, second], &[(1, 2, 1)]), Err(CallError::User { .. })));
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
#[ignore = "requires INFINITY_TEST_DATABASE_URL"]
async fn stale_move_snapshot_is_refused_without_changing_item() {
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.expect("database pool");
    let fixture = crate::services::tests::fixture::create(&db).await.expect("fixture");
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let id = crate::dao::items::transfers::insert(&test_db, 1, fixture.character, 1, FLAG_HANGAR, 1).await.expect("item");
        let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
        tokio::spawn(async move { connection.await.expect("connection"); });
        let tx = client.transaction().await.expect("transaction");
        let mut snapshot = item(id, 1);
        snapshot.owner_id = fixture.character.wrapping_add(1);
        snapshot.location_id = 1;
        snapshot.flag_id = FLAG_HANGAR;
        assert!(!inventory_dao::move_item(&tx, &snapshot, snapshot.location_id, snapshot.flag_id, snapshot.owner_id, snapshot.singleton).await.expect("guarded move"));
        let row = tx.query_one("SELECT owner_id FROM game.items WHERE item_id = $1", &[&id]).await.expect("item remains");
        assert_eq!(row.get::<_, i64>("owner_id"), fixture.character);
        tx.rollback().await.expect("rollback");
    }).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.expect("cleanup");
    result.expect("stale move regression");
}

#[test]
fn remote_item_notifications_route_to_target_not_gm() {
    use crate::db::outbox::Notification;
    let event = Notification::DirectClient { event: "OnItemChange".into(), args: vec![] };
    assert!(matches!(super::notifications::route_target(event, 42, Some(10)), Notification::Pilot { char_id: 42, .. }));
    let event = Notification::DirectClient { event: "OnItemChange".into(), args: vec![] };
    assert!(matches!(super::notifications::route_target(event, 42, Some(42)), Notification::DirectClient { .. }));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL and loaded SDE"]
async fn charge_unload_and_module_move_roll_back_together() {
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.expect("database pool");
    let fixture = crate::services::tests::fixture::create(&db).await.expect("fixture");
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = std::sync::Arc::new(Sde::load(&test_db).await.expect("loaded SDE"));
        let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)).expect("ship type").type_id;
        let module_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_MODULE) && t.volume.is_some_and(|v| v.is_finite() && v > 0.0)).expect("module type with volume").type_id;
        let charge_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.volume.is_some_and(|v| v.is_finite() && v > 0.0)).expect("charge type with volume").type_id;
        let owner = fixture.character;
        let ship = crate::dao::items::transfers::insert(&test_db, ship_type, owner, 1, FLAG_HANGAR, 1).await.expect("ship");
        test_db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&ship]).await.expect("assemble ship");
        let module_volume = crate::services::item_volume::item_volume(&sde, module_type, false, 1, None).unwrap_or_else(|_| panic!("module volume"));
        let charge_volume = crate::services::item_volume::item_volume(&sde, charge_type, false, 2, None).unwrap_or_else(|_| panic!("charge volume"));
        assert!(crate::dao::items::dogma::set_override(&test_db, ship, ATTR_CAPACITY, module_volume + charge_volume + 1.0).await.expect("cargo capacity override"));
        let id = crate::dao::items::transfers::insert(&test_db, module_type, owner, ship, FLAG_FITTING_FIRST, 1).await.expect("module");
        let flag = FLAG_FITTING_FIRST;
        let charge_quantity = 2;
        crate::dao::items::combat::apply_loads(&test_db, ship, &[(id, flag, charge_type, charge_quantity, 0)]).await.expect("charge");
        let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.expect("connect");
        tokio::spawn(async move { connection.await.expect("connection"); });
        let tx = client.transaction().await.expect("transaction");
        let module = load_items(&tx, &sde, ItemQuery::Id(id)).await.expect("load module").pop().expect("module");
        let mut outbox: Outbox = Outbox::default();
        use crate::dao::items::{deliveries, parent_locks::{LockRequest, LockStrength}};
        let destination = Destination::owned(owner, ship, FLAG_CARGO);
        let mut deposit = crate::services::items::DepositPreparation::capture(&tx, &sde, &destination, charge_type).await.unwrap();
        let ancestry = deliveries::ancestry(&tx, ship).await.unwrap();
        let mut requests = super::destination::parent_requests(&sde, ancestry.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share).unwrap();
        requests.push(LockRequest { item_id: id, strength: LockStrength::Update });
        let mandatory: Vec<_> = requests.iter().map(|request| request.item_id).collect();
        let optional: Vec<_> = deposit.candidate_id().filter(|id| !mandatory.contains(id)).into_iter().collect();
        requests.extend(deposit.candidate_request());
        let locks = super::destination::lock_items(&tx, &sde, &requests, &optional).await.unwrap();
        assert!(deliveries::ancestry(&tx, ship).await.unwrap() == ancestry);
        assert!(super::super::combat::targeting_charges::unload(&tx, &sde, &mut outbox, owner, ship, &module, ship, FLAG_CARGO, &mut deposit, &locks).await.is_ok());
        assert!(move_core(&tx, &sde, &mut outbox, &module, &destination, None, Custody::Preserve, &locks, None, None, owner).await.is_ok());
        tx.rollback().await.expect("rollback");
        let row = client.query_one("SELECT i.owner_id, i.location_id, i.flag_id, c.type_id, c.quantity FROM game.items i JOIN game.module_charges c ON c.ship_id = i.location_id AND c.flag_id = i.flag_id WHERE i.item_id = $1", &[&id]).await.expect("original module and charge");
        assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id")), (owner, ship, flag));
        assert_eq!((row.get::<_, i32>("type_id"), row.get::<_, i64>("quantity")), (charge_type, charge_quantity));
    }).await;
    db.execute("DELETE FROM game.module_charges WHERE ship_id IN (SELECT item_id FROM game.items WHERE owner_id = $1)", &[&fixture.character]).await.expect("charge cleanup");
    crate::services::tests::fixture::cleanup(&db, fixture).await.expect("cleanup");
    result.expect("charge rollback regression");
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
                    use crate::dao::items::{deliveries, parent_locks::{LockRequest, LockStrength}};
                    let lineage = deliveries::ancestry_many(tx, &[staged, free, trade.id, 1, 3]).await.map_err(db_err)?;
                    let mut requests = super::destination::parent_requests(&sde, lineage.iter().map(|row| (row.ancestor.item_id, row.ancestor.type_id)), LockStrength::Update, LockStrength::Share)?;
                    requests.extend([staged, free].into_iter().map(|item_id| LockRequest { item_id, strength: LockStrength::Update }));
                    let locks = super::destination::lock_items(tx, &sde, &requests, &[]).await?;
                    assert!(deliveries::ancestry_many(tx, &[staged, free, trade.id, 1, 3]).await.map_err(db_err)? == lineage);
                    for take in [None, Some(1)] {
                        let mut rejected: Outbox = Outbox::default();
                        assert!(matches!(move_core(tx, &sde, &mut rejected, &staged_item, &Destination::owned(2, 1, FLAG_HANGAR), take, Custody::Preserve, &locks, None, None, 2).await,
                            Err(CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(messages::trade::TRADE_ITEM_RESERVED))]));
                        assert!(rejected.into_entries().is_empty());
                    }
                    let mut rejected: Outbox = Outbox::default();
                    assert!(matches!(move_core(tx, &sde, &mut rejected, &free_item, &Destination::owned(2, trade.id, FLAG_HANGAR), None, Custody::Preserve, &locks, None, None, 2).await,
                        Err(CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(messages::trade::TRADE_ITEM_RESERVED))]));
                    assert!(rejected.into_entries().is_empty());
                    move_core(tx, &sde, outbox, &free_item, &Destination::owned(2, 3, FLAG_HANGAR), None, Custody::Preserve, &locks, None, None, 2).await?;
                    move_core(tx, &sde, outbox, &staged_item, &Destination::owned(2, 1, FLAG_HANGAR), None, Custody::Preserve, &locks, None, None, 2).await?;
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

async fn move_fingerprint(db: &crate::db::Db) -> Vec<Vec<String>> {
    let mut result = Vec::new();
    for table in ["items", "module_charges", "weapon_banks", "pending_reloads"] {
        result.push(crate::services::drones::tests::fixture::table_fingerprint(db, table).await.unwrap());
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
    let type_id = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let wreck_type = sde.types.iter().find(|t| t.group_id == Some(infinity_core::consts::GROUP_WRECK) && t.attribute(715).is_none()).unwrap().type_id;
    let container_type = sde.types.iter().find(|t| t.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER) && t.attribute(715).is_none()).unwrap().type_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('inventory-auth-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let mut chars = Vec::new();
    for _ in 0..2 {
        let row = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'inventory-auth-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await.unwrap();
        chars.push(row.get::<_, i64>("character_id"));
    }
    let (me, other) = (chars[0], chars[1]);
    let mut station = None;
    for candidate in sde.map.stations() {
        if crate::dao::offices::at(&db, 1, candidate.station_id).await.unwrap().is_none()
            && crate::dao::offices::at(&db, 2, candidate.station_id).await.unwrap().is_none() {
            station = Some(candidate.station_id);
            break;
        }
    }
    let station = station.expect("SDE must contain a station without either fixture corporation's office");
    let corp: i64 = db.query_one("INSERT INTO game.corporations(name,ticker,ceo_id,creator_id,station_id,created) VALUES ('inventory-auth-' || nextval('game.corporations_corporation_id_seq')::text,'IA' || currval('game.corporations_corporation_id_seq')::text,$1,$1,$2,0) RETURNING corporation_id", &[&other,&station]).await.unwrap().get("corporation_id");
    let foreign_corp: i64 = db.query_one("INSERT INTO game.corporations(name,ticker,ceo_id,creator_id,station_id,created) VALUES ('inventory-auth-' || nextval('game.corporations_corporation_id_seq')::text,'IF' || currval('game.corporations_corporation_id_seq')::text,$1,$1,$2,0) RETURNING corporation_id", &[&other,&station]).await.unwrap().get("corporation_id");
    db.execute("UPDATE game.characters SET corporation_id=$2,corp_roles_hq=$3 WHERE character_id=$1", &[&me,&corp,&ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
    let system = -101_i64;
    let insert = |type_id, owner, location, flag| {
        let db = &db;
        async move { crate::dao::items::transfers::insert(db, type_id, owner, location, flag, 1).await.unwrap() }
    };
    let own_ship = insert(ship_type, me, station, FLAG_HANGAR).await;
    let foreign_ship = insert(ship_type, other, station, FLAG_HANGAR).await;
    let own_container = insert(container_type, me, station, FLAG_HANGAR).await;
    let foreign_container = insert(container_type, other, station, FLAG_HANGAR).await;
    let office = insert(type_id, corp, station, FLAG_HANGAR).await;
    crate::dao::offices::insert(&db, office, corp, station, 1).await.unwrap();
    let foreign_office = insert(type_id, foreign_corp, station, FLAG_HANGAR).await;
    crate::dao::offices::insert(&db, foreign_office, foreign_corp, station, 1).await.unwrap();
    let wreck = insert(wreck_type, other, system, FLAG_HANGAR).await;
    let can = insert(container_type, other, system, FLAG_HANGAR).await;
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = ANY($1)", &[&&[own_ship, foreign_ship, own_container, foreign_container, wreck, can][..]]).await.unwrap();
    db.execute("UPDATE game.items SET location_id = $2, x = 0, y = 0, z = 0 WHERE item_id = ANY($1)", &[&&[wreck, can, foreign_ship][..], &system]).await.unwrap();
    let mut foreign = Vec::new();
    for (location, flag) in [(foreign_ship, FLAG_CARGO), (foreign_ship, FLAG_FITTING_FIRST), (foreign_ship, infinity_core::consts::FLAG_DRONE_BAY), (foreign_container, FLAG_HANGAR), (station, FLAG_HANGAR)] {
        foreign.push(insert(type_id, other, location, flag).await);
    }
    let own = insert(type_id, me, station, FLAG_HANGAR).await;
    let corp_item = insert(type_id, corp, office, FLAG_CORP_SAG1).await;
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
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, db.clone(), sde, chat, world.clone(), login);
    client.session.apply(vec![("charid", Value::Int(me)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(station)), ("shipid", Value::Int(own_ship))]);
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
        assert!(matches!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &destination, id, source, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await, Err(CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))]));
        assert_eq!(move_fingerprint(&db).await, before);
        assert!(client.outbox.is_empty());
    }
    let before = move_fingerprint(&db).await;
    client.outbox.clear();
    assert!(matches!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &Container::Item(foreign_office), own, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await, Err(CallError::User { msg, dict }) if msg == messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(messages::NOT_YOUR_CORPORATION_HANGAR))]));
    assert_eq!(move_fingerprint(&db).await, before);
    assert!(client.outbox.is_empty());
    db.execute("UPDATE game.characters SET corp_roles_hq = 0 WHERE character_id = $1", &[&me]).await.unwrap();
    for id in [corp_item, personal_office_item] {
        let before = move_fingerprint(&db).await;
        client.outbox.clear();
        assert!(matches!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &hangar, id, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await, Err(CallError::User { msg, dict }) if msg == messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(messages::NO_CORPORATION_HANGAR_TAKE_ROLE))]));
        assert_eq!(move_fingerprint(&db).await, before);
        assert!(client.outbox.is_empty());
    }
    db.execute("UPDATE game.characters SET corp_roles_hq = $2 WHERE character_id = $1", &[&me, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
    for (destination, location, flag) in [(Container::Item(own_ship), own_ship, FLAG_CARGO), (Container::Item(own_container), own_container, FLAG_HANGAR), (hangar.clone(), station, FLAG_HANGAR)] {
        assert!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &destination, own, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_ok());
        let moved = load_items(&db, &client.sde, ItemQuery::Id(own)).await.unwrap().pop().unwrap();
        assert_eq!((moved.owner_id, moved.location_id, moved.flag_id), (me, location, flag));
    }
    assert!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &hangar, corp_item, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_ok());
    let moved = load_items(&db, &client.sde, ItemQuery::Id(corp_item)).await.unwrap().pop().unwrap();
    assert_eq!((moved.owner_id, moved.location_id, moved.flag_id), (me, station, FLAG_HANGAR));
    let carrier_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(infinity_core::consts::dogma::ATTR_SHIP_MAINTENANCE_BAY_CAPACITY).is_some_and(|v| v > 0.0)).unwrap();
    let stored_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.volume.is_some_and(|v| v > 0.0 && v <= carrier_type.attribute(infinity_core::consts::dogma::ATTR_SHIP_MAINTENANCE_BAY_CAPACITY).unwrap())
        && super::holds::accepts(&client.sde, infinity_core::consts::FLAG_SHIP_HANGAR, t.type_id)).unwrap().type_id;
    let carrier = insert(carrier_type.type_id, me, station, FLAG_HANGAR).await;
    let stored = insert(stored_type, me, station, FLAG_HANGAR).await;
    let stored_cargo = insert(type_id, me, stored, FLAG_CARGO).await;
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = ANY($1)", &[&&[carrier, stored][..]]).await.unwrap();
    client.session.apply(vec![("shipid", Value::Int(carrier))]);
    let destination = Container::Item(carrier);
    assert!(matches!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, None, client.notify_tx.clone(), &destination, carrier, None, Some(infinity_core::consts::FLAG_SHIP_HANGAR), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await, Err(CallError::User { msg, .. }) if msg == messages::MSG_CAN_NOT_PLACE_ACTIVE_SHIP_IN_ANOTHER_HANGAR));
    assert!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, None, client.notify_tx.clone(), &destination, stored, None, Some(infinity_core::consts::FLAG_SHIP_HANGAR), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_ok());
    assert!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, None, client.notify_tx.clone(), &hangar, stored, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_ok());
    let content = load_items(&db, &client.sde, ItemQuery::Id(stored_cargo)).await.unwrap().pop().unwrap();
    assert_eq!((content.location_id, content.flag_id), (stored, FLAG_CARGO));
    client.session.apply(vec![("shipid", Value::Int(own_ship))]);
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&own_ship]).await.unwrap();
    let before = move_fingerprint(&db).await;
    client.outbox.clear();
    assert!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &Container::Item(own_ship), own, None, Some(FLAG_FITTING_FIRST), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_err());
    assert_eq!(move_fingerprint(&db).await, before);
    assert!(client.outbox.is_empty());
    db.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&own_ship, &system]).await.unwrap();
    client.session.apply(vec![("stationid", Value::None), ("solarsystemid", Value::Int(system))]);
    let ball = |id, x| crate::space::Ball {
        id, type_id: 1, owner_id: me, corp_id: corp, alliance_id: 0, char_id: (id == own_ship).then_some(me),
        mode: crate::space::MODE_STOP, radius: 50.0, signature_radius: None, mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: vec![],
    };
    let (vessel, distant, positioned_ship, cargo_can) = (ball(own_ship, 0.0), ball(wreck, 2601.0), ball(foreign_ship, 0.0), ball(can, 2600.0));
    world.with(system, move |s| { s.insert(vessel); s.insert(distant); s.insert(positioned_ship); s.insert(cargo_can); }).await.unwrap();
    for id in [loot, foreign[0]] {
        let before = move_fingerprint(&db).await;
        client.outbox.clear();
        assert!(matches!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &Container::Item(own_ship), id, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await, Err(CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))]));
        assert_eq!(move_fingerprint(&db).await, before);
        assert!(client.outbox.is_empty());
    }
    let nearby = ball(wreck, 2600.0);
    world.with(system, move |s| { s.insert(nearby); }).await.unwrap();
    for id in [loot, can_loot] {
        assert!(move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &Container::Item(own_ship), id, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_ok());
        let moved = load_items(&db, &client.sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap();
        assert_eq!((moved.owner_id, moved.location_id, moved.flag_id), (me, own_ship, FLAG_CARGO));
    }
    let fixture_chars = [me, other];
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        crate::services::tests::fixture::cleanup_progress(tx, &fixture_chars, &fixture_chars).await
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    crate::dao::offices::delete(&db, office).await.unwrap();
    crate::dao::offices::delete(&db, foreign_office).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id = ANY($1) OR item_id = ANY($2)", &[&&chars[..], &&[office, foreign_office, corp_item][..]]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id = ANY($1)", &[&vec![corp, foreign_corp]]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE user_id = $1", &[&account]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn headless_personal_stack_move_notifies_sender() {
    use std::sync::Arc;
    use crate::services::identity::Identity;
    use crate::services::tests::fixture;

    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let actor = fixture::create(&db).await.unwrap();
    let d = db.clone();
    let result = tokio::spawn(async move {
        let station = sde.map.stations().next().expect("SDE station").station_id;
        let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
        let stack_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
        let ship = crate::dao::items::transfers::insert(&d, ship_type, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
        d.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&ship]).await.unwrap();
        let stack = crate::dao::items::transfers::insert(&d, stack_type, actor.character, ship, FLAG_CARGO, 10).await.unwrap();
        let world = crate::space::World::new(d.clone(), sde.clone(), 1);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let identity = Identity { char_id: actor.character, corp_id: 1, ship_id: Some(ship), station_id: Some(station), system_id: None };
        let destination = Container::Hangar { station_id: station, owner_id: actor.character };
        let before = load_items(&d, &sde, ItemQuery::Id(stack)).await.unwrap().pop().unwrap();
        let moved = move_item(&identity, &d, &world, &sde, None, sender.clone(), &destination, stack, Some(ship), Some(FLAG_HANGAR), Some(4),
            |n| crate::services::effects::online::dispatch_notification(n, &world, &sender, Some(actor.character))).await.unwrap().unwrap();
        assert_ne!(moved, stack);
        let original = load_items(&d, &sde, ItemQuery::Id(stack)).await.unwrap().pop().unwrap();
        let split = load_items(&d, &sde, ItemQuery::Id(moved)).await.unwrap().pop().unwrap();
        assert_eq!((original.owner_id, original.location_id, original.flag_id, original.quantity), (actor.character, ship, FLAG_CARGO, 6));
        assert_eq!((split.owner_id, split.location_id, split.flag_id, split.quantity), (actor.character, station, FLAG_HANGAR, 4));
        let notices: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert_eq!(notices.iter().filter(|(event, _)| event == "OnItemChange").count(), 2);
        assert!(notices.iter().any(|(event, args)| event == "OnItemChange" && args[0] == original.row()));
        assert!(notices.iter().any(|(event, args)| event == "OnItemChange" && args[0] == split.row()));
        for (sequence, (event, args)) in notices.iter().enumerate() {
            if event != "OnItemChange" { continue; }
            let payload = Value::tuple(args.clone());
            let wire = marshal::dumps(&payload);
            let decoded = marshal::loads(&wire).unwrap();
            assert_eq!(decoded, payload);
            let decoded_row = &decoded.items().unwrap()[0];
            let descriptor = crate::services::item_descriptor();
            assert_eq!(descriptor.get(decoded_row, "quantity"), descriptor.get(&args[0], "quantity"));
            let wire_hex: String = wire.iter().map(|byte| format!("{byte:02x}")).collect();
            println!("CLIENT_QUANTITY_INPUT_V1 {}", serde_json::json!({
                "version": 1, "id": format!("inventory-split-{sequence}"), "scenario": "personal_stack_split",
                "producer_test": "inventory::tests::headless_personal_stack_move_notifies_sender",
                "producer_symbols": ["inventory::movement::move_item", "inventory::movement::move_core"],
                "kind": "notification", "sequence": sequence.to_string(),
                "recipient": {"kind": "pilot", "id": actor.character.to_string()},
                "session": {"source": "fixture_context", "character": actor.character.to_string(), "ship": ship.to_string(), "station": station.to_string()},
                "event": event, "payload_scope": "notification_args", "wire_hex": wire_hex, "wire_size": wire.len().to_string(),
                "before": {"source": "fixture_context", "item_id": before.item_id.to_string(), "location_id": before.location_id.to_string(), "flag_id": before.flag_id.to_string(), "quantity": before.quantity.to_string()},
                "after": {"source": "server_output", "items": [
                    {"item_id": original.item_id.to_string(), "location_id": original.location_id.to_string(), "flag_id": original.flag_id.to_string(), "quantity": original.quantity.to_string()},
                    {"item_id": split.item_id.to_string(), "location_id": split.location_id.to_string(), "flag_id": split.flag_id.to_string(), "quantity": split.quantity.to_string()}
                ]}, "rows": []
            }));
        }
    }).await;
    fixture::cleanup(&db, actor).await.unwrap();
    assert_eq!(fixture::remaining(&db, actor).await.unwrap(), 0);
    result.unwrap();
}
