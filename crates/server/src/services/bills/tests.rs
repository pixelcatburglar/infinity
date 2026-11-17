//! Bill settlement regressions: the one exact payer path for manual corporate,
//! character, alliance-executor and automatic payments, plus the live-term
//! renewal rule and the settings write boundary.

#[cfg(feature = "tests-economy")]
use super::*;
#[cfg(feature = "tests-economy")]
use crate::dao::{bills as bills_dao, offices as offices_dao};
#[cfg(feature = "tests-economy")]
use crate::services::tests::fixture;
#[cfg(feature = "tests-economy")]
use infinity_core::consts::{ACCOUNT_CASH, FILETIME_DAY, ROLE_ACCOUNTANT, ROLE_DIRECTOR, TYPE_OFFICE};
#[cfg(feature = "tests-economy")]
use crate::isk::IskAmount;
#[cfg(feature = "tests-economy")]
use crate::space::World;
#[cfg(feature = "tests-economy")]
use std::sync::Arc;
#[cfg(feature = "tests-social")]
pub(in crate::services) fn bill_row_for_capture(row: &crate::dao::bills::Bill) -> marshal::Value { super::bill_row(row) }
#[cfg(feature = "tests-economy")]
async fn world_of(db: &Db) -> (Arc<crate::sde::Sde>, Arc<World>) {
    let sde = Arc::new(crate::sde::Sde::load(db).await.unwrap());
    (sde.clone(), crate::space::World::new(db.clone(), sde, 1))
}

#[cfg(feature = "tests-economy")]
/// A corporation with an accountant/director, wallet headroom and an active
/// lease whose renewal bill is issued for boundary `until`.
async fn rented_fixture(db: &Db, sde: &Arc<crate::sde::Sde>, _world: &World) -> (fixture::Fixture, i64, i64, i64, i64) {
    let actor = fixture::create(db).await.unwrap();
    let station = sde.map.stations().find(|s| s.solar_system_id.is_some() && s.owner_id.is_some()).unwrap().station_id;
    let corp = fixture::create_corporation(db, actor.character, station).await.unwrap();
    fixture::assign_corporation(db, actor.character, corp, ROLE_DIRECTOR | ROLE_ACCOUNTANT).await.unwrap();
    db.execute("INSERT INTO game.corp_wallets (corporation_id, account_key, balance) VALUES ($1, $2, 1000) ON CONFLICT (corporation_id, account_key) DO UPDATE SET balance = EXCLUDED.balance", &[&corp, &ACCOUNT_CASH]).await.unwrap();
    let office = crate::dao::items::transfers::insert(db, TYPE_OFFICE, corp, station, 0, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&office]).await.unwrap();
    let until = crate::client::filetime_now() + FILETIME_DAY;
    offices_dao::insert(db, office, corp, station, until, crate::client::filetime_now()).await.unwrap();
    (actor, corp, station, office, until)
}

#[cfg(feature = "tests-economy")]
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn manual_and_automatic_payment_extend_one_term_once() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (sde, world) = world_of(&db).await;
    let (actor, corp, station, office, until) = rented_fixture(&db, &sde, &world).await;
    let amount = IskAmount::from_isk(100.0).unwrap();
    let creditor = sde.map.station(station).and_then(|station| station.owner_id).expect("station landlord");
    let bill: i64 = db.query_one("INSERT INTO game.bills (bill_type, debtor_id, creditor_id, amount, due, created, external_id, external_id2, period_start, period_end) VALUES ($1, $2, $8, $3::text::numeric, $4, 0, $5, $6, $4, $7) RETURNING bill_id",
        &[&BILL_RENTAL, &corp, &amount.decimal(), &until, &office, &station, &(until + 30 * FILETIME_DAY), &creditor]).await.unwrap().get("bill_id");
    bills_dao::set_setting(&db, corp, BILL_RENTAL, true).await.unwrap();
    bills_dao::set_division(&db, bills_dao::BillOwner::Corporation(corp), ACCOUNT_CASH).await.unwrap();
    let (manual, automatic) = tokio::join!(
        settlement::pay(&db, &world, bill, settlement::Payer::Corporation { actor: actor.character, corporation: corp, key: ACCOUNT_CASH }),
        settlement::pay(&db, &world, bill, settlement::Payer::Automatic { owner: bills_dao::BillOwner::Corporation(corp) })
    );
    assert_ne!(manual.is_ok(), automatic.is_ok());
    let lease = offices_dao::get(&db, office).await.unwrap().unwrap();
    assert_eq!(lease.rented_until, until + 30 * FILETIME_DAY);
    let paid = bills_dao::get(&db, bill).await.unwrap().unwrap();
    assert_eq!(paid.status, infinity_core::consts::BILL_STATUS_PAID);
    // The same bill cannot settle twice through either payer.
    assert!(settlement::pay(&db, &world, bill, settlement::Payer::Automatic { owner: bills_dao::BillOwner::Corporation(corp) }).await.is_err());
    let balance: String = db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id = $1 AND account_key = $2", &[&corp, &ACCOUNT_CASH]).await.unwrap().get("balance");
    assert_eq!(balance, "900.00");
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}

