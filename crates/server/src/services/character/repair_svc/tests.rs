use super::*;

fn row(item: i64, attribute: i32, damage: f64, max: f64, unit: f64, module: bool) -> Quote {
    Quote { item, type_id: 1, group: 1, category: 1, attribute, damage, max, unit, module }
}

#[test]
fn ship_layers_are_one_quote_and_one_rounded_cost() {
    let rows = vec![row(1, ATTR_HULL_DAMAGE, 1.2, 100.0, 2.0, false), row(1, ATTR_ARMOR_DAMAGE, 2.2, 100.0, 2.0, false)];
    let wire = wire_rows(&rows);
    assert_eq!(wire, vec![keyval([
        ("itemID", Value::Int(1)), ("typeID", Value::Int(1)), ("groupID", Value::Int(1)),
        ("damage", Value::Float(3.4000000000000004)), ("maxHealth", Value::Float(100.0)),
        ("repairable", Value::Bool(true)), ("costToRepairOneUnitOfDamage", Value::Float(2.0)),
    ])]);
    assert!(wire_rows(&[]).is_empty());
    let module = row(2, ATTR_HULL_DAMAGE, 1.0, 10.0, 0.0, true);
    assert_eq!(wire_rows(&[module]), vec![keyval([
        ("itemID", Value::Int(2)), ("typeID", Value::Int(1)), ("groupID", Value::Int(1)),
        ("damage", Value::Float(1.0)), ("maxHealth", Value::Float(10.0)),
        ("repairable", Value::Bool(false)), ("costToRepairOneUnitOfDamage", Value::Float(0.0)),
    ])]);
    assert_eq!(total_cost(&rows), 8.0);
    assert_eq!(fraction(4.0, total_cost(&rows), false), Some(0.5));
}

#[test]
fn payment_validation_refuses_invalid_and_module_partial() {
    for payment in [f64::NAN, f64::INFINITY, -1.0, 0.0, 11.0] {
        assert_eq!(fraction(payment, 10.0, false), None);
    }
    assert_eq!(fraction(5.0, 10.0, true), None);
    assert_eq!(fraction(10.0, 10.0, true), Some(1.0));
}

#[test]
fn reordered_full_payment_uses_wallet_precision() {
    let client = 0.1_f64 + 0.2 + 0.3;
    let server = 0.3_f64 + 0.2 + 0.1;
    assert_ne!(client, server);
    assert_eq!(fraction(client, server, true), Some(1.0));
    assert_eq!(fraction(client, server, false), Some(1.0));
    assert_eq!(fraction(0.304, server, false), Some(0.5));
    assert_eq!(fraction(0.004, server, false), None);
    assert_eq!(fraction(0.61, server, false), None);
    assert_eq!(fraction(0.59, server, true), None);
}

