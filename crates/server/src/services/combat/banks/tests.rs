use super::*;
use crate::{conn::Conn, handshake::Login};

mod integration;

fn fixture_module(id: i64, flag: i32, kind: i32, group: i64, charge: Option<(i32, i128)>) -> Module {
    Module { item: Item { item_id: id, type_id: kind, owner_id: 1, location_id: 1, flag_id: flag,
        quantity: 1, singleton: true, group_id: group, category_id: i64::from(CATEGORY_MODULE), custom_info: None }, online: true, charge }
}

fn user_key(error: CallError) -> String {
    match error { CallError::User { msg, .. } => msg, _ => panic!("expected user refusal") }
}

#[test]
fn pair_rules_and_effective_reload() {
    let group = GROUPABLE_GROUP_IDS[0];
    let a = fixture_module(1, 27, 1, group, None);
    let mut b = fixture_module(2, 28, 1, group, None);
    assert!(validate_pair(&a, &b).is_ok());
    b.online = false;
    assert_eq!(user_key(validate_pair(&a, &b).err().unwrap()), messages::MSG_CANT_LINK_MODULE_NOT_ONLINE);
    b.online = true;
    b.item.type_id = 2;
    assert!(validate_pair(&a, &b).is_err());
    b.item.type_id = 1;
    b.item.group_id = -1;
    assert!(validate_pair(&a, &b).is_err());
    b.item.group_id = group;
    b.charge = Some((3, 1));
    assert!(validate_pair(&a, &b).is_err());
    assert_eq!(effective_charge(None, None), None);
    assert_eq!(effective_charge(Some((3, 0)), None), None);
    assert_eq!(effective_charge(Some((3, 5)), Some((3, 5))), Some((3, 10)));
    assert_eq!(effective_charge(None, Some((4, 2))), Some((4, 2)));
    assert_eq!(effective_charge(Some((3, i64::MAX)), Some((3, i64::MAX))), Some((3, i128::from(i64::MAX) * 2)));
}

#[test]
fn link_all_partitions_and_retains_master() {
    let group = GROUPABLE_GROUP_IDS[0];
    let mut items = vec![
        fixture_module(9, 27, 1, group, None), fixture_module(8, 28, 1, group, None),
        fixture_module(7, 29, 1, group, Some((10, 2))), fixture_module(6, 30, 1, group, Some((10, 2))),
        fixture_module(5, 31, 1, group, Some((10, 3))), fixture_module(4, 32, 2, group, None),
        fixture_module(3, 33, 2, group, None), fixture_module(2, 34, 1, -1, None),
    ];
    items[7].online = false;
    let rows = [charges::Bank { master_id: 8, slave_id: 9 }];
    let groups = grouped(&items, &rows).ok().unwrap();
    assert_eq!(groups, vec![(8, vec![9]), (7, vec![6]), (4, vec![3])]);
    items[0].online = false;
    assert_eq!(user_key(grouped(&items, &rows).err().unwrap()), messages::MSG_CANT_LINK_MODULE_NOT_ONLINE);
}

#[test]
fn ship_authorization_requires_active_ship() {
    assert_eq!(authorize_ship(Some(7), Some(7)).ok(), Some(7));
    for (requested, active) in [(None, Some(7)), (Some(8), Some(7)), (Some(7), None)] {
        assert!(authorize_ship(requested, active).is_err());
    }
}

async fn rpc(client: &mut Client, method: &str, ship: i64, first: i64, second: i64) -> CallResult {
    let args = [Value::Int(ship), Value::Int(first), Value::Int(second)];
    let kwargs = Value::Dict(vec![]);
    crate::services::dogma::dispatch(client, &Call { service: "dogmaIM", method, args: &args, kwargs: &kwargs, bound: true, bind_params: None }).await
}

async fn rows(db: &Db, ship: i64) -> Vec<(i64, i64)> {
    charges::banks(db, ship).await.unwrap().into_iter().map(|r| (r.master_id, r.slave_id)).collect()
}

async fn set_charge(db: &Db, ship: i64, flag: i32, kind: i32, quantity: i64) {
    db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,$3,$4) ON CONFLICT (ship_id,flag_id) DO UPDATE SET type_id=$3,quantity=$4", &[&ship, &flag, &kind, &quantity]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE"]