#[cfg(feature = "tests-economy")]
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn character_payment_is_pure_debt_and_foreign_bill_refuses() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (sde, world) = world_of(&db).await;
    let actor = fixture::create(&db).await.unwrap();
    let other = fixture::create(&db).await.unwrap();
    db.execute("UPDATE game.characters SET balance = 1000 WHERE character_id = $1", &[&actor.character]).await.unwrap();
    let amount = IskAmount::from_isk(42.0).unwrap();
    let creditor = sde.map.stations().find_map(|station| station.owner_id).expect("station landlord");
    // MarketFine (bill type 1): a known non-office debt settles the amount only.
    let bill: i64 = db.query_one("INSERT INTO game.bills (bill_type, debtor_id, creditor_id, amount, due, created) VALUES (1, $1, $3, $2::text::numeric, 0, 0) RETURNING bill_id",
        &[&actor.character, &amount.decimal(), &creditor]).await.unwrap().get("bill_id");
    settlement::pay(&db, &world, bill, settlement::Payer::Character { actor: actor.character }).await.unwrap();
    assert_eq!(bills_dao::get(&db, bill).await.unwrap().unwrap().status, infinity_core::consts::BILL_STATUS_PAID);
    let balance: String = db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id = $1", &[&actor.character]).await.unwrap().get("balance");
    assert_eq!(balance, "958.00");
    let foreign: i64 = db.query_one("INSERT INTO game.bills (bill_type, debtor_id, creditor_id, amount, due, created) VALUES (1, $1, $1, $2::text::numeric, 0, 0) RETURNING bill_id",
        &[&other.character, &amount.decimal()]).await.unwrap().get("bill_id");
    assert!(settlement::pay(&db, &world, foreign, settlement::Payer::Character { actor: actor.character }).await.is_err());
    db.execute("DELETE FROM game.bills WHERE bill_id = ANY($1)", &[&vec![bill, foreign]]).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, other).await.unwrap();
}

#[cfg(feature = "tests-economy")]
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn stale_term_bill_refuses_instead_of_reviving_the_office() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (sde, world) = world_of(&db).await;
    let (actor, corp, station, office, until) = rented_fixture(&db, &sde, &world).await;
    // A renewal bill whose boundary already passed cannot be paid into a new term.
    let past = until - 30 * FILETIME_DAY;
    let amount = IskAmount::from_isk(100.0).unwrap();
    let bill: i64 = db.query_one("INSERT INTO game.bills (bill_type, debtor_id, creditor_id, amount, due, created, external_id, external_id2, period_start, period_end) VALUES ($1, $2, $2, $3::text::numeric, $4, 0, $5, $6, $4, $7) RETURNING bill_id",
        &[&BILL_RENTAL, &corp, &amount.decimal(), &past, &office, &station, &until]).await.unwrap().get("bill_id");
    assert!(settlement::pay(&db, &world, bill, settlement::Payer::Corporation { actor: actor.character, corporation: corp, key: ACCOUNT_CASH }).await.is_err());
    assert_eq!(offices_dao::get(&db, office).await.unwrap().unwrap().rented_until, until);
    assert_eq!(bills_dao::get(&db, bill).await.unwrap().unwrap().status, infinity_core::consts::BILL_STATUS_UNPAID);
    db.execute("DELETE FROM game.bills WHERE bill_id = $1", &[&bill]).await.unwrap();
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}

#[cfg(feature = "tests-economy")]
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn mixed_owner_settings_refuse_without_changing_the_valid_owner() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (sde, world) = world_of(&db).await;
    let (actor, corp, _station, office, _) = rented_fixture(&db, &sde, &world).await;
    bills_dao::set_setting(&db, corp, BILL_RENTAL, false).await.unwrap();
    let entry = Value::Dict(vec![
        (Value::Int(i64::from(BILL_RENTAL)), Value::Bool(true)),
        (Value::str("divisionID"), Value::Int(i64::from(ACCOUNT_CASH))),
    ]);
    let changes = Value::Dict(vec![(Value::Int(corp), entry.clone()), (Value::Int(corp + 1), entry)]);
    assert!(settings::save(&db, actor.character, Some(&changes)).await.is_err());
    let settings = bills_dao::settings(&db, corp).await.unwrap();
    assert!(settings.iter().any(|setting| setting.bill_type == BILL_RENTAL && !setting.enabled));
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}

