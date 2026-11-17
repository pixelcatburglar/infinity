//! Real-database lifecycle check for station reprocessing character bonuses: quotes
//! and settlement follow the pilot's skills, installed implants and active boosters.
//! Scrap metal ignores the character yield; ore and ice yields cap at 100% efficiency.
//! Output selections keep their requested owner across the default hangar, the
//! session ship's cargo and office divisions; refusals leave no side effects.
use super::*;
use crate::dao::{characters::selection, items::{brain, inventory, loading::ItemQuery, transfers}};
use crate::db::transaction::TxFuture;
use crate::isk::IskAmount;
use crate::services::items::load_items;
use infinity_core::consts::{dogma::{ATTR_BOOSTERNESS, ATTR_IMPLANTNESS, ATTR_SKILL_TIME_CONSTANT}, CATEGORY_SHIP, FILETIME_HOUR, FLAG_BOOSTER, FLAG_CARGO, FLAG_CORP_SAG1, FLAG_IMPLANT, TYPE_OFFICE};
use std::collections::HashMap;

// Fixture type IDs; every binding below is asserted against the loaded SDE first.
const ORE: i32 = 1230;
const ICE: i32 = 16262;
const SCRAP: i32 = 15331;
const IMPLANT: i32 = 27174;
const BOOSTER: i32 = 59798;

async fn rpc(c: &mut Client, method: &str, args: &[Value]) -> Value {
    dispatch(c, &Call { service: "reprocessingSvc", method, args, kwargs: &Value::None, bound: true, bind_params: None }).await.expect("bound reprocessingSvc").expect(method)
}

/// One short transaction holding the character lock, as the brain DAO requires.
async fn brain_tx<R: Send + 'static>(db: &Db, char_id: i64, run: impl for<'call, 'tx> Fn(&'call crate::db::Transaction<'tx>) -> TxFuture<'call, R, crate::db::Error> + Sync) -> R {
    db.transaction(|tx, _: &mut ()| { let pending = run(tx); Box::pin(async move { selection::lock_character(tx, char_id).await?; pending.await }) }, |v, ()| async move { Ok(v) }).await.unwrap()
}

/// Insert one unit, mark it singleton and install it in its brain slot.
async fn install(tx: &crate::db::Transaction<'_>, sde: &crate::sde::Sde, char_id: i64, station: i64, type_id: i32, flag: i32, times: (Option<i64>, Option<i64>)) -> Result<i64, crate::db::Error> {
    let id = transfers::insert(tx, type_id, char_id, station, FLAG_HANGAR, 1).await?;
    let item = load_items(tx, sde, ItemQuery::Id(id)).await?.into_iter().next().unwrap();
    assert!(inventory::move_item(tx, &item, station, FLAG_HANGAR, char_id, true).await?);
    let slot = sde.types.attribute(type_id, if flag == FLAG_IMPLANT { ATTR_IMPLANTNESS } else { ATTR_BOOSTERNESS }).unwrap() as i32;
    assert!(brain::install(tx, id, char_id, station, FLAG_HANGAR, flag, slot, times.0, times.1, (flag == FLAG_BOOSTER).then_some(&[][..])).await?);
    Ok(id)
}

/// Shared test boot: game config, dedicated database and one isolated fixture.
async fn boot() -> (Db, crate::services::tests::fixture::Fixture) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    (db, fixture)
}

/// Bound client docked at the first SDE station, funded wallet, pilot channel.
async fn client_at(db: &Db, char_id: i64) -> (Client, tokio::net::TcpStream, tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>, i64) {
    let (mut c, peer) = crate::services::tests::client(db.clone(), char_id).await;
    let station = c.sde.map.stations().next().expect("SDE station").station_id;
    c.session.apply(vec![("stationid", Value::Int(station))]);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(char_id, tx);
    crate::dao::wallets::characters::set(db, char_id, IskAmount::from_isk(1_000_000.0).unwrap()).await.unwrap();
    assert!(c.sde.types.get(ICE).unwrap().materials().iter().any(|m| c.sde.prices.get(m.material_type_id).is_some_and(|p| p.average > 0.0)), "ICE must produce priced materials");
    (c, peer, rx, station)
}

