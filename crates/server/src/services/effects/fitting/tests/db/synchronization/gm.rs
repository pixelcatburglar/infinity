//! Real GM fitting and unloading transactions against SDE-backed inventory.
use infinity_core::{consts::{FLAG_HANGAR, dogma::{ATTR_ARMOR_HP, ATTR_CPU, ATTR_CPU_LOAD, ATTR_CPU_OUTPUT, ATTR_POWER, ATTR_POWER_LOAD, ATTR_POWER_OUTPUT}}};
use crate::{services::{fit::Key, inventory::gm}};
use marshal::Value;
use super::fixture::{self, MovementFixture};

fn ship_batches(events: &[(String, Vec<Value>)], owner: i64, ship: i64, attr: i32) -> Vec<usize> {
    events.iter().enumerate().filter_map(|(index, (name, args))| {
        if name != "OnModuleAttributeChanges" { return None; }
        let [Value::List(rows)] = args.as_slice() else { return None; };
        rows.iter().any(|row| matches!(row, Value::Tuple(fields)
            if fields.get(1) == Some(&Value::Int(owner)) && fields.get(2) == Some(&Value::Int(ship))
                && fields.get(3) == Some(&Value::Int(i64::from(attr))))).then_some(index)
    }).collect()
}

async fn select_ship(f: &MovementFixture) -> i64 {
    let station = f.client.session.int("stationid").unwrap();
    assert_eq!(f.client.db.execute("UPDATE game.characters SET ship_id=$2,station_id=$3 WHERE character_id=$1",
        &[&f.rows.owner, &f.rows.ship, &station]).await.unwrap(), 1);
    station
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn foreign_gm_multi_fit_routes_one_final_batch_to_owner() {
    let attribute = ATTR_ARMOR_HP;
    let f = MovementFixture::create_passive(attribute).await;
    let db = f.client.db.clone();
    let world = f.client.world.clone();
    let rows = f.rows;
    let result = tokio::spawn(async move {
        let mut f = f;
        assert!(!f.requires_online, "GM fit must exercise a passive ship modifier");
        select_ship(&f).await;
        let before = f.fit(rows.ship).await.value(Key::Ship, attribute);
        let type_id: i32 = f.client.db.query_one("SELECT type_id FROM game.items WHERE item_id=$1", &[&rows.modules[0]]).await.unwrap().get("type_id");
        let (owner_tx, mut owner_rx) = tokio::sync::mpsc::unbounded_channel();
        f.client.world.register_pilot(rows.owner, owner_tx);
        f.client.session.apply(vec![("charid", Value::None)]);
        assert_eq!(gm::gm_fit(&mut f.client, rows.owner, type_id, None, 2).await.unwrap(), 2);
        let fitted = f.client.db.query("SELECT item_id,location_id,flag_id FROM game.items WHERE item_id=ANY($1) ORDER BY flag_id", &[&rows.modules.as_slice()]).await.unwrap();
        assert_eq!(fitted.len(), 2);
        for (index, row) in fitted.iter().enumerate() {
            assert_eq!(row.get::<_, i64>("location_id"), rows.ship);
            assert_eq!(row.get::<_, i32>("flag_id"), f.slot + index as i32);
        }
        let after = f.fit(rows.ship).await.value(Key::Ship, attribute);
        assert!(before.is_finite() && after.is_finite() && after > before);
        let mut events = Vec::new();
        while let Ok(event) = owner_rx.try_recv() { events.push(event); }
        assert_eq!(fixture::attribute_rows(&events, rows.owner, rows.ship, attribute), vec![(before, after)]);
        let batches = ship_batches(&events, rows.owner, rows.ship, attribute);
        assert_eq!(batches.len(), 1);
        let batch = batches[0];
        assert_eq!(events[..batch].iter().filter(|(name, _)| name == "OnItemChange").count(), 2);
        let [Value::List(changes)] = events[batch].1.as_slice() else { panic!("effective batch") };
        for module in rows.modules {
            assert!(changes.iter().any(|row| matches!(row, Value::Tuple(fields)
                if fields.get(1) == Some(&Value::Int(rows.owner)) && fields.get(2) == Some(&Value::Int(module)))));
        }
        assert!(f.drain().iter().all(|(name, _)| name != "OnModuleAttributeChanges"));
    }).await;
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&rows.owner]).await.unwrap();
    super::super::fixture::cleanup(&db, &world, rows).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn owner_gm_multi_unload_publishes_final_legal_fit_and_refuses_invalid() {
    let f = MovementFixture::create_online_resource().await;
    let db = f.client.db.clone();
    let world = f.client.world.clone();
    let rows = f.rows;
    let result = tokio::spawn(async move {
        let mut f = f;
        let station = select_ship(&f).await;
        assert!(f.requires_online);
        for (index, module) in rows.modules.iter().copied().enumerate() {
            for attr in [ATTR_CPU, ATTR_POWER] {
                assert!(crate::dao::items::dogma::set_override(&f.client.db, module, attr, 0.0).await.unwrap());
            }
            assert_eq!(f.move_module(rows.ship, rows.ship, f.slot + index as i32, module).await.ok().flatten(), Some(module));
            f.set_online(rows.ship, module, true).await.unwrap();
        }
        let before = f.fit(rows.ship).await;
        let old = before.value(Key::Ship, ATTR_CPU_OUTPUT);
        let online = crate::dao::items::dogma::online(&f.client.db, rows.ship).await.unwrap();
        assert!(rows.modules.iter().all(|id| online.iter().any(|entry| entry.item_id == *id)));
        let charges: i64 = f.client.db.query_one("SELECT count(*) AS count FROM game.module_charges WHERE ship_id=$1", &[&rows.ship]).await.unwrap().get("count");
        assert_eq!(charges, 0);
        f.drain();
        let charge_type = f.client.sde.types.iter().find(|ty| ty.published == Some(true)
            && ty.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)).expect("published charge in SDE").type_id;
        let charge = crate::dao::items::transfers::insert(&f.client.db, charge_type, rows.owner, rows.ship, fixture::CARGO, 2).await.unwrap();
        let snapshot = super::super::fixture::fingerprint(&f.client.db).await.unwrap();
        assert!(gm::gm_unload(&mut f.client, rows.ship, rows.owner, rows.modules.to_vec(), rows.ship, FLAG_HANGAR).await.is_err());
        assert_eq!(super::super::fixture::fingerprint(&f.client.db).await.unwrap(), snapshot);
        assert!(f.drain().is_empty());
        assert_eq!(gm::gm_unload(&mut f.client, rows.ship, rows.owner, rows.modules.to_vec(), station, FLAG_HANGAR).await.unwrap(), 2);
        let after = f.fit(rows.ship).await;
        let final_cpu = after.value(Key::Ship, ATTR_CPU_OUTPUT);
        assert!(final_cpu < old);
        assert!(after.value(Key::Ship, ATTR_CPU_LOAD) <= final_cpu);
        assert!(after.value(Key::Ship, ATTR_POWER_LOAD) <= after.value(Key::Ship, ATTR_POWER_OUTPUT));
        assert!(crate::dao::items::dogma::online(&f.client.db, rows.ship).await.unwrap().iter().all(|entry| !rows.modules.contains(&entry.item_id)));
        let items = f.client.db.query("SELECT location_id,flag_id,online,quantity FROM game.items WHERE item_id=ANY($1)", &[&rows.modules.as_slice()]).await.unwrap();
        assert_eq!(items.len(), 2);
        for row in items {
            assert_eq!(row.get::<_, i64>("location_id"), station);
            assert_eq!(row.get::<_, i32>("flag_id"), FLAG_HANGAR);
            assert!(!row.get::<_, bool>("online"));
            assert_eq!(row.get::<_, i64>("quantity"), 1);
        }
        let final_charges: i64 = f.client.db.query_one("SELECT count(*) AS count FROM game.module_charges WHERE ship_id=$1", &[&rows.ship]).await.unwrap().get("count");
        assert_eq!(final_charges, charges);
        let cargo = f.client.db.query_one("SELECT owner_id,location_id,flag_id,quantity FROM game.items WHERE item_id=$1", &[&charge]).await.unwrap();
        assert_eq!((cargo.get::<_, i64>("owner_id"), cargo.get::<_, i64>("location_id"),
            cargo.get::<_, i32>("flag_id"), cargo.get::<_, i64>("quantity")), (rows.owner, rows.ship, fixture::CARGO, 2));
        let events = f.drain();
        assert_eq!(fixture::attribute_rows(&events, rows.owner, rows.ship, ATTR_CPU_OUTPUT), vec![(old, final_cpu)]);
        assert_eq!(ship_batches(&events, rows.owner, rows.ship, ATTR_CPU_OUTPUT).len(), 1);
    }).await;
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&rows.owner]).await.unwrap();
    super::super::fixture::cleanup(&db, &world, rows).await.unwrap();
    result.unwrap();
}