#[cfg(feature = "tests-economy")]
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn alliance_manual_and_automatic_use_independent_divisions_and_current_executor() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (sde, world) = world_of(&db).await;
    let (actor, corp, station, office, _) = rented_fixture(&db, &sde, &world).await;
    // The billing handler below first scans due leases; push this lease's
    // boundary beyond the configured advance window so it issues no renewal.
    let until = crate::client::filetime_now() + crate::config::game().corporations.advance_billing_days.saturating_add(1).saturating_mul(FILETIME_DAY);
    db.execute("UPDATE game.offices SET rented_until = $2 WHERE office_id = $1", &[&office, &until]).await.unwrap();
    let alliance: i64 = db.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('bill-' || nextval('game.alliances_alliance_id_seq')::text,'B' || currval('game.alliances_alliance_id_seq')::text,$1,$1,$2,0) RETURNING alliance_id", &[&corp, &actor.character]).await.unwrap().get("alliance_id");
    db.execute("UPDATE game.corporations SET alliance_id=$2,bill_division=$3 WHERE corporation_id=$1", &[&corp, &alliance, &ACCOUNT_CASH]).await.unwrap();
    let selected = ACCOUNT_CASH + 1;
    db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,1000) ON CONFLICT (corporation_id,account_key) DO UPDATE SET balance=EXCLUDED.balance", &[&corp, &selected]).await.unwrap();
    bills_dao::set_division(&db, bills_dao::BillOwner::Alliance(alliance), selected).await.unwrap();
    bills_dao::set_setting(&db, alliance, 1, true).await.unwrap();
    let creditor = sde.map.station(station).and_then(|row| row.owner_id).unwrap();
    let bill = || async {
        db.query_one("INSERT INTO game.bills (bill_type,debtor_id,creditor_id,amount,due,created) VALUES (1,$1,$2,100,0,0) RETURNING bill_id", &[&alliance, &creditor]).await.unwrap().get::<_, i64>("bill_id")
    };
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(actor.character, sender);
    let manual = bill().await;
    settlement::pay(&db, &world, manual, settlement::Payer::Alliance { actor: actor.character, alliance, executor: corp, key: ACCOUNT_CASH }).await.unwrap();
    let automatic = bill().await;
    maintenance::run_billing(db.clone(), world.clone(), crate::scheduler::Job { id: crate::scheduler::BILLS.into(), kind: crate::scheduler::BILLS.into(), due_at: crate::client::filetime_now(), payload: "{}".into() }).await.unwrap();
    assert_eq!(bills_dao::get(&db, automatic).await.unwrap().unwrap().status, infinity_core::consts::BILL_STATUS_PAID);
    let mut notices = 0;
    while let Ok((event, _)) = receiver.try_recv() {
        if event == "OnBillReceived" { notices += 1; }
    }
    assert_eq!(notices, 2);
    for key in [ACCOUNT_CASH, selected] {
        let balance: String = db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&corp, &key]).await.unwrap().get("balance");
        assert_eq!(balance, "900.00");
    }
    bills_dao::set_setting(&db, alliance, 1, false).await.unwrap();
    let disabled = bill().await;
    assert!(settlement::pay(&db, &world, disabled, settlement::Payer::Automatic { owner: bills_dao::BillOwner::Alliance(alliance) }).await.is_err());
    assert_eq!(bills_dao::get(&db, disabled).await.unwrap().unwrap().status, infinity_core::consts::BILL_STATUS_UNPAID);
    let other = fixture::create(&db).await.unwrap();
    let other_corp = fixture::create_corporation(&db, other.character, station).await.unwrap();
    db.execute("UPDATE game.alliances SET executor_corp_id=$2 WHERE alliance_id=$1", &[&alliance, &other_corp]).await.unwrap();
    let stale = bill().await;
    assert!(settlement::pay(&db, &world, stale, settlement::Payer::Alliance { actor: actor.character, alliance, executor: corp, key: ACCOUNT_CASH }).await.is_err());
    world.unregister_pilot_generation(actor.character, None);
    db.execute("DELETE FROM game.bills WHERE debtor_id=$1", &[&alliance]).await.unwrap();
    db.execute("DELETE FROM game.bill_autopay WHERE owner_id=$1", &[&alliance]).await.unwrap();
    db.execute("UPDATE game.corporations SET alliance_id=NULL WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.alliances WHERE alliance_id=$1", &[&alliance]).await.unwrap();
    fixture::cleanup_corporation(&db, other_corp, &[other.character], 0).await.unwrap();
    fixture::cleanup(&db, other).await.unwrap();
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}
