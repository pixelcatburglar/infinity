//! GM unload regression: the shared final cargo-capacity check must refuse a
//! batch that only overflows once a passive capacity source has been removed.
//! The test owns its rows and runs against a dedicated database with SDE.

use super::*;
use crate::sde::Sde;
use crate::services::inventory::{hold_storage, holds};
use infinity_core::consts::dogma::{ATTR_CAPACITY, ATTR_CPU, ATTR_POWER, EFFECT_ONLINE};
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SUBSYSTEM, FLAG_FITTING_FIRST};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn unload_that_removes_cargo_capacity_rolls_back_everything() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.expect("database pool");
    let fixture = crate::services::tests::fixture::create(&db).await.expect("fixture");
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = std::sync::Arc::new(Sde::load(&test_db).await.expect("loaded SDE"));
        let owner = fixture.character;
        let has_capacity_shipid = |t: &&crate::sde::types::TypeData| t.modifiers(&sde.dogma, ATTR_CAPACITY).any(|(_, m)| m.domain == "shipID");
        let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.fit_attribute(ATTR_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)).expect("ship type with cargo capacity").type_id;
        let probe_ship = crate::dao::items::transfers::insert(&test_db, ship_type, owner, 1, FLAG_HANGAR, 1).await.expect("probe ship");
        test_db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&probe_ship]).await.expect("probe hull");
        let mut booster_type = None;
        let mut ratio = 0.0;
        for module_type in sde.types.iter().filter(|t| {
            t.published == Some(true) && t.category_id.is_some_and(|c| c == CATEGORY_MODULE || c == CATEGORY_SUBSYSTEM)
                && t.volume.is_some_and(|v| v.is_finite() && v > 0.0)
                && !sde.types.effects(t.type_id).iter().any(|e| e.effect_id == EFFECT_ONLINE as i32) && has_capacity_shipid(&t)
        }) {
            let probe = crate::dao::items::transfers::insert(&test_db, module_type.type_id, owner, probe_ship, FLAG_FITTING_FIRST, 1).await.unwrap();
            test_db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&probe]).await.unwrap();
            let before = crate::services::fit::Fit::load_unpiloted(&test_db, sde.clone(), probe_ship).await.unwrap();
            let holds::CapacityLimit::Finite(boosted) = holds::effective_capacity(&before, FLAG_CARGO) else { panic!("finite probe capacity") };
            test_db.execute("DELETE FROM game.items WHERE item_id = $1", &[&probe]).await.unwrap();
            let shrunk = crate::services::fit::Fit::load_unpiloted(&test_db, sde.clone(), probe_ship).await.unwrap();
            let holds::CapacityLimit::Finite(shrunk) = holds::effective_capacity(&shrunk, FLAG_CARGO) else { panic!("finite probe capacity") };
            if shrunk > 0.0 && boosted > shrunk { booster_type = Some(module_type.type_id); ratio = boosted / shrunk; break; }
        }
        test_db.execute("DELETE FROM game.items WHERE item_id = $1", &[&probe_ship]).await.unwrap();
        let booster_type = booster_type.expect("published passive shipID cargo-capacity module");
        let has_effect = |t: &&crate::sde::types::TypeData, effect: i32| sde.types.effects(t.type_id).iter().any(|e| e.effect_id == effect);
        let online_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
            && has_effect(&t, EFFECT_ONLINE as i32) && !has_capacity_shipid(&t)
            && t.volume.is_some_and(|v| v.is_finite() && v > 0.0)).expect("ordinary online module").type_id;
        let charge_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)
            && t.volume.is_some_and(|v| v.is_finite() && v > 0.0) && !has_capacity_shipid(&t)).expect("charge type with volume").type_id;
        let ship = crate::dao::items::transfers::insert(&test_db, ship_type, owner, 1, FLAG_HANGAR, 1).await.expect("ship");
        test_db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&ship]).await.expect("assemble ship");
        test_db.execute("UPDATE game.characters SET ship_id = $2 WHERE character_id = $1", &[&owner, &ship]).await.expect("active ship");
        let online = crate::dao::items::transfers::insert(&test_db, online_type, owner, ship, FLAG_FITTING_FIRST + 1, 1).await.expect("online module");
        test_db.execute("UPDATE game.items SET singleton = true, online = true WHERE item_id = $1", &[&online]).await.expect("online state");
        crate::dao::items::combat::apply_loads(&test_db, ship, &[(online, FLAG_FITTING_FIRST + 1, charge_type, 1, 0)]).await.expect("charge");
        let booster = crate::dao::items::transfers::insert(&test_db, booster_type, owner, ship, FLAG_FITTING_FIRST, 1).await.expect("capacity module");
        test_db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&booster]).await.expect("capacity module singleton");
        let volumes = [
            crate::services::item_volume::item_volume(&sde, online_type, true, 1, None).expect("online module volume"),
            crate::services::item_volume::item_volume(&sde, booster_type, true, 1, None).expect("capacity module volume"),
            crate::services::item_volume::item_volume(&sde, charge_type, false, 1, None).expect("charge volume"),
        ];
        let final_used: f64 = volumes.iter().sum();
        assert!(crate::dao::items::dogma::set_override(&test_db, ship, ATTR_CAPACITY, final_used / ratio.sqrt()).await.expect("capacity override"));
        let base = crate::services::fit::Fit::load_unpiloted(&test_db, sde.clone(), ship).await.expect("base fit");
        test_db.execute("DELETE FROM game.items WHERE item_id = $1", &[&booster]).await.unwrap();
        let shrunk = crate::services::fit::Fit::load_unpiloted(&test_db, sde.clone(), ship).await.unwrap();
        let holds::CapacityLimit::Finite(capacity_after) = holds::effective_capacity(&shrunk, FLAG_CARGO) else { panic!("finite final capacity") };
        let booster = crate::dao::items::transfers::insert(&test_db, booster_type, owner, ship, FLAG_FITTING_FIRST, 1).await.expect("capacity module");
        test_db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&booster]).await.expect("capacity module singleton");
        let holds::CapacityLimit::Finite(capacity_before) = holds::effective_capacity(&base, FLAG_CARGO) else { panic!("finite original capacity") };
        assert!(capacity_after < final_used && final_used <= capacity_before, "per-item admission must pass while the final fit overflows");
        // Resource overrides keep reconcile from offlining the module early.
        for attribute in [ATTR_CPU, ATTR_POWER] {
            assert!(crate::dao::items::dogma::set_override(&test_db, online, attribute, 0.0).await.expect("fitting requirement override"));
        }
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), owner).await;
        client.session.apply(vec![("shipid", Value::Int(ship)), ("stationid", Value::None)]);
        let (notify_tx, mut notices) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = notify_tx;
        let error = gm_unload(&mut client, ship, owner, vec![online, booster], ship, FLAG_CARGO).await.expect_err("overfull cargo must refuse");
        let CallError::User { msg, dict } = error else { panic!("capacity refusal must be a UserError") };
        assert_eq!(msg, infinity_core::messages::MSG_NOT_ENOUGH_CARGO_SPACE);
        assert_eq!(dict, vec![("available".into(), Value::Float(0.0)), ("volume".into(), Value::Float(final_used - capacity_after))]);
        assert!(notices.try_recv().is_err(), "a refused batch sends no notifications");
        for (id, flag) in [(online, FLAG_FITTING_FIRST + 1), (booster, FLAG_FITTING_FIRST)] {
            let row = test_db.query_one("SELECT location_id, flag_id, online FROM game.items WHERE item_id = $1", &[&id]).await.expect("module row");
            assert_eq!((row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"), row.get::<_, bool>("online")), (ship, flag, id == online));
        }
        let magazine = test_db.query_one("SELECT type_id, quantity FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2", &[&ship, &(FLAG_FITTING_FIRST + 1)]).await.expect("magazine restored");
        assert_eq!((magazine.get::<_, i32>("type_id"), magazine.get::<_, i64>("quantity")), (charge_type, 1));
        let fit = crate::services::fit::Fit::load_unpiloted(&test_db, sde.clone(), ship).await.unwrap();
        let snapshot = hold_storage::HoldCapacity::capture(&fit, FLAG_CARGO).unwrap();
        let cargo = hold_storage::room(&test_db, &sde, owner, &snapshot).await.expect("cargo room");
        assert_eq!(cargo.used, 0.0, "no module or charge may reach cargo");
        assert!(crate::dao::items::dogma::set_cargo_unlimited(&test_db, ship, final_used / ratio.sqrt()).await.unwrap());
        assert_eq!(gm_unload(&mut client, ship, owner, vec![online, booster], ship, FLAG_CARGO).await.unwrap(), 2);
        let cargo = crate::dao::items::loading::load_items(&test_db, crate::dao::items::loading::ItemQuery::LocationFlags(ship, &[FLAG_CARGO])).await.unwrap();
        assert!(cargo.iter().any(|row| row.item_id == online) && cargo.iter().any(|row| row.item_id == booster) && cargo.iter().any(|row| row.type_id == charge_type && row.quantity == 1));
    }).await;
    db.execute("DELETE FROM game.module_charges WHERE ship_id IN (SELECT item_id FROM game.items WHERE owner_id = $1)", &[&fixture.character]).await.expect("charge cleanup");
    db.execute("DELETE FROM game.weapon_banks WHERE ship_id IN (SELECT item_id FROM game.items WHERE owner_id = $1)", &[&fixture.character]).await.expect("bank cleanup");
    db.execute("DELETE FROM game.pending_reloads WHERE owner_id = $1", &[&fixture.character]).await.expect("reload cleanup");
    db.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id = $1)", &[&fixture.character]).await.expect("attribute cleanup");
    crate::services::tests::fixture::cleanup(&db, fixture).await.expect("cleanup");
    result.expect("final capacity regression");
}
