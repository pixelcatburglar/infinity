//! Database regressions for corporate contract lists and inventory picking.
//! Exercises contractProxy's client-facing list shapes and custody decisions against game data.
//! Requires the dedicated test database and loaded SDE; no client integration is covered here.

use super::super::*;
use crate::consts::{CATEGORY_CHARGE, CORP_HANGAR_DIVISIONS, FLAG_CARGO, FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1, ROLE_ACCOUNTANT, ROLE_CONTRACT_MANAGER, ROLE_HANGAR_CAN_QUERY_1, ROLE_HANGAR_CAN_TAKE_1, ROLE_JUNIOR_ACCOUNTANT};
use crate::services::tests::fixture;

fn success<T>(result: Result<T, CallError>) -> T {
    match result { Ok(value) => value, Err(CallError::Internal(error)) => panic!("internal error: {error:#}"), Err(CallError::User { msg, .. }) => panic!("user error: {msg}") }
}
fn attr<'a>(value: &'a Value, name: &str) -> &'a Value {
    super::super::super::field(value, name).unwrap_or_else(|| panic!("missing {name}"))
}
fn number(value: &Value, name: &str) -> i64 { attr(value, name).as_int().unwrap() }
fn ids(value: &Value) -> Vec<i64> {
    value.items().unwrap().iter().map(|row| number(row, "contractID")).collect()
}