#[test]
fn client_repairable_whitelist_requires_singleton() {
    for category in [CATEGORY_DEPLOYABLE, CATEGORY_SHIP, CATEGORY_DRONE, CATEGORY_STARBASE, CATEGORY_MODULE, CATEGORY_STRUCTURE_MODULE] {
        assert!(repairable(true, Some(category), None));
        assert!(!repairable(false, Some(category), None));
    }
    for group in CARGO_CONTAINER_GROUPS.into_iter().chain([GROUP_TOOL]) {
        assert!(repairable(true, None, Some(group)));
        assert!(!repairable(false, None, Some(group)));
    }
    assert!(!repairable(true, None, None));
    assert!(!repairable(true, Some(999), Some(999)));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn station_settlement_is_atomic_and_checks_context() {
    use crate::{config::{Config, deployment::DatabaseConfig}, conn::Conn, db::Db, handshake::Login, space::World};
    crate::tests::init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ty = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(ATTR_STRUCTURE_HP).unwrap_or(0.0) > 10.0 && t.attribute(ATTR_ARMOR_HP).unwrap_or(0.0) > 10.0 && computed_base_price(&sde, t.type_id) > 0.0).expect("priced ship type").type_id;
    let station = sde.map.stations().next().unwrap().station_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let owner: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 1000000000000) RETURNING character_id", &[&account, &station]).await.unwrap().get("character_id");
    let ship: i64 = db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, 1, true) RETURNING item_id", &[&ty, &owner, &station, &FLAG_HANGAR]).await.unwrap().get("item_id");
    dogma::set_override(&db, ship, ATTR_HULL_DAMAGE, 2.0).await.unwrap();
    dogma::set_override(&db, ship, ATTR_ARMOR_DAMAGE, 3.0).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, addr) = listener.accept().await.unwrap();
    let login = Login { user_id: account, user_name: account.to_string(), role: 0, client_id: owner, session_id: owner, language_id: "EN".into() };
    let mut c = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), db.clone(), sde.clone(), crate::chat::Hub::new("localhost", db.clone(), world.clone()), world, login);
    c.session.apply(vec![("charid", Value::Int(owner)), ("stationid", Value::Int(station)), ("shipid", Value::Int(ship))]);
    let bind = Value::tuple([Value::Int(station), Value::Int(station), Value::Int(0)]);
    let kwargs = Value::Dict(vec![]);
    let requested = Value::list([Value::Int(ship)]);
    let before = wallets::balance(&db, owner).await.ok().unwrap();
    let grouped = db.transaction(|tx, _: &mut Outbox| { let sde = sde.clone(); Box::pin(async move { quotes(tx, sde, &[ship], owner, station).await }) }, |value, _| async move { Ok(value) }).await.ok().unwrap();
    let cost = total_cost(&grouped[0].1);
    let paid = market::round2(cost / 2.0);
    let portion = paid / market::round2(cost);
    let quote_args = [requested.clone()];
    let quote_call = Call { service: "repairSvc", method: "GetRepairQuotes", args: &quote_args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    let ty_data = sde.types.get(ty).unwrap();
    let row = &grouped[0].1[0];
    let fit = Fit::load(&db, sde.clone(), owner, ship).await.ok().unwrap();
    let max_health = fit.value(Key::Ship, ATTR_STRUCTURE_HP) + fit.value(Key::Ship, ATTR_ARMOR_HP);
    assert_eq!(handle(&c, &quote_call).await.ok().unwrap(), Value::Dict(vec![(Value::Int(ship), Value::list([keyval([
        ("itemID", Value::Int(ship)), ("typeID", Value::Int(ty as i64)),
        ("groupID", Value::Int(ty_data.group_id.unwrap_or(0) as i64)),
        ("damage", Value::Float(5.0)), ("maxHealth", Value::Float(max_health)),
        ("repairable", Value::Bool(true)), ("costToRepairOneUnitOfDamage", Value::Float(row.unit)),
    ])]))]));
    let args = [requested.clone(), Value::Float(cost / 2.0)];
    let call = Call { service: "repairSvc", method: "RepairItemsInStation", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    let over = [requested.clone(), Value::Float(cost * 2.0)];
    let bad_call = Call { service: "repairSvc", method: "RepairItemsInStation", args: &over, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert!(crate::services::dispatch(&mut c, &bad_call).await.is_err());
    let wrong_bind = Value::tuple([Value::Int(station + 1), Value::Int(station), Value::Int(0)]);
    let bad_call = Call { bind_params: Some(&wrong_bind), ..bad_call };
    assert!(handle(&c, &bad_call).await.is_err());
    db.execute("UPDATE game.items SET owner_id = $2 WHERE item_id = $1", &[&ship, &(owner + 1)]).await.unwrap();
    assert!(handle(&c, &call).await.is_err());
    db.execute("UPDATE game.items SET owner_id = $2 WHERE item_id = $1", &[&ship, &owner]).await.unwrap();
    db.execute("UPDATE game.characters SET balance = 0 WHERE character_id = $1", &[&owner]).await.unwrap();
    assert!(handle(&c, &call).await.is_err());
    assert_eq!(wallets::balance(&db, owner).await.ok().unwrap(), crate::isk::IskAmount::ZERO);
    let unchanged = dogma::overrides(&db, ship).await.unwrap();
    assert_eq!(unchanged.iter().find(|r| r.attribute_id == ATTR_HULL_DAMAGE).unwrap().value, 2.0);
    assert_eq!(unchanged.iter().find(|r| r.attribute_id == ATTR_ARMOR_DAMAGE).unwrap().value, 3.0);
    db.execute("UPDATE game.characters SET balance = $2::text::numeric WHERE character_id = $1", &[&owner, &before.decimal()]).await.unwrap();
    assert_eq!(wallets::balance(&db, owner).await.ok().unwrap(), before);
    assert!(handle(&c, &call).await.is_ok());
    assert_eq!(wallets::balance(&db, owner).await.ok().unwrap(), before.checked_sub(crate::isk::IskAmount::from_isk(paid).unwrap()).unwrap());
    let damage = dogma::overrides(&db, ship).await.unwrap();
    assert!((damage.iter().find(|r| r.attribute_id == ATTR_HULL_DAMAGE).unwrap().value - 2.0 * (1.0 - portion)).abs() < 1e-9);
    assert!((damage.iter().find(|r| r.attribute_id == ATTR_ARMOR_DAMAGE).unwrap().value - 3.0 * (1.0 - portion)).abs() < 1e-9);
    let grouped = db.transaction(|tx, _: &mut Outbox| { let sde = sde.clone(); Box::pin(async move { quotes(tx, sde, &[ship], owner, station).await }) }, |value, _| async move { Ok(value) }).await.ok().unwrap();
    let remaining = total_cost(&grouped[0].1);
    let stale_args = [requested.clone(), Value::Float(cost)];
    let stale_call = Call { service: "repairSvc", method: "RepairItemsInStation", args: &stale_args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert!(handle(&c, &stale_call).await.is_err());
    let full_args = [requested.clone(), Value::Float(remaining)];
    let full_call = Call { args: &full_args, ..call };
    assert!(handle(&c, &full_call).await.is_ok());
    let balance = wallets::balance(&db, owner).await.ok().unwrap();
    assert_eq!(balance, before.checked_sub(crate::isk::IskAmount::from_isk(paid).unwrap()).unwrap().checked_sub(crate::isk::IskAmount::from_isk(market::round2(remaining)).unwrap()).unwrap());
    assert_eq!(handle(&c, &quote_call).await.ok().unwrap(), Value::Dict(vec![(Value::Int(ship), Value::list([]))]));
    let invalid = [requested, Value::Float(f64::NAN)];
    let invalid_call = Call { args: &invalid, ..full_call };
    assert!(handle(&c, &invalid_call).await.is_err());
    assert_eq!(wallets::balance(&db, owner).await.ok().unwrap(), balance);
    let module_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE) && t.attribute(ATTR_STRUCTURE_HP).unwrap_or(0.0) > 2.0 && computed_base_price(&sde, t.type_id) > 0.0).expect("priced module type").type_id;
    let module: i64 = db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, 1, true) RETURNING item_id", &[&module_type, &owner, &station, &FLAG_HANGAR]).await.unwrap().get("item_id");
    dogma::set_override(&db, module, ATTR_HULL_DAMAGE, 1.0).await.unwrap();
    let grouped = db.transaction(|tx, _: &mut Outbox| { let sde = sde.clone(); Box::pin(async move { quotes(tx, sde, &[module], owner, station).await }) }, |value, _| async move { Ok(value) }).await.ok().unwrap();
    let module_cost = total_cost(&grouped[0].1);
    let partial = [Value::list([Value::Int(module)]), Value::Float(module_cost / 2.0)];
    let module_call = Call { service: "repairSvc", method: "RepairItemsInStation", args: &partial, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert!(handle(&c, &module_call).await.is_err());
    assert_eq!(wallets::balance(&db, owner).await.ok().unwrap(), balance);
    assert_eq!(dogma::overrides(&db, module).await.unwrap().iter().find(|r| r.attribute_id == ATTR_HULL_DAMAGE).unwrap().value, 1.0);
    let full_module = [Value::list([Value::Int(module)]), Value::Float(module_cost)];
    let full_module_call = Call { args: &full_module, ..module_call };
    assert!(handle(&c, &full_module_call).await.is_ok());
    assert_eq!(wallets::balance(&db, owner).await.ok().unwrap(), balance.checked_sub(crate::isk::IskAmount::from_isk(market::round2(module_cost)).unwrap()).unwrap());
    assert_eq!(dogma::overrides(&db, module).await.unwrap().iter().find(|r| r.attribute_id == ATTR_HULL_DAMAGE).unwrap().value, 0.0);
    db.execute("DELETE FROM game.item_attributes WHERE item_id = $1", &[&module]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1", &[&module]).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}