/// Sorted (item, type, owner, flag, quantity) rows of `owners` at one location.
async fn rows_at(db: &Db, sde: &crate::sde::Sde, location: i64, owners: &[i64]) -> Vec<(i64, i32, i64, i32, i64)> {
    let mut rows: Vec<_> = load_items(db, sde, ItemQuery::Location(location)).await.unwrap().into_iter().filter(|i| owners.contains(&i.owner_id)).map(|i| (i.item_id, i.type_id, i.owner_id, i.flag_id, i.quantity)).collect();
    rows.sort_unstable();
    rows
}

/// Every reply material exists only at (owner, flag) within `rows`.
fn check_outputs(rows: &[(i64, i32, i64, i32, i64)], owner: i64, flag: i32, reply: &Value) {
    let Value::Tuple(parts) = reply else { panic!("expected reply tuple") };
    let Value::Dict(materials) = &parts[1] else { panic!("expected material dict") };
    assert!(materials.len() > 1, "expected multiple produced materials");
    for (key, value) in materials {
        let (ty, want) = (key.as_int().unwrap() as i32, value.as_int().unwrap());
        assert!(want > 0 && rows.iter().filter(|r| r.1 == ty).map(|r| r.4).sum::<i64>() == want && rows.iter().filter(|r| r.1 == ty).all(|r| r.2 == owner && r.3 == flag), "material {ty} misplaced or wrong quantity");
    }
}