async fn item_row(db: &Db, sde: &Sde, id: i64) -> Value {
    load_items(db, sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap().row()
}

async fn new_contract(db: &Db, issuer: i64, corp: i64, station: i64, corporate: bool, assignee: i64, kind: i32, expires: i64) -> i64 {
    crate::dao::contracts::lifecycle::insert(db, &crate::dao::contracts::lifecycle::NewContract {
        kind, issuer, issuer_corp: corp, for_corp: corporate,
        issuer_wallet_key: if corporate { ACCOUNT_CASH + 2 } else { ACCOUNT_CASH },
        issuer_hangar_flag: if corporate { FLAG_CORP_DELIVERIES } else { FLAG_HANGAR },
        issuer_source_location: station, assignee, start: station, end: station,
        price: 0.0, reward: 0.0, collateral: 0.0, title: "fixture", description: "fixture",
        paid_deposit: 0.0, auction_tax_rate: 0.0, auction_tax_maximum: 0.0,
        num_days: 1, status: STATUS_OUTSTANDING, issued: filetime_now(), expires,
    }).await.unwrap()
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn corporate_lists_counts_search_and_wire() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station_data = client.sde.map.stations().find(|s| s.station_id > 0).unwrap();
    let station = station_data.station_id;
    let system = station_data.solar_system_id.unwrap_or(0);
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name, ticker, ceo_id, creator_id, station_id, created) VALUES ('contract-list-' || nextval('game.corporations_corporation_id_seq')::text, 'L' || nextval('game.corporations_corporation_id_seq')::text, $1, $1, $2, 0) RETURNING corporation_id", &[&ceo.character, &station]).await.unwrap().get("corporation_id");
    db.execute("UPDATE game.characters SET corporation_id = $1 WHERE character_id = ANY($2)", &[&corp, &vec![actor.character, ceo.character]]).await.unwrap();
    client.session.apply(vec![("corpid", Value::Int(corp)), ("corpAccountKey", Value::Int(i64::from(ACCOUNT_CASH + 2))), ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system))]);
    let task_db = db.clone();
    let task = tokio::spawn(async move {
        let db = task_db;
        let future = filetime_now() + FILETIME_DAY;
        let personal = new_contract(&db, actor.character, corp, station, false, 0, TYPE_ITEM_EXCHANGE, future).await;
        let personal_internal = new_contract(&db, actor.character, corp, station, false, ceo.character, TYPE_ITEM_EXCHANGE, future).await;
        let corporate = new_contract(&db, actor.character, corp, station, true, 0, TYPE_ITEM_EXCHANGE, future).await;
        let corporate_internal = new_contract(&db, actor.character, corp, station, true, corp, TYPE_ITEM_EXCHANGE, future).await;
        let expired = new_contract(&db, actor.character, corp, station, true, 0, TYPE_ITEM_EXCHANGE, filetime_now() - 1).await;
        let accepted = new_contract(&db, ceo.character, corp, station, false, 0, TYPE_COURIER, future).await;
        assert!(crate::dao::contracts::lifecycle::accept_courier(&db, accepted, STATUS_OUTSTANDING, corp, ACCOUNT_CASH + 3, filetime_now(), STATUS_IN_PROGRESS, 0).await.unwrap());
        let bid = new_contract(&db, ceo.character, corp, station, false, 0, TYPE_AUCTION, future).await;
        crate::dao::contracts::lifecycle::insert_bid(&db, bid, corp, ACCOUNT_CASH + 4, 1.0, filetime_now()).await.unwrap();
        let count = success(counts(&client).await);
        assert_eq!((number(&count, "myCharTotal"), number(&count, "nonCorpForMyChar"), number(&count, "myCorpTotal"), number(&count, "nonCorpForMyCorp")), (2, 1, 3, 2));
        let page = success(page_info(&client).await);
        assert_eq!((number(&page, "numOutstandingContracts"), number(&page, "numOutstandingContractsNonCorp"), number(&page, "numOutstandingContractsForCorp")), (2, 1, 2));
        assert_eq!(number(&page, "numBiddingOnCorp"), 1);
        assert_eq!(number(&page, "numInProgressCorp"), 1);
        assert_eq!(number(&page, "numRequiresAttentionCorp"), 1);
        assert_eq!(success(super::super::super::max_outstanding(&db, actor.character, false).await), 1);
        assert_eq!(success(super::super::super::max_outstanding(&db, actor.character, true).await), 10);
        for skill in [SKILL_CONTRACTING, SKILL_ADVANCED_CONTRACTING, SKILL_CORPORATION_CONTRACTING] {
            db.execute("INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, $2, 5, 0)", &[&actor.character, &skill]).await.unwrap();
        }
        assert_eq!(success(super::super::super::max_outstanding(&db, actor.character, false).await), 41);
        assert_eq!(success(super::super::super::max_outstanding(&db, actor.character, true).await), 60);
        let kwargs = Value::Dict(vec![]);
        for (owner, included, excluded) in [(corp, corporate, personal), (actor.character, personal, corporate), (corp + 1, 0, corporate)] {
            let args = [Value::Int(owner), Value::Int(i64::from(STATUS_OUTSTANDING)), Value::Int(CONTYPE_ALL), Value::Bool(false)];
            let call = Call { service: "contractProxy", method: "GetContractListForOwner", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let list = success(list_for_owner(&client, &call).await);
            let found = ids(attr(&list, "contracts"));
            if included != 0 { assert!(found.contains(&included)); }
            assert!(!found.contains(&excluded));
        }
        for (issuer, availability, wanted, absent) in [(corp, AVAIL_PUBLIC, corporate, personal), (actor.character, AVAIL_PUBLIC, personal, corporate), (corp, AVAIL_MY_CORP, corporate_internal, corporate)] {
            let filters = Value::dict([("issuerID", Value::Int(issuer)), ("availability", Value::Int(availability))]);
            let result = success(search(&client, &filters).await);
            let rows = attr(&result, "contracts").items().unwrap();
            let found: Vec<_> = rows.iter().map(|r| number(attr(r, "contract"), "contractID")).collect();
            assert!(found.contains(&wanted));
            assert!(!found.contains(&absent));
        }
        let wire = success(super::super::super::contract(&db, corporate_internal).await).keyval(&client.sde);
        assert_eq!(number(&wire, "issuerID"), actor.character);
        assert_eq!(number(&wire, "issuerCorpID"), corp);
        assert_eq!(number(&wire, "issuerWalletKey"), i64::from(ACCOUNT_CASH + 2));
        assert_eq!(number(&wire, "availability"), AVAIL_MY_CORP);
        assert_eq!(attr(&wire, "forCorp"), &Value::Bool(true));
        let accepted_wire = success(super::super::super::contract(&db, accepted).await).keyval(&client.sde);
        assert_eq!(number(&accepted_wire, "acceptorWalletKey"), i64::from(ACCOUNT_CASH + 3));
        assert!(success(needing_attention(&db, corp).await).iter().any(|k| k.id == expired));
        assert!(!success(needing_attention(&db, actor.character).await).iter().any(|k| k.id == expired));
        assert!(success(bidding_on(&db, corp).await).iter().any(|k| k.id == bid));
        assert!(success(in_progress(&db, corp).await).iter().any(|k| k.id == accepted));
        let current = query::list(&db, Filter::Issuer(corp, STATUS_OUTSTANDING)).await.unwrap();
        assert!(current.iter().any(|k| k.id == corporate));
        assert!(!query::list(&db, Filter::Issuer(actor.character, STATUS_OUTSTANDING)).await.unwrap().iter().any(|k| k.id == corporate));
        let login = success(login_info(&client).await);
        assert!(ids(attr(&login, "assignedToMe")).contains(&corporate_internal));
        assert!(ids(attr(&login, "inProgress")).contains(&accepted));
        let attention = DbRowDescriptor::new([("contractID", DbType::I8), ("forCorp", DbType::Bool)]);
        assert!(attr(&login, "needsAttention").items().unwrap().iter().any(|row| attention.get(row, "contractID") == Some(Value::Int(expired)) && attention.get(row, "forCorp") == Some(Value::Bool(true))));
        assert!(!ids(attr(&login, "assignedToMe")).contains(&personal_internal));
    });
    let result = task.await;
    db.execute("DELETE FROM game.contract_bids WHERE contract_id IN (SELECT contract_id FROM game.contracts WHERE issuer_id = ANY($1))", &[&vec![actor.character, ceo.character]]).await.unwrap();
    db.execute("DELETE FROM game.contracts WHERE issuer_id = ANY($1)", &[&vec![actor.character, ceo.character]]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id = 1 WHERE character_id = ANY($1)", &[&vec![actor.character, ceo.character]]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id = $1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn corporate_picker_roles_nested_custody_and_split_notifications() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let observer = fixture::create(&db).await.unwrap();
    let other_ceo = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station_data = client.sde.map.stations().find(|s| s.station_id > 0).unwrap();
    let station = station_data.station_id;
    let system = station_data.solar_system_id.unwrap_or(0);
    let type_id = client.sde.types.iter().find(|t| t.type_id > 0 && t.published == Some(true) && t.category_id == Some(CATEGORY_CHARGE)).unwrap().type_id;
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name, ticker, ceo_id, creator_id, station_id, created) VALUES ('contract-picker-' || nextval('game.corporations_corporation_id_seq')::text, 'P' || nextval('game.corporations_corporation_id_seq')::text, $1, $1, $2, 0) RETURNING corporation_id", &[&ceo.character, &station]).await.unwrap().get("corporation_id");
    let other_corp: i64 = db.query_one("INSERT INTO game.corporations (name, ticker, ceo_id, creator_id, station_id, created) VALUES ('contract-foreign-' || nextval('game.corporations_corporation_id_seq')::text, 'F' || nextval('game.corporations_corporation_id_seq')::text, $1, $1, $2, 0) RETURNING corporation_id", &[&other_ceo.character, &station]).await.unwrap().get("corporation_id");
    db.execute("UPDATE game.characters SET corporation_id = $2 WHERE character_id = $1", &[&other_ceo.character, &other_corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id = $1 WHERE character_id = ANY($2)", &[&corp, &vec![actor.character, ceo.character, observer.character]]).await.unwrap();
    client.session.apply(vec![("corpid", Value::Int(corp)), ("corpAccountKey", Value::Int(i64::from(ACCOUNT_CASH))), ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system))]);
    let task_db = db.clone();
    let task = tokio::spawn(async move {
        let db = task_db;
        let office = transfers::insert(&db, type_id, corp, station, 0, 1).await.unwrap();
        crate::dao::offices::insert(&db, office, corp, station, filetime_now() + FILETIME_DAY).await.unwrap();
        let mut divisions = Vec::new();
        for n in 0..CORP_HANGAR_DIVISIONS {
            divisions.push(transfers::insert(&db, type_id, corp, office, FLAG_CORP_SAG1 + n, 10).await.unwrap());
        }
        let deliveries = transfers::insert(&db, type_id, corp, station, FLAG_CORP_DELIVERIES, 10).await.unwrap();
        let root = transfers::insert(&db, type_id, corp, station, FLAG_CORP_DELIVERIES, 1).await.unwrap();
        let child = transfers::insert(&db, type_id, corp, root, FLAG_CARGO, 2).await.unwrap();
        let foreign = transfers::insert(&db, type_id, other_corp, root, FLAG_CARGO, 1).await.unwrap();
        let foreign_nested = transfers::insert(&db, type_id, corp, foreign, FLAG_CARGO, 1).await.unwrap();
        let cycle_a = transfers::insert(&db, type_id, corp, station, FLAG_CORP_DELIVERIES, 1).await.unwrap();
        let cycle_b = transfers::insert(&db, type_id, corp, cycle_a, FLAG_CARGO, 1).await.unwrap();
        db.execute("UPDATE game.items SET location_id = $2, flag_id = $3 WHERE item_id = $1", &[&cycle_a, &cycle_b, &FLAG_CARGO]).await.unwrap();
        let delivery_row = item_row(&db, &client.sde, deliveries).await;
        for n in 0..CORP_HANGAR_DIVISIONS {
            let role = ROLE_HANGAR_CAN_QUERY_1 << n;
            db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor.character, &role]).await.unwrap();
            let listed = success(items_in(&client, station, None, true, FLAG_CORP_SAG1).await);
            let items = listed.items().unwrap();
            for (index, id) in divisions.iter().enumerate() {
                assert_eq!(items.contains(&item_row(&db, &client.sde, *id).await), index == n as usize);
            }
            assert!(!items.contains(&delivery_row));
        }
        db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor.character, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
        assert!(success(items_in(&client, station, None, true, FLAG_CORP_SAG1).await).items().unwrap().is_empty());
        let sag_split = success(split_stack(&client, station, divisions[0], 2, true, FLAG_CORP_SAG1).await).as_int().unwrap();
        let sag_row = load_items(&db, &client.sde, ItemQuery::Id(sag_split)).await.unwrap().pop().unwrap();
        assert_eq!((sag_row.owner_id, sag_row.location_id, sag_row.flag_id, sag_row.quantity), (corp, office, FLAG_CORP_SAG1, 2));
        db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor.character, &ROLE_JUNIOR_ACCOUNTANT]).await.unwrap();
        assert!(success(items_in(&client, station, None, true, FLAG_CORP_DELIVERIES).await).items().unwrap().contains(&delivery_row));
        assert!(success(items_in(&client, station, Some(root), true, FLAG_CORP_DELIVERIES).await).items().unwrap().contains(&item_row(&db, &client.sde, child).await));
        assert!(!success(items_in(&client, station, Some(root), true, FLAG_CORP_DELIVERIES).await).items().unwrap().contains(&item_row(&db, &client.sde, foreign).await));
        assert!(success(items_in(&client, station, Some(foreign_nested), true, FLAG_CORP_DELIVERIES).await).items().unwrap().is_empty());
        let counts = success(container_counts(&client, station, &[root, foreign, foreign_nested, cycle_a], true, FLAG_CORP_DELIVERIES).await);
        match counts { Value::Dict(entries) => {
            assert!(entries.iter().any(|(key, value)| key.as_int() == Some(root) && value.as_int() == Some(1)));
            assert!(!entries.iter().any(|(key, _)| key.as_int() == Some(foreign) || key.as_int() == Some(foreign_nested) || key.as_int() == Some(cycle_a)));
        }, _ => panic!("expected container count dictionary") }
        assert!(success(items_in(&client, 0, None, true, FLAG_CORP_DELIVERIES).await).items().unwrap().is_empty());
        assert!(success(items_in(&client, station, Some(cycle_a), true, FLAG_CORP_DELIVERIES).await).items().unwrap().is_empty());
        let (ceo_tx, mut ceo_rx) = tokio::sync::mpsc::unbounded_channel();
        let (observer_tx, mut observer_rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(ceo.character, ceo_tx);
        client.world.register_pilot(observer.character, observer_tx);
        let before: String = db.query_one("SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') AS fingerprint FROM game.items i WHERE owner_id = $1", &[&corp]).await.unwrap().get("fingerprint");
        assert!(split_stack(&client, station, deliveries, 3, true, FLAG_CORP_DELIVERIES).await.is_err());
        db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor.character, &ROLE_CONTRACT_MANAGER]).await.unwrap();
        assert!(split_stack(&client, station, deliveries, 3, true, FLAG_CORP_DELIVERIES).await.is_err());
        assert_eq!(load_items(&db, &client.sde, ItemQuery::Id(deliveries)).await.unwrap().pop().unwrap().quantity, 10);
        let after: String = db.query_one("SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') AS fingerprint FROM game.items i WHERE owner_id = $1", &[&corp]).await.unwrap().get("fingerprint");
        assert_eq!(before, after);
        assert!(ceo_rx.try_recv().is_err());
        assert!(observer_rx.try_recv().is_err());
        db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor.character, &ROLE_ACCOUNTANT]).await.unwrap();
        let new_id = success(split_stack(&client, station, deliveries, 3, true, FLAG_CORP_DELIVERIES).await).as_int().unwrap();
        let old = load_items(&db, &client.sde, ItemQuery::Id(deliveries)).await.unwrap().pop().unwrap();
        let new = load_items(&db, &client.sde, ItemQuery::Id(new_id)).await.unwrap().pop().unwrap();
        assert_eq!((old.quantity, new.quantity), (7, 3));
        assert_eq!((new.type_id, new.owner_id, new.location_id, new.flag_id, new.singleton, &new.custom_info), (old.type_id, corp, station, FLAG_CORP_DELIVERIES, false, &old.custom_info));
        let notices: Vec<_> = std::iter::from_fn(|| ceo_rx.try_recv().ok()).collect();
        assert!(notices.iter().any(|(event, args)| event == "OnItemChange" && args[0] == old.row()));
        assert!(notices.iter().any(|(event, args)| event == "OnItemChange" && args[0] == new.row()));
        assert!(observer_rx.try_recv().is_err());
        let personal = transfers::insert(&db, type_id, actor.character, station, FLAG_HANGAR, 5).await.unwrap();
        let private = success(items_in(&client, station, None, false, FLAG_HANGAR).await);
        assert!(private.items().unwrap().contains(&item_row(&db, &client.sde, personal).await));
        assert!(!private.items().unwrap().contains(&item_row(&db, &client.sde, deliveries).await));
        assert!(split_stack(&client, station, deliveries, 1, false, FLAG_HANGAR).await.is_err());
        db.execute("UPDATE game.characters SET corporation_id = 1 WHERE character_id = $1", &[&actor.character]).await.unwrap();
        assert!(success(items_in(&client, station, None, true, FLAG_CORP_DELIVERIES).await).items().unwrap().is_empty());
        assert!(split_stack(&client, station, deliveries, 1, true, FLAG_CORP_DELIVERIES).await.is_err());
        client.world.unregister_pilot(ceo.character);
        client.world.unregister_pilot(observer.character);
    });
    let result = task.await;
    db.execute("DELETE FROM game.offices WHERE corporation_id = $1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id = $1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id = $1", &[&other_corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id = 1 WHERE character_id = $1", &[&other_ceo.character]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id = $1", &[&other_corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id = 1 WHERE character_id = ANY($1)", &[&vec![actor.character, ceo.character, observer.character]]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id = $1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, observer).await.unwrap();
    fixture::cleanup(&db, other_ceo).await.unwrap();
    result.unwrap();
}
