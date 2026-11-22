//! Real-SDE coverage of hidden stance modes in `Fit::load`: the exact hull's recognized mode is an offline `Key::Module` source for Ship, Module and Charge receivers; other hidden rows stay inert and duplicates are refused.
use super::super::super::{Fit, Key};
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation};
use infinity_core::consts::{FLAG_HIDDEN_MODIFIERS, FLAG_HI_SLOT_FIRST, FLAG_MED_SLOT_FIRST};
use infinity_core::consts::dogma::{ATTR_ARMOR_DAMAGE_AMOUNT, ATTR_ARMOR_EM_DAMAGE_RESONANCE,
    ATTR_CAPACITOR_NEED, ATTR_EXPLOSION_DELAY, ATTR_MAX_VELOCITY, ATTR_RATE_OF_FIRE,
    ATTR_REACTIVATION_DELAY};
use std::sync::Arc;

fn item(type_id: i32, flag_id: i32) -> ShipItemSpec<'static> {
    ShipItemSpec { type_id, flag_id, quantity: 1, singleton: true, online: flag_id != FLAG_HIDDEN_MODIFIERS, attributes: &[] }
}

async fn load(ship_type: i32, items: &[ShipItemSpec<'static>]) -> (crate::db::Db, Arc<crate::sde::Sde>, crate::services::tests::fixture::ShipFixture, Fit) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let f = create_ship(&db, ship_type, ShipLocation::Docked { station: 1 }, items).await.unwrap();
    let fit = Fit::load(&db, sde.clone(), f.character, f.ship).await.unwrap();
    (db, sde, f, fit)
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn recognized_mode_is_an_offline_source_for_every_receiver() {
    // Confessor defense: hull, module, stacked receivers.
    let (db, _, f, fit) = load(34317, &[
        item(34319, FLAG_HIDDEN_MODIFIERS), item(11355, FLAG_HI_SLOT_FIRST),
        item(11269, FLAG_MED_SLOT_FIRST), item(11269, FLAG_MED_SLOT_FIRST + 1)]).await;
    assert_eq!(fit.source_type(Key::Module(f.items[0])), Some(34319));
    let dampened = 1.0 + (0.8 - 1.0) * (-(1.0_f64 / 2.67).powi(2)).exp();
    assert!((fit.value(Key::Ship, ATTR_ARMOR_EM_DAMAGE_RESONANCE) - 0.5 / 1.5 * 0.8 * dampened).abs() < 1e-9);
    assert!((fit.value(Key::Module(f.items[1]), ATTR_CAPACITOR_NEED) - 38.0 / 1.5).abs() < 1e-9);
    assert!((fit.value(Key::Module(f.items[1]), ATTR_ARMOR_DAMAGE_AMOUNT) - 49.0 / 0.75).abs() < 1e-9);
    cleanup_ship(&db, None, f).await.unwrap();
    // Skua sharpshooter: rocket charge velocity via the charID modifier.
    let (db, sde, f, _) = load(89808, &[item(90064, FLAG_HIDDEN_MODIFIERS), item(10629, FLAG_HI_SLOT_FIRST)]).await;
    db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,266,1)", &[&f.ship, &FLAG_HI_SLOT_FIRST]).await.unwrap();
    let fit = Fit::load(&db, sde, f.character, f.ship).await.unwrap();
    assert!((fit.value(Key::Charge(f.items[1]), ATTR_MAX_VELOCITY) - 2250.0 / 0.2857).abs() < 1.0);
    cleanup_ship(&db, None, f).await.unwrap();
    // Anhinga primary: charge velocity, flight time, launcher group ROF.
    let (db, sde, f, _) = load(89807, &[item(90061, FLAG_HIDDEN_MODIFIERS), item(13320, FLAG_HI_SLOT_FIRST)]).await;
    db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,203,1)", &[&f.ship, &FLAG_HI_SLOT_FIRST]).await.unwrap();
    let fit = Fit::load(&db, sde, f.character, f.ship).await.unwrap();
    let charge = Key::Charge(f.items[1]);
    assert!((fit.value(charge, ATTR_MAX_VELOCITY) - 4700.0 / 1.5).abs() < 1e-6);
    assert!((fit.value(charge, ATTR_EXPLOSION_DELAY) - 14000.0 / 0.0909).abs() < 1.0);
    assert!((fit.value(Key::Module(f.items[1]), ATTR_RATE_OF_FIRE) - 20680.0 / 1.333).abs() < 1e-3);
    cleanup_ship(&db, None, f).await.unwrap();
    // Anhinga secondary: group 1189 reactivation delay is effective 669.
    let (db, _, f, fit) = load(89807, &[item(90065, FLAG_HIDDEN_MODIFIERS), item(33915, FLAG_MED_SLOT_FIRST)]).await;
    assert!((fit.value(Key::Module(f.items[1]), ATTR_REACTIVATION_DELAY) - 180000.0 * 0.1).abs() < 1e-6);
    cleanup_ship(&db, None, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn hidden_rows_stay_inert_and_duplicate_modes_are_refused() {
    // Foreign mode and non-mode hidden row stay inert.
    let (db, sde, f, fit) = load(34317, &[item(90060, FLAG_HIDDEN_MODIFIERS), item(11355, FLAG_HIDDEN_MODIFIERS)]).await;
    assert_eq!(fit.source_type(Key::Module(f.items[0])), None);
    assert_eq!(fit.source_type(Key::Module(f.items[1])), None);
    assert_eq!(fit.value(Key::Ship, ATTR_ARMOR_EM_DAMAGE_RESONANCE), 0.5);
    cleanup_ship(&db, None, f).await.unwrap();
    // Two recognized modes of one hull are an integrity error.
    let f = create_ship(&db, 34317, ShipLocation::Docked { station: 1 }, &[
        item(34319, FLAG_HIDDEN_MODIFIERS), item(34323, FLAG_HIDDEN_MODIFIERS)]).await.unwrap();
    assert!(Fit::load(&db, sde, f.character, f.ship).await.is_err());
    cleanup_ship(&db, None, f).await.unwrap();
}
