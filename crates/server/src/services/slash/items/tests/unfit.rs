//! `/unfit` regressions through the real `slash.SlashCmd` dispatch route.
//! These need a dedicated database with real SDE; the fixture owns its rows.

use super::*;
use infinity_core::consts::dogma::{ATTR_CAPACITY, ATTR_CPU, ATTR_POWER, EFFECT_ONLINE, EFFECT_RIG_SLOT, EFFECT_LO_POWER, EFFECT_HI_POWER, EFFECT_MED_POWER};
use infinity_core::consts::{CATEGORY_CHARGE, CATEGORY_MODULE, CATEGORY_SHIP, CATEGORY_SUBSYSTEM, FLAG_HI_SLOT_FIRST, FLAG_MED_SLOT_FIRST, FLAG_RIG_FIRST, FLAG_SUBSYSTEM_FIRST};
use marshal::Value;

/// A published module type with volume whose effects include `effect`.
fn module_with(sde: &crate::sde::Sde, effect: i32) -> i32 {
    sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.volume.is_some_and(|v| v.is_finite() && v > 0.0)
        && sde.types.effects(t.type_id).iter().any(|e| e.effect_id == effect))
        .expect("published module with the rack effect").type_id
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn unfit_all_unloads_ordinary_modules_and_returns_charges() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.expect("database pool");
    let sde = std::sync::Arc::new(crate::sde::Sde::load(&db).await.expect("loaded SDE"));
    let low_type = module_with(&sde, EFFECT_LO_POWER);
    let mid_type = module_with(&sde, EFFECT_MED_POWER);
    let high_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.volume.is_some_and(|v| v.is_finite() && v > 0.0)
        && sde.types.effects(t.type_id).iter().any(|e| e.effect_id == EFFECT_HI_POWER)
        && sde.types.effects(t.type_id).iter().any(|e| e.effect_id == EFFECT_ONLINE as i32)).expect("published online high module").type_id;
    // A retained cargo-capacity modifier (for example a passive penalty rig or
    // subsystem) would shrink the overridden base capacity and make success
    // depend on the dataset; pick fixtures without shipID capacity modifiers.
    let without_ship_capacity = |t: &&crate::sde::types::TypeData| !t.modifiers(&sde.dogma, ATTR_CAPACITY).any(|(_, m)| m.domain == "shipID");
    let rig_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.volume.is_some_and(|v| v.is_finite() && v > 0.0)
        && sde.types.effects(t.type_id).iter().any(|e| e.effect_id == EFFECT_RIG_SLOT)
        && without_ship_capacity(&t)).expect("published rig without capacity modifier").type_id;
    let subsystem_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SUBSYSTEM)
        && t.volume.is_some_and(|v| v.is_finite() && v > 0.0) && without_ship_capacity(&t)).expect("published subsystem without capacity modifier").type_id;
    let charge_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_CHARGE)
        && t.volume.is_some_and(|v| v.is_finite() && v > 0.0)).expect("published charge").type_id;
    let cargo_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_CHARGE)
        && t.volume.is_some_and(|v| v.is_finite() && v > 0.0) && t.type_id != charge_type).expect("distinct published cargo type").type_id;
    let (ship_type, system, constellation, region) = {
        let s = sde.map.systems().find(|s| s.constellation_id.is_some() && s.region_id.is_some()).expect("solar system");
        let hull = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)).expect("published ship").type_id;
        (hull, s.solar_system_id, s.constellation_id.unwrap(), s.region_id.unwrap())
    };
    let fixture = crate::services::tests::fixture::create(&db).await.expect("fixture");
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let owner = fixture.character;
        let ship = crate::dao::items::transfers::insert(&test_db, ship_type, owner, system, 0, 1).await.expect("ship");
        test_db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&ship]).await.expect("assemble ship");
        test_db.execute("UPDATE game.characters SET ship_id = $2, solar_system_id = $3, constellation_id = $4, region_id = $5 WHERE character_id = $1",
            &[&owner, &ship, &system, &constellation, &region]).await.expect("board in space");
        let low = crate::dao::items::transfers::insert(&test_db, low_type, owner, ship, FLAG_FITTING_FIRST, 1).await.expect("low module");
        let mid = crate::dao::items::transfers::insert(&test_db, mid_type, owner, ship, FLAG_MED_SLOT_FIRST, 1).await.expect("mid module");
        let high = crate::dao::items::transfers::insert(&test_db, high_type, owner, ship, FLAG_HI_SLOT_FIRST, 1).await.expect("high module");
        let rig_id = crate::dao::items::transfers::insert(&test_db, rig_type, owner, ship, FLAG_RIG_FIRST, 1).await.expect("rig");
        let subsystem_id = crate::dao::items::transfers::insert(&test_db, subsystem_type, owner, ship, FLAG_SUBSYSTEM_FIRST, 1).await.expect("subsystem");
        let cargo = crate::dao::items::transfers::insert(&test_db, cargo_type, owner, ship, FLAG_CARGO, 7).await.expect("original cargo");
        for (module, online) in [(low, false), (mid, false), (high, true), (rig_id, true), (subsystem_id, true)] {
            test_db.execute("UPDATE game.items SET singleton = true, online = $2 WHERE item_id = $1", &[&module, &online]).await.expect("assemble module");
        }
        // An online module's CPU/power draw is validated by the reconcile;
        // zeroing it keeps the rest of the fit unchanged.
        for attribute in [ATTR_CPU, ATTR_POWER] {
            assert!(crate::dao::items::dogma::set_override(&test_db, high, attribute, 0.0).await.expect("zero module load"));
        }
        crate::dao::items::combat::apply_loads(&test_db, ship, &[(high, FLAG_HI_SLOT_FIRST, charge_type, 3, 0)]).await.expect("load charges");
        let volume = [low_type, mid_type, high_type].iter()
            .map(|t| crate::services::item_volume::item_volume(&sde, *t, true, 1, None).expect("module volume")).sum::<f64>()
            + crate::services::item_volume::item_volume(&sde, charge_type, false, 3, None).expect("charge volume")
            + crate::services::item_volume::item_volume(&sde, cargo_type, false, 7, None).expect("cargo volume");
        assert!(crate::dao::items::dogma::set_override(&test_db, ship, ATTR_CAPACITY, volume + 1.0).await.expect("cargo capacity override"));
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), owner).await;
        client.login.role = infinity_core::consts::ROLE_GML;
        client.session.apply(vec![("shipid", Value::Int(ship)), ("stationid", Value::None), ("solarsystemid", Value::Int(system))]);
        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = sender;
        assert_eq!(crate::services::slash::tests::support::rpc(&mut client, "/unfit me all").await.expect("unfit reply"), Value::unicode(messages::gm::gm_item_unloaded(3)));
        let rows = test_db.query("SELECT item_id, location_id, flag_id, quantity, singleton FROM game.items WHERE owner_id = $1 ORDER BY item_id", &[&owner]).await.expect("items after unfit");
        let state = |id: i64| rows.iter().find(|r| r.get::<_, i64>("item_id") == id).map(|r| (r.get::<_, i64>("location_id"), r.get::<_, i32>("flag_id"), r.get::<_, i64>("quantity"), r.get::<_, bool>("singleton"))).expect("item row");
        for module in [low, mid, high] { assert_eq!(state(module), (ship, FLAG_CARGO, 1, true)); }
        assert_eq!(state(rig_id), (ship, FLAG_RIG_FIRST, 1, true));
        assert_eq!(state(subsystem_id), (ship, FLAG_SUBSYSTEM_FIRST, 1, true));
        assert_eq!(state(cargo), (ship, FLAG_CARGO, 7, false));
        let charge_rows = test_db.query("SELECT quantity FROM game.items WHERE owner_id = $1 AND type_id = $2 AND location_id = $3 AND flag_id = $4", &[&owner, &charge_type, &ship, &FLAG_CARGO]).await.expect("returned charges");
        assert_eq!(charge_rows.iter().map(|r| r.get::<_, i64>("quantity")).sum::<i64>(), 3);
        assert!(test_db.query_opt("SELECT 1 FROM game.module_charges WHERE ship_id = $1", &[&ship]).await.expect("magazines").is_none());
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|(name, _)| name == "OnItemChange"), "inventory notification expected");
        let snapshot = crate::services::fit::Fit::load(&test_db, sde.clone(), owner, ship).await.map(|fit| crate::services::inventory::hold_storage::HoldCapacity::capture(&fit, FLAG_CARGO)).expect("final fit").expect("cargo snapshot");
        let room = crate::services::inventory::hold_storage::room(&test_db, &sde, owner, &snapshot).await.expect("cargo room");
        let crate::services::inventory::holds::CapacityLimit::Finite(capacity) = room.capacity else { panic!("finite cargo capacity") };
        assert!(room.used <= capacity);
    }).await;
    db.execute("DELETE FROM game.module_charges WHERE ship_id IN (SELECT item_id FROM game.items WHERE owner_id = $1)", &[&fixture.character]).await.expect("charge cleanup");
    crate::services::tests::fixture::cleanup(&db, fixture).await.expect("cleanup");
    result.expect("unfit regression");
}