async fn real_bank_rpcs_and_transactional_rules() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().expect("station in SDE").station_id;
    use infinity_core::consts::dogma::{ATTR_CPU, ATTR_CPU_OUTPUT, ATTR_POWER, ATTR_POWER_OUTPUT, ATTR_HI_SLOTS, ATTR_TURRET_SLOTS_LEFT, ATTR_LAUNCHER_SLOTS_LEFT, EFFECT_TURRET_FITTED, EFFECT_LAUNCHER_FITTED, ATTR_REQUIRED_SKILLS, ATTR_REQUIRED_SKILL_LEVELS, ATTR_SKILL_TIME_CONSTANT};
    /// SDE dogma effect hiPower, the client high-slot rack effect.
    const EFFECT_HI_POWER: i32 = 12;
    /// Client dogma.const.attributeMaxGroupFitted.
    const MAX_GROUP_FITTED: i32 = 1544;
    /// Client dogma.const.attributeMaxTypeFitted.
    const MAX_TYPE_FITTED: i32 = 2431;
    /// Client dogma.const.attributeMaxGroupOnline.
    const MAX_GROUP_ONLINE: i32 = 978;
    // These restrictions and limits mirror private constants in effects/fitting.rs:
    // CAN_FIT_GROUPS, CAN_FIT_TYPES, MAX_GROUP_FITTED, MAX_TYPE_FITTED, and MAX_GROUP_ONLINE.
    const HULL_RESTRICTIONS: [i32; 32] = [1298,1299,1300,1301,1872,1879,1880,1881,2065,2396,2476,2477,2478,2479,2480,2481,2482,2483,2484,2485,1302,1303,1304,1305,1944,2103,2463,2486,2487,2488,2758,5948];
    let base = |ty: i32, attr: i32| crate::services::fit::base_attribute(sde.types.get(ty), sde.dogma.attribute(attr), attr);
    let (gun, hull) = sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.group_id.is_some_and(|g| GROUPABLE_GROUP_IDS.contains(&i64::from(g)))
        && [infinity_core::consts::dogma::EFFECT_ONLINE as i32, EFFECT_HI_POWER].iter().all(|id| t.effects().iter().any(|e| e.effect_id == *id))
        && HULL_RESTRICTIONS.iter().all(|&a| base(t.type_id, a) <= 0.0)
        && [MAX_GROUP_FITTED, MAX_TYPE_FITTED].iter().all(|&a| { let v = base(t.type_id, a); v <= 0.0 || v >= 8.0 })
        && t.attribute(MAX_GROUP_ONLINE).is_none_or(|v| v >= 8.0))
        .find_map(|gun| {
            let turret = gun.effects().iter().any(|e| e.effect_id == EFFECT_TURRET_FITTED);
            let launcher = gun.effects().iter().any(|e| e.effect_id == EFFECT_LAUNCHER_FITTED);
            sde.types.iter().find(|h| h.published == Some(true) && h.category_id == Some(CATEGORY_SHIP)
                && base(h.type_id, ATTR_HI_SLOTS) >= 8.0
                && (!turret || base(h.type_id, ATTR_TURRET_SLOTS_LEFT) >= 8.0)
                && (!launcher || base(h.type_id, ATTR_LAUNCHER_SLOTS_LEFT) >= 8.0)
                && base(h.type_id, ATTR_CPU_OUTPUT) >= 8.0 * base(gun.type_id, ATTR_CPU)
                && base(h.type_id, ATTR_POWER_OUTPUT) >= 8.0 * base(gun.type_id, ATTR_POWER))
                .map(|h| (gun.type_id, h.type_id))
        }).expect("unrestricted groupable gun and viable eight-slot hull in SDE");
    let required: Vec<_> = ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS).filter_map(|(&skill, level)| {
        let id = base(gun, skill) as i32;
        let level = base(gun, level) as i32;
        (id > 0 && level > 0).then_some((id, level))
    }).collect();
    let ammo: Vec<i32> = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_CHARGE)).take(2).map(|t| t.type_id).collect();
    assert_eq!(ammo.len(), 2);
    let now = filetime_now();
    let name = format!("weapon-banks-{now}");
    let password: Vec<u8> = Vec::new();
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ($1,$2,0) RETURNING user_id", &[&name, &password]).await.unwrap().get("user_id");
    let owner: i64 = db.query_one("SELECT nextval('game.characters_character_id_seq') AS character_id", &[]).await.unwrap().get("character_id");
    let corporation: i64 = db.query_one("WITH id AS (SELECT nextval('game.corporations_corporation_id_seq') AS n) INSERT INTO game.corporations (corporation_id,name,ticker,ceo_id,creator_id,station_id,created) SELECT n,'weapon-banks-' || n::text,'WB' || n::text,$1,$1,$2,$3 FROM id RETURNING corporation_id", &[&owner, &station, &now]).await.unwrap().get("corporation_id");
    db.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id,balance) VALUES ($1,$2,$3,1,1,1,$4,0)", &[&owner, &account, &name, &corporation]).await.unwrap();
    let insert = "INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,$5) RETURNING item_id";
    let ship: i64 = db.query_one(insert, &[&hull, &owner, &station, &FLAG_HANGAR, &false]).await.unwrap().get("item_id");
    db.execute("UPDATE game.characters SET ship_id=$2,station_id=$3 WHERE character_id=$1", &[&owner, &ship, &station]).await.unwrap();
    // Insert in reverse slot order so item-ID order cannot accidentally choose the first slave.
    let mut guns = Vec::new();
    for flag in (27_i32..=34).rev() {
        let id: i64 = db.query_one(insert, &[&gun, &owner, &ship, &flag, &true]).await.unwrap().get("item_id");
        guns.push(id);
    }
    guns.reverse();
    let scenario_db = db.clone();
    let scenario_guns = guns.clone();
    let scenario = tokio::spawn(async move {
    let db = scenario_db;
    let guns = scenario_guns;
    for (skill, level) in required {
        let rank = sde.types.get(skill).and_then(|t| t.fit_attribute(ATTR_SKILL_TIME_CONSTANT)).expect("required skill rank");
        let mut points = 1_i64;
        while crate::services::skills::formulas::level_from_points(points, rank) < level {
            points = points.checked_mul(2).expect("required skill level must be attainable");
        }
        db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,$4) ON CONFLICT (character_id,type_id) DO UPDATE SET level=GREATEST(game.skills.level,EXCLUDED.level),skill_points=GREATEST(game.skills.skill_points,EXCLUDED.skill_points)", &[&owner, &skill, &level, &points]).await.unwrap();
    }
    let world = World::new(db.clone(), sde.clone(), 0);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let mut client = Client::new(Conn::new(stream, address), Arc::new(crate::config::Config::default()), db.clone(), sde,
        crate::chat::Hub::new("localhost", db.clone(), world.clone()), world.clone(), Login { user_id: account, user_name: name, role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
    client.session.apply(vec![("charid", Value::Int(owner)), ("shipid", Value::Int(ship)), ("stationid", Value::Int(station))]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(owner, tx.clone());
    client.notify_tx = tx;
    let methods = ["LinkWeapons", "MergeModuleGroups", "PeelAndLink", "UnlinkModule", "UnlinkAllModules", "DestroyWeaponBank", "LinkAllWeapons", "GetWeaponBankInfoForShip"];
    for method in methods { assert!(rpc(&mut client, method, ship + 1, guns[0], guns[1]).await.is_err()); }
    assert!(rpc(&mut client, "GetWeaponBankInfoForShip", ship, 0, 0).await.is_ok());
    assert!(rpc(&mut client, "LinkWeapons", ship, guns[0], ship).await.is_err());
    assert!(rows(&db, ship).await.is_empty());
    assert!(matches!(rpc(&mut client, "UnlinkModule", ship, guns[0], 0).await, Err(CallError::User { .. })));
    assert!(rpc(&mut client, "DestroyWeaponBank", ship, guns[0], 0).await.is_ok());
    assert!(rpc(&mut client, "PeelAndLink", ship, guns[0], guns[1]).await.is_ok());
    assert!(rows(&db, ship).await.is_empty());
    for (master, slave) in [(guns[0], guns[1]), (guns[3], guns[4]), (guns[3], guns[5])] {
        assert!(rpc(&mut client, "LinkWeapons", ship, master, slave).await.is_ok());
    }
    let before = rows(&db, ship).await;
    for method in ["LinkWeapons", "MergeModuleGroups", "PeelAndLink"] {
        assert!(rpc(&mut client, method, ship, guns[0], guns[1]).await.is_ok());
        assert_eq!(rows(&db, ship).await, before);
    }
    set_charge(&db, ship, 32, ammo[0], 1).await;
    let before_mismatch = rows(&db, ship).await;
    assert!(rpc(&mut client, "MergeModuleGroups", ship, guns[0], guns[3]).await.is_err());
    assert_eq!(rows(&db, ship).await, before_mismatch);
    db.execute("DELETE FROM game.module_charges WHERE ship_id=$1 AND flag_id=$2", &[&ship, &32_i32]).await.unwrap();
    assert!(rpc(&mut client, "PeelAndLink", ship, guns[1], guns[5]).await.is_ok());
    assert_eq!(rows(&db, ship).await, vec![(guns[0], guns[1]), (guns[0], guns[4]), (guns[3], guns[5])]);
    let unlinked = rpc(&mut client, "UnlinkModule", ship, guns[4], 0).await.ok().unwrap();
    assert_eq!(unlinked, Value::Int(guns[1]));
    assert!(rpc(&mut client, "MergeModuleGroups", ship, guns[0], guns[5]).await.is_ok());
    assert_eq!(rows(&db, ship).await, vec![(guns[0], guns[3]), (guns[0], guns[4]), (guns[0], guns[5])]);
    assert!(rpc(&mut client, "DestroyWeaponBank", ship, guns[4], 0).await.is_ok());
    assert!(rows(&db, ship).await.is_empty());
    set_charge(&db, ship, 27, ammo[0], 1).await;
    set_charge(&db, ship, 28, ammo[1], 1).await;
    assert!(rpc(&mut client, "LinkWeapons", ship, guns[0], guns[1]).await.is_err());
    assert!(rows(&db, ship).await.is_empty());
    db.execute("DELETE FROM game.module_charges WHERE ship_id=$1 AND flag_id=$2", &[&ship, &28_i32]).await.unwrap();
    pending_reloads::insert(&db, owner, ship, guns[1], 28, ammo[0], 1, infinity_core::consts::FLAG_CARGO, ship).await.unwrap();
    assert!(rpc(&mut client, "LinkWeapons", ship, guns[0], guns[1]).await.is_ok());
    db.execute("DELETE FROM game.pending_reloads WHERE ship_id=$1", &[&ship]).await.unwrap();
    assert!(rpc(&mut client, "UnlinkAllModules", ship, 0, 0).await.is_ok());
    set_charge(&db, ship, 27, ammo[0], 10).await;
    set_charge(&db, ship, 28, ammo[0], 5).await;
    set_charge(&db, ship, 29, ammo[0], 5).await;
    pending_reloads::insert(&db, owner, ship, guns[1], 28, ammo[0], 5, infinity_core::consts::FLAG_CARGO, ship).await.unwrap();
    assert!(rpc(&mut client, "LinkWeapons", ship, guns[0], guns[1]).await.is_ok());
    let before_topup_refusal = rows(&db, ship).await;
    assert!(rpc(&mut client, "LinkWeapons", ship, guns[0], guns[2]).await.is_err());
    assert_eq!(rows(&db, ship).await, before_topup_refusal);
    db.execute("DELETE FROM game.pending_reloads WHERE ship_id=$1", &[&ship]).await.unwrap();
    assert!(rpc(&mut client, "UnlinkAllModules", ship, 0, 0).await.is_ok());
    db.execute("DELETE FROM game.module_charges WHERE ship_id=$1 AND flag_id=$2", &[&ship, &29_i32]).await.unwrap();
    set_charge(&db, ship, 27, ammo[0], 0).await;
    set_charge(&db, ship, 28, ammo[1], 0).await;
    assert!(rpc(&mut client, "LinkWeapons", ship, guns[0], guns[1]).await.is_ok());
    assert!(rpc(&mut client, "UnlinkAllModules", ship, 0, 0).await.is_ok());
    for (index, kind, quantity) in [(0, ammo[0], 1), (1, ammo[0], 1), (2, ammo[1], 1), (3, ammo[1], 1), (4, ammo[1], 2)] {
        set_charge(&db, ship, 27 + index, kind, quantity).await;
    }
    assert!(rpc(&mut client, "LinkWeapons", ship, guns[1], guns[0]).await.is_ok());
    assert!(rpc(&mut client, "LinkAllWeapons", ship, 0, 0).await.is_ok());
    assert_eq!(rows(&db, ship).await, vec![(guns[1], guns[0]), (guns[2], guns[3]), (guns[5], guns[6]), (guns[5], guns[7])]);
    let before = rows(&db, ship).await;
    db.execute("UPDATE game.items SET online=false WHERE item_id=$1", &[&guns[4]]).await.unwrap();
    assert!(rpc(&mut client, "LinkAllWeapons", ship, 0, 0).await.is_err());
    assert_eq!(rows(&db, ship).await, before);
    while let Ok((name, _)) = rx.try_recv() { assert_ne!(name, "OnWeaponBanksChanged"); }
    integration::exercise(&db, &mut client, ship, owner, &guns, &mut rx).await;
    }).await;
    for id in &guns { state().active.remove(id); }
    db.execute("DELETE FROM game.weapon_banks WHERE ship_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.module_charges WHERE ship_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.pending_reloads WHERE ship_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id=$1 OR item_id=ANY($2)", &[&ship, &&guns[..]]).await.unwrap();
    db.execute("DELETE FROM game.skills WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE location_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corporation]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    scenario.unwrap();
}
