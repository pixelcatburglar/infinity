use super::*;
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, CATEGORY_SUBSYSTEM, FLAG_CARGO, FLAG_HI_SLOT_FIRST, FLAG_RIG_FIRST, FLAG_SUBSYSTEM_FIRST, GM_ROLES};
use infinity_core::consts::dogma::{ATTR_ARMOR_HP, ATTR_CAPACITOR_CAPACITY, ATTR_CHARGE, ATTR_HEAT_HI, ATTR_HEAT_LOW, ATTR_HEAT_MED, ATTR_MASS, ATTR_RECHARGE_RATE, ATTR_SHIELD_RECHARGE_RATE, ATTR_STRUCTURE_HP};
use crate::config::deployment::DatabaseConfig;
use crate::db::Db;
use crate::services::{tests::fixture, value_field, wallets, Call};
use crate::space::World;
use crate::tests::init_config;

/// Insert one singleton item and return its ID.
async fn item(db: &Db, type_id: i32, owner: i64, location: i64, flag: i32) -> i64 {
    db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, 1, true) RETURNING item_id",
        &[&type_id, &owner, &location, &flag]).await.unwrap().get("item_id")
}

/// Dock the fixture character in a damaged ship carrying a burnt hi-slot module,
/// a damaged rig and subsystem, and a damaged cargo twin. Real categories are
/// required: every fitted member is covered, cargo is not.
async fn docked_ship(db: &Db, sde: Arc<Sde>, owner: i64) -> (i64, i64, [i64; 3], i64, [f64; 3], f64) {
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_RECHARGE_RATE, ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE]
            .iter().all(|&a| t.attribute(a).unwrap_or(0.0) > 10.0)).expect("ship type").type_id;
    let pick = |flag| sde.types.iter().find(|t| t.category_id == Some(match flag {
        f if f == FLAG_SUBSYSTEM_FIRST => CATEGORY_SUBSYSTEM,
        _ => CATEGORY_MODULE,
    }) && t.attribute(ATTR_STRUCTURE_HP).unwrap_or(0.0) > 10.0
        && slot_effect(&sde.dogma, flag).is_some_and(|id| t.effects().iter().any(|effect| effect.effect_id == id))).expect("slot module type").type_id;
    let station = sde.map.stations().next().unwrap().station_id;
    let ship = item(db, ship_type, owner, station, FLAG_HANGAR).await;
    db.execute("UPDATE game.characters SET station_id = $2, ship_id = $3 WHERE character_id = $1", &[&owner, &station, &ship]).await.unwrap();
    let mut modules = Vec::new();
    for flag in [FLAG_HI_SLOT_FIRST, FLAG_RIG_FIRST, FLAG_SUBSYSTEM_FIRST] {
        modules.push(item(db, pick(flag), owner, ship, flag).await);
    }
    let cargo = item(db, pick(FLAG_HI_SLOT_FIRST), owner, ship, FLAG_CARGO).await;
    let fit = Fit::load(db, sde.clone(), owner, ship).await.ok().unwrap();
    let mut hp = [0.0; 3];
    for (index, id) in modules.iter().enumerate() {
        hp[index] = fit.value(Key::Module(*id), ATTR_STRUCTURE_HP);
        assert!(dogma::set_override(db, *id, ATTR_HULL_DAMAGE, hp[index]).await.unwrap());
    }
    assert!(dogma::set_override(db, cargo, ATTR_HULL_DAMAGE, 6.0).await.unwrap());
    // Shield, armor and hull damage with no recharge clock; an unrelated override stays.
    for (attribute, value) in [(ATTR_SHIELD_CHARGE, 10.0), (ATTR_ARMOR_DAMAGE, 8.0), (ATTR_HULL_DAMAGE, 9.0)] {
        assert!(dogma::set_override(db, ship, attribute, value).await.unwrap());
    }
    // The base override is written first; the effective cap is then read back through
    // the fit so passive subsystem/rig modifiers cannot skew the repair oracle.
    let shield_base = fit.value(Key::Ship, ATTR_SHIELD_CAPACITY) * 1.5;
    assert!(dogma::set_override(db, ship, ATTR_SHIELD_CAPACITY, shield_base).await.unwrap());
    assert!(dogma::set_override(db, ship, ATTR_MASS, 99.0).await.unwrap());
    let shield_cap = Fit::load(db, sde, owner, ship).await.ok().unwrap().value(Key::Ship, ATTR_SHIELD_CAPACITY);
    (station, ship, modules.try_into().unwrap(), cargo, hp, shield_cap)
}