/// A committed call notifies directly (OnItemChange) and via the pilot channel (OnAccountChange).
fn assert_notified(c: &Client, pilot_rx: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>) {
    assert!(c.outbox.iter().any(|p| matches!(&p.destination, infinity_wire::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnItemChange"))));
    assert!(std::iter::from_fn(|| pilot_rx.try_recv().ok()).any(|(name, _)| name == "OnAccountChange"));
}

/// One singleton office item at the station plus its permanent lease row.
async fn office_at(db: &Db, sde: &crate::sde::Sde, corporation: i64, station: i64) -> i64 {
    let office = transfers::insert(db, TYPE_OFFICE, corporation, station, FLAG_HANGAR, 1).await.unwrap();
    let item = load_items(db, sde, ItemQuery::Id(office)).await.unwrap().into_iter().next().unwrap();
    assert!(inventory::move_item(db, &item, station, FLAG_HANGAR, corporation, true).await.unwrap());
    crate::services::tests::fixture::create_office(db, office, corporation, station).await.unwrap();
    office
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn reprocessing_yield_brain_lifecycle_matches_quotes_and_settlement() {
    let (db, fixture) = boot().await;
    let result = crate::tests::catch_scenario(async {
        let sde = crate::tests::shared_sde(&db).await;
        let data = |id: i32| sde.types.get(id).unwrap_or_else(|| panic!("SDE type {id}"));
        assert_eq!((data(ORE).portion_size, data(ORE).attribute(ATTR_PROCESSING_SKILL)), (Some(100), Some(60377.0)));
        assert_eq!(data(ORE).materials().iter().map(|m| (m.material_type_id, m.quantity)).collect::<Vec<_>>(), [(34, Some(400))]);
        assert_eq!((data(1373).attribute(ATTR_REFINING_YIELD_PERCENTAGE), data(3385).attribute(ATTR_REFINING_YIELD_MUTATOR), data(3389).attribute(ATTR_REFINING_YIELD_MUTATOR)), (Some(1.0), Some(3.0), Some(2.0)));
        assert_eq!((data(ICE).portion_size, data(ICE).attribute(ATTR_PROCESSING_SKILL)), (Some(1), Some(18025.0)));
        assert_eq!(data(ICE).materials().iter().map(|m| (m.material_type_id, m.quantity)).collect::<Vec<_>>(), [(16272, Some(69)), (16273, Some(35)), (16274, Some(414)), (16275, Some(1))]);
        assert_eq!((data(SCRAP).portion_size, data(SCRAP).attribute(ATTR_PROCESSING_SKILL)), (Some(1), None));
        assert_eq!(data(SCRAP).materials().iter().map(|m| (m.material_type_id, m.quantity)).collect::<Vec<_>>(), [(34, Some(500))]);
        assert_eq!(sde.dogma.effect(435).unwrap().effect_category, Some(0));
        for (id, group, raw) in [(27169, 1229, 2.0), (IMPLANT, 1229, 4.0), (27175, 1229, 1.0), (BOOSTER, 303, 8.0), (59800, 303, 8.0), (59802, 303, 8.0)] {
            let ty = data(id);
            assert_eq!((ty.published, ty.group_id, ty.attribute(ATTR_REFINING_YIELD_MUTATOR)), (Some(true), Some(group), Some(raw)), "type {id}");
            assert!(ty.modifiers(&sde.dogma, ATTR_REFINING_YIELD_PERCENTAGE).any(|(e, m)| e.effect_id == 435 && m.func == "ItemModifier" && m.domain == "charID" && m.op == 6 && m.modified == ATTR_REFINING_YIELD_PERCENTAGE && m.modifying == ATTR_REFINING_YIELD_MUTATOR), "type {id} yield binding");
        }
        let char_id = fixture.character;
        let (mut c, _peer) = crate::services::tests::client(db.clone(), char_id).await;
        let station = sde.map.stations().next().expect("SDE station").station_id;
        c.session.apply(vec![("stationid", Value::Int(station))]);
        for (skill, level) in [(3385, 4), (3389, 3), (60377, 2), (18025, 2), (12196, 3)] {
            let sp = crate::services::skills::formulas::sp_for_level(sde.types.attribute(skill, ATTR_SKILL_TIME_CONSTANT).unwrap(), i64::from(level));
            crate::dao::skills::training::set_level(&db, char_id, skill, level, sp).await.unwrap();
        }
        let cfg = &crate::config::game().mining;
        let mats: Vec<i32> = [ORE, ICE, SCRAP].into_iter().flat_map(|t| data(t).materials().iter().map(|m| m.material_type_id)).collect();
        let prices: HashMap<i32, f64> = db.query("SELECT type_id, average FROM game.retail_prices WHERE type_id = ANY($1)", &[&mats]).await.unwrap().into_iter().map(|r| (r.get::<_, i32>("type_id"), r.get::<_, f64>("average"))).collect();
        let mut wallet = IskAmount::from_isk(1_000_000.0).unwrap();
        crate::dao::wallets::characters::set(&db, char_id, wallet).await.unwrap();
        // Independent expectation: whole batches, floored over the merged total.
        let want_quote = |ty: i32, qty: i64, mult: f64| {
            let (portion, efficiency) = (i64::from(data(ty).portion_size.unwrap_or(1).max(1)), (cfg.station_reprocessing_efficiency * mult).min(1.0));
            Quote { type_id: ty, leftover: qty % portion, materials: data(ty).materials().iter().filter_map(|m| {
                let total = m.quantity.unwrap_or(0) * (qty / portion);
                let received = (total as f64 * efficiency).floor() as i64;
                (total > 0).then(|| (m.material_type_id, received, total - received, received as f64 * prices.get(&m.material_type_id).copied().unwrap_or(0.0) * cfg.station_reprocessing_tax))
            }).collect() }
        };
        let (mut produced, mut brain_item, now) = (BTreeMap::<i32, i64>::new(), 0, filetime_now());
        for (state, bonus) in [1.0, 1.04, 1.08, 1.0, 3.0].into_iter().enumerate() {
            match state {
                1 => brain_item = brain_tx(&db, char_id, |tx| { let sde = Arc::clone(&sde); Box::pin(async move { install(tx, &sde, char_id, station, IMPLANT, FLAG_IMPLANT, (None, None)).await }) }).await,
                2 => brain_item = brain_tx(&db, char_id, |tx| { let sde = Arc::clone(&sde); Box::pin(async move {
                    let old = brain::get(tx, char_id, brain_item).await?.expect("installed implant");
                    assert!(brain::remove(tx, char_id, &old).await?);
                    install(tx, &sde, char_id, station, BOOSTER, FLAG_BOOSTER, (Some(now - FILETIME_SECOND), Some(now + 3 * FILETIME_HOUR))).await }) }).await,
                3 => { brain_tx(&db, char_id, |tx| Box::pin(async move {
                    let old = brain::get(tx, char_id, brain_item).await?.expect("installed booster");
                    assert!(brain::update_metadata(tx, char_id, &old, old.slot, old.activated_at, Some(now - FILETIME_SECOND), old.effect_ids.as_deref()).await?);
                    Ok(()) })).await;
                    assert!(brain::get(&db, char_id, brain_item).await.unwrap().is_some()); }
                4 => { let id = brain_tx(&db, char_id, |tx| { let sde = Arc::clone(&sde); Box::pin(async move { install(tx, &sde, char_id, station, IMPLANT, FLAG_IMPLANT, (None, None)).await }) }).await;
                    assert!(crate::dao::items::dogma::set_override(&db, id, ATTR_REFINING_YIELD_MUTATOR, 200.0).await.unwrap()); }
                _ => {}
            }
            let mult = 1.12 * 1.06 * 1.04 * bonus; // skills x specialization x brain effect
            for (t, want) in [(ORE, mult), (ICE, mult), (SCRAP, 1.06)] {
                let got = rpc(&mut c, "GetCharRefiningYieldPercentageForType", &[Value::Int(char_id), Value::Int(i64::from(t))]).await.as_float().unwrap();
                assert!((got - want).abs() < 1e-9, "state {state} type {t}: {got} != {want}");
            }
            let inputs = [(ORE, 201_i64, mult), (ICE, 2, mult), (SCRAP, 2, 1.06)];
            let mut ids = Vec::new();
            for &(t, q, _) in &inputs { ids.push(transfers::insert(&db, t, char_id, station, FLAG_HANGAR, q).await.unwrap()); }
            let want = Value::tuple([Value::Float(cfg.station_reprocessing_tax), Value::Dict(vec![(Value::None, Value::Float(cfg.station_reprocessing_efficiency))]), Value::Dict(ids.iter().zip(&inputs).map(|(&i, &(t, q, m))| (Value::Int(i), quote_value(i, &want_quote(t, q, m)))).collect())]);
            assert_eq!(rpc(&mut c, "GetQuotes", &[Value::list(ids.iter().map(|&i| Value::Int(i))), Value::Int(0)]).await, want);
            let (mut out, mut tax) = (BTreeMap::<i32, i64>::new(), 0.0);
            for &(t, q, m) in &inputs { for &(mt, r, _, isk) in &want_quote(t, q, m).materials { *out.entry(mt).or_default() += r; tax += isk; } }
            assert_eq!(rpc(&mut c, "Reprocess", &[Value::list(ids.iter().map(|&i| Value::Int(i))), Value::Int(station), Value::Int(char_id), Value::Int(station), Value::Int(i64::from(FLAG_HANGAR))]).await,
                Value::tuple([Value::List(ids.iter().map(|&i| Value::Int(i)).collect()), Value::Dict(out.iter().map(|(&t, &q)| (Value::Int(i64::from(t)), Value::Int(crate::services::dogma::info::cap_client_quantity(q)))).collect())]));
            for (&t, &q) in &out { *produced.entry(t).or_default() += q; }
            wallet = wallet.checked_sub(IskAmount::from_isk(tax).unwrap()).unwrap();
            assert_eq!(crate::services::wallets::balance(&db, char_id).await.unwrap(), wallet);
            assert_eq!(load_items(&db, &sde, ItemQuery::Ids(&ids)).await.unwrap().iter().map(|i| (i.item_id, i.quantity)).collect::<Vec<_>>(), [(ids[0], 1)]);
            let mut got = BTreeMap::<i32, i64>::new();
            for i in load_items(&db, &sde, ItemQuery::OwnerLocationFlag(char_id, station, FLAG_HANGAR)).await.unwrap() { if mats.contains(&i.type_id) { *got.entry(i.type_id).or_default() += i.quantity; } }
            assert_eq!(got, produced, "state {state}");
        }
    }).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn reprocessing_output_selections_preserve_ownership() {
    let (db, fixture) = Box::pin(boot()).await;
    let (mut corporation, mut office) = (0_i64, 0_i64);
    let result = crate::tests::catch_scenario(Box::pin(async {
        let sde = crate::tests::shared_sde(&db).await;
        let char_id = fixture.character;
        let (mut c, _peer, mut pilot_rx, station) = Box::pin(client_at(&db, char_id)).await;
        let authority = crate::services::corps::permissions::load(&db, char_id).await.unwrap().expect("fixture authority");
        assert_eq!((authority.corporation, authority.live, authority.roles), (1, false, [0; 4]), "fixture stays in its NPC corporation with zero roles");
        c.session.apply(vec![("corpid", Value::Int(1))]);
        // Omitted output selection defaults to the personal hangar here.
        let ids = [transfers::insert(&db, ICE, char_id, station, FLAG_HANGAR, 10).await.unwrap()];
        let reply = rpc(&mut c, "Reprocess", &[Value::list(ids.map(Value::Int)), Value::Int(station), Value::Int(char_id)]).await;
        check_outputs(&rows_at(&db, &sde, station, &[char_id]).await, char_id, FLAG_HANGAR, &reply);
        assert!(load_items(&db, &sde, ItemQuery::Ids(&ids)).await.unwrap().is_empty());
        // Same-location output into the session ship's cargo hold.
        let hull = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true)).max_by_key(|t| t.capacity.unwrap_or(0.0) as i64).expect("large cargo hull").type_id;
        let ship = crate::dao::items::creation::rookie_ship(&db, hull, char_id, station, FLAG_HANGAR).await.unwrap();
        c.session.apply(vec![("shipid", Value::Int(ship))]);
        let ids = [transfers::insert(&db, ICE, char_id, ship, FLAG_CARGO, 10).await.unwrap()];
        let reply = rpc(&mut c, "Reprocess", &[Value::list(ids.map(Value::Int)), Value::Int(ship), Value::Int(char_id), Value::Int(ship), Value::Int(i64::from(FLAG_CARGO))]).await;
        check_outputs(&rows_at(&db, &sde, ship, &[char_id]).await, char_id, FLAG_CARGO, &reply);
        assert!(load_items(&db, &sde, ItemQuery::Ids(&ids)).await.unwrap().is_empty());
        // Office division: real membership, zero roles, corporation owner.
        corporation = crate::services::tests::fixture::create_corporation(&db, char_id, station).await.unwrap();
        office = office_at(&db, &sde, corporation, station).await;
        crate::services::tests::fixture::assign_corporation(&db, char_id, corporation, 0).await.unwrap();
        c.session.apply(vec![("corpid", Value::Int(corporation))]);
        let ids = [transfers::insert(&db, ICE, char_id, station, FLAG_HANGAR, 10).await.unwrap()];
        let division = FLAG_CORP_SAG1 + 2;
        let reply = rpc(&mut c, "Reprocess", &[Value::list(ids.map(Value::Int)), Value::Int(station), Value::Int(corporation), Value::Int(office), Value::Int(i64::from(division))]).await;
        check_outputs(&rows_at(&db, &sde, office, &[char_id, corporation]).await, corporation, division, &reply);
        assert!(load_items(&db, &sde, ItemQuery::Ids(&ids)).await.unwrap().is_empty());
        assert!(crate::services::wallets::balance(&db, char_id).await.unwrap() < IskAmount::from_isk(1_000_000.0).unwrap());
        assert_notified(&c, &mut pilot_rx);
    })).await;
    crate::services::tests::fixture::cleanup_corporation(&db, corporation, &[fixture.character], office).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn reprocessing_refuses_unavailable_outputs_without_side_effects() {
    let (db, fixture) = Box::pin(boot()).await;
    let (mut corporation, mut office) = (0_i64, 0_i64);
    let result = crate::tests::catch_scenario(Box::pin(async {
        let sde = crate::tests::shared_sde(&db).await;
        let char_id = fixture.character;
        let (mut c, _peer, mut pilot_rx, station) = Box::pin(client_at(&db, char_id)).await;
        let remote = sde.map.stations().find(|s| s.station_id != station).expect("second SDE station").station_id;
        // Active office lease of a corporation the character never joined.
        corporation = crate::services::tests::fixture::create_corporation(&db, char_id, station).await.unwrap();
        office = office_at(&db, &sde, corporation, station).await;
        c.session.apply(vec![("corpid", Value::Int(corporation))]);
        let ids = [transfers::insert(&db, ICE, char_id, station, FLAG_HANGAR, 10).await.unwrap()];
        let balance = crate::services::wallets::balance(&db, char_id).await.unwrap();
        let before = (rows_at(&db, &sde, station, &[char_id, corporation]).await, rows_at(&db, &sde, remote, &[char_id, corporation]).await, rows_at(&db, &sde, office, &[char_id, corporation]).await);
        // (owner hint, output location, output flag, real member): refused shapes.
        for (index, (owner, out, flag, member)) in [
            (char_id, remote, Value::Int(i64::from(FLAG_HANGAR)), false),
            (corporation, office, Value::Int(i64::from(FLAG_CORP_SAG1)), false),
            (char_id, station, Value::Int(i64::from(FLAG_HANGAR) + (1_i64 << 32)), false),
            (char_id, station, Value::Int(i64::from(FLAG_CARGO)), false),
            (corporation, office, Value::Int(i64::from(FLAG_HANGAR)), true),
        ].into_iter().enumerate() {
            if member { crate::services::tests::fixture::assign_corporation(&db, char_id, corporation, 0).await.unwrap(); }
            let args = [Value::list(ids.map(Value::Int)), Value::Int(station), Value::Int(owner), Value::Int(out), flag];
            let reply = dispatch(&mut c, &Call { service: "reprocessingSvc", method: "Reprocess", args: &args, kwargs: &Value::None, bound: true, bind_params: None }).await.expect("bound");
            assert!(matches!(reply, Err(CallError::User { .. })), "case {index}");
            assert_eq!((rows_at(&db, &sde, station, &[char_id, corporation]).await, rows_at(&db, &sde, remote, &[char_id, corporation]).await, rows_at(&db, &sde, office, &[char_id, corporation]).await), before, "case {index}");
            assert_eq!(crate::services::wallets::balance(&db, char_id).await.unwrap(), balance, "case {index}");
            assert!(c.outbox.is_empty() && pilot_rx.try_recv().is_err(), "case {index}");
        }
        // Positive control: the untouched source still settles normally.
        let args = [Value::list(ids.map(Value::Int)), Value::Int(station), Value::Int(char_id), Value::Int(station), Value::Int(i64::from(FLAG_HANGAR))];
        let reply = rpc(&mut c, "Reprocess", &args).await;
        check_outputs(&rows_at(&db, &sde, station, &[char_id]).await, char_id, FLAG_HANGAR, &reply);
        assert!(load_items(&db, &sde, ItemQuery::Ids(&ids)).await.unwrap().is_empty());
        assert!(crate::services::wallets::balance(&db, char_id).await.unwrap() < balance);
        assert_notified(&c, &mut pilot_rx);
    })).await;
    crate::services::tests::fixture::cleanup_corporation(&db, corporation, &[fixture.character], office).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
