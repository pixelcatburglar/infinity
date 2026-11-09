use super::*;

mod reservations;
mod guard_race;
mod lifecycle;

#[test]
fn attributes_are_checked_and_positive_units_round_up() {
    assert!(requirement(0.0, 0.0).unwrap().is_none());
    assert!(requirement(17.0, 0.0).unwrap().is_none());
    assert_eq!(requirement(17.0, 0.01).unwrap(), Some((17, 1)));
    assert_eq!(requirement(17.0, 2.1).unwrap(), Some((17, 3)));
    for (ty, qty) in [(0.0, 1.0), (1.5, 1.0), (-1.0, 1.0), (1.0, -1.0), (f64::NAN, 0.0), (f64::INFINITY, 0.0), (f64::NEG_INFINITY, 1.0), (1.0, f64::NAN), (1.0, f64::INFINITY), (1.0, f64::NEG_INFINITY), (i32::MAX as f64 + 1.0, 1.0), (1.0, i64::MAX as f64)] {
        assert!(requirement(ty, qty).is_err(), "{ty} {qty}");
    }
}

#[test]
fn client_hold_order_uses_last_duplicate_key() {
    assert_eq!(holds(Some(GROUP_ICE_PRODUCT)), &[FLAG_SPECIALIZED_FUEL_BAY, FLAG_CARGO]);
    assert_eq!(holds(Some(GROUP_FUEL_BLOCK)), &[FLAG_COLONY_RESOURCES_HOLD, FLAG_CARGO]);
    assert_eq!(holds(None), &[FLAG_CARGO]);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn ordered_stacks_shortage_and_guarded_prefix_rollback() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&cfg).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let fuel = sde.types.iter().find(|ty| ty.group_id == Some(GROUP_ICE_PRODUCT)).expect("ice product in SDE").type_id;
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let (owner, ship) = (-9001_i64, -9002_i64);
    let insert = "INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,false) RETURNING item_id";
    let cargo: i64 = tx.query_one(insert, &[&fuel, &owner, &ship, &FLAG_CARGO, &5_i64]).await.unwrap().get("item_id");
    let first: i64 = tx.query_one(insert, &[&fuel, &owner, &ship, &FLAG_SPECIALIZED_FUEL_BAY, &2_i64]).await.unwrap().get("item_id");
    let second: i64 = tx.query_one(insert, &[&fuel, &owner, &ship, &FLAG_SPECIALIZED_FUEL_BAY, &3_i64]).await.unwrap().get("item_id");
    let flags = [FLAG_CARGO, FLAG_SPECIALIZED_FUEL_BAY];
    let items = load_items(&tx, &sde, ItemQuery::LocationOwnerFlags(ship, owner, &flags)).await.unwrap();
    let changes = plan(&BTreeMap::from([(fuel, 6)]), &items, &[second], &sde, owner).unwrap();
    assert_eq!(changes[&first].quantity, None);
    assert_eq!(changes[&cargo].quantity, Some(1));
    assert!(!changes.contains_key(&second));
    assert!(plan(&BTreeMap::from([(fuel, 8)]), &items, &[second], &sde, owner).is_err());
    let block = sde.types.iter().find(|ty| ty.group_id == Some(GROUP_FUEL_BLOCK)).expect("fuel block in SDE").type_id;
    let generic = sde.types.iter().find(|ty| ty.group_id != Some(GROUP_FUEL_BLOCK) && ty.group_id != Some(GROUP_ICE_PRODUCT)).expect("generic type in SDE").type_id;
    let base = items[0].clone();
    let candidate = |id, type_id, flag_id, quantity, owner_id, singleton| Item {
        item_id: id, type_id, flag_id, quantity, owner_id, singleton, ..base.clone()
    };
    let candidates = vec![
        candidate(1, block, infinity_core::consts::FLAG_FLEET_HANGAR, 20, owner, false),
        candidate(2, block, FLAG_CARGO, 5, owner, false),
        candidate(3, block, FLAG_COLONY_RESOURCES_HOLD, 2, owner, false),
        candidate(4, generic, FLAG_SPECIALIZED_FUEL_BAY, 20, owner, false),
        candidate(5, generic, FLAG_CARGO, 3, owner, false),
        candidate(6, generic, FLAG_CARGO, 20, owner + 1, false),
        candidate(7, generic, FLAG_CARGO, 20, owner, true),
        candidate(8, generic, FLAG_CARGO, 0, owner, false),
        candidate(9, generic, FLAG_CARGO, -1, owner, false),
        candidate(10, generic, FLAG_CARGO, 20, owner, false),
    ];
    let mixed = plan(&BTreeMap::from([(block, 4), (generic, 3)]), &candidates, &[10], &sde, owner).unwrap();
    assert_eq!(mixed.len(), 3);
    assert_eq!(mixed[&3].quantity, None);
    assert_eq!(mixed[&2].quantity, Some(3));
    assert_eq!(mixed[&5].quantity, None);
    assert!(plan(&BTreeMap::from([(generic, 4)]), &candidates, &[10], &sde, owner).is_err());
    for quantity in [i64::from(i32::MAX) + 1, 1_i64 << 32, (1_i64 << 53) + 1, i64::MAX] {
        let wide = [candidate(11, generic, FLAG_CARGO, quantity, owner, false)];
        let changes = plan(&BTreeMap::from([(generic, quantity - 1)]), &wide, &[], &sde, owner).unwrap();
        assert_eq!(changes[&11].quantity, Some(1));
    }
    let inner = tx.transaction().await.unwrap();
    inner.execute("UPDATE game.items SET quantity = quantity + 1 WHERE item_id = $1", &[&first]).await.unwrap();
    assert!(!quantities::apply(&inner, &changes).await.unwrap());
    inner.rollback().await.unwrap();
    let restored = load_items(&tx, &sde, ItemQuery::LocationOwnerFlags(ship, owner, &flags)).await.unwrap();
    assert_eq!(restored.iter().map(|item| (item.item_id, item.quantity)).collect::<Vec<_>>(), vec![(cargo, 5), (first, 2), (second, 3)]);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn effective_bank_requirements_commit_once_per_cycle_and_notify() {
    use std::sync::Arc;
    use infinity_core::{consts::{CATEGORY_MODULE, CATEGORY_SHIP}};
    use crate::{dao::items::dogma, db::Db, sde::Sde, space::World};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let module_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE)).unwrap().type_id;
    let fuel = sde.types.iter().find(|ty| ty.group_id == Some(GROUP_ICE_PRODUCT)).unwrap().type_id;
    let (account, owner): (i64, i64) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('fuel-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let owner = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'fuel-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        Ok::<_, crate::db::Error>((account, owner))
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    let result = crate::tests::catch_scenario(async {
    let insert = "INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,$5,$6,true) RETURNING item_id";
    let ship: i64 = db.query_one(insert, &[&ship_type, &owner, &1_i64, &0_i32, &1_i64, &true]).await.unwrap().get("item_id");
    let mut modules = Vec::new();
    for flag in [27_i32, 28_i32] {
        let id: i64 = db.query_one(insert, &[&module_type, &owner, &ship, &flag, &1_i64, &true]).await.unwrap().get("item_id");
        dogma::set_override(&db, id, ATTR_CONSUMPTION_TYPE, f64::from(fuel)).await.unwrap();
        dogma::set_override(&db, id, ATTR_CONSUMPTION_QUANTITY, 1.5).await.unwrap();
        modules.push(Module { id, type_id: module_type, flag });
    }
    let stack: i64 = db.query_one(insert, &[&fuel, &owner, &ship, &FLAG_CARGO, &8_i64, &false]).await.unwrap().get("item_id");
    let world = World::new(db.clone(), sde.clone(), 1);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let cx = Ctx { db: db.clone(), world, notify: sender, char_id: owner, ship_id: ship, system_id: 1 };
    let fit = Fit::load(&db, sde, owner, ship).await.unwrap();
    consume_cycle(&cx, &fit, &modules).await.unwrap();
    let remaining = load_items(&db, cx.world.sde(), ItemQuery::Id(stack)).await.unwrap().remove(0);
    assert_eq!(remaining.quantity, 4);
    let (name, args) = receiver.try_recv().unwrap();
    assert_eq!(name, "OnItemChange");
    assert_eq!(args, vec![remaining.row(), Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int(8))]), Value::None]);
    consume_cycle(&cx, &fit, &modules).await.unwrap();
    assert!(load_items(&db, cx.world.sde(), ItemQuery::Id(stack)).await.unwrap().is_empty());
    let (name, args) = receiver.try_recv().unwrap();
    assert_eq!(name, "OnItemChange");
    let removed = Item { location_id: infinity_core::consts::LOCATION_JUNKYARD, ..remaining };
    assert_eq!(args, vec![removed.row(), Value::Dict(vec![(Value::Int(infinity_core::consts::IX_LOCATION_ID), Value::Int(ship))]), Value::None]);
    assert!(consume_cycle(&cx, &fit, &modules).await.is_err());
    assert!(receiver.try_recv().is_err());
    for module in &modules {
        dogma::set_override(&db, module.id, ATTR_CONSUMPTION_QUANTITY, 4_611_686_018_427_387_904.0).await.unwrap();
    }
    let overflow_fit = Fit::load(&db, cx.world.sde().clone(), owner, ship).await.unwrap();
    assert!(matches!(consume_cycle(&cx, &overflow_fit, &modules).await, Err(CallError::Internal(_))));
    assert!(cx.world.sde().types.get(i32::MAX).is_none(), "unknown fuel type fixture must not exist in SDE");
    dogma::set_override(&db, modules[0].id, ATTR_CONSUMPTION_TYPE, f64::from(i32::MAX)).await.unwrap();
    dogma::set_override(&db, modules[0].id, ATTR_CONSUMPTION_QUANTITY, 1.0).await.unwrap();
    let unknown_fit = Fit::load(&db, cx.world.sde().clone(), owner, ship).await.unwrap();
    assert!(matches!(consume_cycle(&cx, &unknown_fit, &modules[..1]).await, Err(CallError::Internal(_))));
    assert!(receiver.try_recv().is_err());
    }).await;
    let cleanup = crate::tests::catch_scenario(async {
        db.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&owner]).await.unwrap();
        db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&owner]).await.unwrap();
        db.execute("DELETE FROM game.characters WHERE character_id=$1 AND user_id=$2", &[&owner, &account]).await.unwrap();
        db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    }).await;
    if let Err(cleanup) = cleanup {
        if let Err(original) = result { eprintln!("fixture cleanup also panicked"); std::panic::resume_unwind(original); }
        std::panic::resume_unwind(cleanup);
    }
    if let Err(original) = result { std::panic::resume_unwind(original); }
}