/// The SDE slot effect ID that fits a flag family, from dgmEffects' slot flags.
fn slot_effect(dogma: &crate::sde::dogma::Dogma, flag: i32) -> Option<i32> {
    let name = match flag {
        f if f == FLAG_HI_SLOT_FIRST => "hiPower",
        f if f == FLAG_RIG_FIRST => "rigSlot",
        _ => "subSystem",
    };
    dogma.effects_by_name(name).next().map(|effect| effect.effect_id)
}

/// Route `/heal` for `owner` through the real dispatch chain and return the reply.
async fn routed_heal(db: &Db, sde: Arc<Sde>, world: Arc<World>, owner: i64, station: i64, ship: i64, role: i64) -> Result<Value, CallError> {
    let (mut c, _peer) = crate::services::tests::client(db.clone(), 0).await;
    c.sde = sde;
    c.world = world;
    c.login.role = role;
    c.session.apply(vec![("charid", Value::Int(owner)), ("stationid", Value::Int(station)), ("shipid", Value::Int(ship))]);
    crate::services::dispatch(&mut c, &Call { service: "slash", method: "SlashCmd",
        args: &[Value::unicode("/heal")], kwargs: &Value::Dict(vec![]), bound: false, bind_params: None }).await
}

fn attribute_of(info: &Value, attribute: i32) -> Option<f64> {
    let Value::Dict(attrs) = value_field(info, "attributes")? else { return None };
    attrs.iter().find(|(k, _)| k.as_int() == Some(i64::from(attribute))).and_then(|(_, v)| v.as_float())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn station_heal_repairs_fitted_damage_and_preserves_state() {
    init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = fixture::create(&db).await.unwrap();
    let (station, ship, modules, cargo, hp, shield_cap) = docked_ship(&db, sde.clone(), fixture.character).await;
    let balance = wallets::balance(&db, fixture.character).await.ok().unwrap();

    assert!(routed_heal(&db, sde.clone(), World::new(db.clone(), sde.clone(), 1), fixture.character, station, ship, 0).await.is_err());
    db.execute("UPDATE game.characters SET ship_id = NULL WHERE character_id = $1", &[&fixture.character]).await.unwrap();
    assert!(routed_heal(&db, sde.clone(), World::new(db.clone(), sde.clone(), 1), fixture.character, station, ship, GM_ROLES).await.is_err());
    db.execute("UPDATE game.characters SET ship_id = $2 WHERE character_id = $1", &[&fixture.character, &ship]).await.unwrap();

    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let world = World::new(db.clone(), sde.clone(), 1);
    world.register_pilot(fixture.character, sender);
    let (mut c, _peer) = crate::services::tests::client(db.clone(), 0).await;
    c.sde = sde.clone();
    c.world = world;
    c.login.role = GM_ROLES;
    c.session.apply(vec![("charid", Value::Int(fixture.character)), ("stationid", Value::Int(station)), ("shipid", Value::Int(ship))]);
    crate::services::dispatch(&mut c, &Call { service: "slash", method: "SlashCmd",
        args: &[Value::unicode("/heal")], kwargs: &Value::Dict(vec![]), bound: false, bind_params: None }).await.ok().unwrap();

    let rows = dogma::overrides(&db, ship).await.unwrap();
    let shield = rows.iter().find(|r| r.attribute_id == ATTR_SHIELD_CHARGE).unwrap();
    assert_eq!(shield.value, shield_cap);
    assert!(shield_cap > 0.0);
    assert!(shield.updated_at > 0);
    for (attribute, expected) in [(ATTR_ARMOR_DAMAGE, 0.0), (ATTR_HULL_DAMAGE, 0.0), (ATTR_MASS, 99.0)] {
        assert_eq!(rows.iter().find(|r| r.attribute_id == attribute).unwrap().value, expected);
    }
    for (id, expected) in modules.iter().map(|id| (*id, 0.0)).chain([(cargo, 6.0)]) {
        assert_eq!(dogma::overrides(&db, id).await.unwrap().iter().find(|r| r.attribute_id == ATTR_HULL_DAMAGE).unwrap().value, expected);
    }
    for id in &modules {
        assert!(db.query_one("SELECT NOT online AS offline FROM game.items WHERE item_id = $1", &[id]).await.unwrap().get::<_, bool>("offline"));
    }
    assert_eq!(wallets::balance(&db, fixture.character).await.ok().unwrap(), balance);

    let (event, args) = receiver.try_recv().unwrap();
    assert_eq!(event, "OnModuleAttributeChanges");
    let Some(Value::List(changed)) = args.into_iter().next() else { panic!("expected change list") };
    for (item, attribute, old, new) in modules.iter().enumerate().map(|(i, id)| (*id, ATTR_HULL_DAMAGE, hp[i], 0.0))
        .chain([(ship, ATTR_SHIELD_CHARGE, 10.0, shield_cap), (ship, ATTR_ARMOR_DAMAGE, 8.0, 0.0), (ship, ATTR_HULL_DAMAGE, 9.0, 0.0)]) {
        assert!(changed.iter().any(|v| matches!(v, Value::Tuple(f) if f[1].as_int() == Some(fixture.character)
            && f[2].as_int() == Some(item) && f[3].as_int() == Some(i64::from(attribute))
            && f[5].as_float() == Some(new) && f[6].as_float() == Some(old))), "missing change {item}/{attribute}");
    }
    assert!(receiver.try_recv().is_err());

    let reply = crate::services::dispatch(&mut c, &Call { service: "dogmaIM", method: "GetAllInfo",
        args: &[Value::Bool(false), Value::Bool(true), Value::None], kwargs: &Value::Dict(vec![]), bound: false, bind_params: None }).await.ok().unwrap();
    let Some(Value::Dict(ship_info)) = value_field(&reply, "shipInfo") else { panic!("expected shipInfo") };
    let ship_info = ship_info.iter().find(|(k, _)| k.as_int() == Some(ship)).map(|(_, v)| v).unwrap();
    let capacity = attribute_of(ship_info, ATTR_CAPACITOR_CAPACITY).expect("capacitor capacity");
    assert!(capacity > 0.0);
    assert_eq!(attribute_of(ship_info, ATTR_CHARGE), Some(capacity));
    for id in [ATTR_HEAT_LOW, ATTR_HEAT_MED, ATTR_HEAT_HI] { assert_eq!(attribute_of(ship_info, id), Some(0.0)); }
    let Some(Value::Tuple(state)) = value_field(&reply, "shipState") else { panic!("expected shipState") };
    let Value::Dict(instances) = &state[0] else { panic!("expected instance cache") };
    let burnt = instances.iter().find(|(k, _)| k.as_int() == Some(modules[0])).map(|(_, v)| v).unwrap().items().unwrap();
    assert_eq!((burnt[1].as_int(), burnt[2].as_float()), (Some(0), Some(0.0)));
    let Value::Dict(racks) = &state[3] else { panic!("expected heat states") };
    for (_, rack) in racks {
        let row = rack.items().unwrap();
        assert_eq!((row[0].as_float(), row[2].as_float()), (Some(0.0), Some(0.0)));
    }
    fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn station_heal_rolls_back_conditions_modules_and_notifications() {
    init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = fixture::create(&db).await.unwrap();
    let (station, ship, modules, cargo, ..) = docked_ship(&db, sde.clone(), fixture.character).await;
    let balance = wallets::balance(&db, fixture.character).await.ok().unwrap();
    let before = dogma::overrides(&db, ship).await.unwrap();

    let _lock = market::lock().await;
    let mut dispatched = false;
    let result = db.transaction(|tx, outbox| {
        let sde = sde.clone();
        Box::pin(async move {
            heal_docked_in(tx, outbox, Identity { char_id: fixture.character, ship_id: Some(ship), corp_id: 0, station_id: Some(station), system_id: None }, sde).await?;
            Err::<(), _>(refuse())
        })
    }, |(), _| { dispatched = true; async move { Ok(()) } }).await;
    assert!(matches!(result, Err(CallError::User { .. })) && !dispatched);

    assert_eq!(dogma::overrides(&db, ship).await.unwrap().iter().map(|r| (r.attribute_id, r.value, r.updated_at)).collect::<Vec<_>>(),
        before.iter().map(|r| (r.attribute_id, r.value, r.updated_at)).collect::<Vec<_>>());
    for id in modules.iter().chain([&cargo]) {
        assert!(dogma::overrides(&db, *id).await.unwrap().iter().find(|r| r.attribute_id == ATTR_HULL_DAMAGE).unwrap().value > 0.0);
    }
    assert_eq!(wallets::balance(&db, fixture.character).await.ok().unwrap(), balance);
    fixture::cleanup(&db, fixture).await.unwrap();
}

