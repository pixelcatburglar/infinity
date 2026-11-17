//! Production dispatch and notification payloads for client consumer checks.
use super::*;
use crate::services::{self, Call, CallError};
use infinity_core::{consts::{BLUEPRINT_COPY_QUANTITY, CATEGORY_BLUEPRINT, FILETIME_DAY, FLAG_CARGO, GROUP_CARGO_CONTAINER, RENTAL_DAYS}, session::OnlineSession};
use marshal::Value;
struct Context { actor: i64, observer: i64, corp: i64, observer_corp: i64, station: i64, observer_station: i64, office: i64, system: i64, observer_system: i64, items: Option<(i64, i64, i64, i64, i64, i32, i32)> }
fn emit(seq: &mut usize, id: &str, symbol: &str, payload: &Value, ctx: &Context, event: Option<&str>, recipient: Option<i64>) {
    let bytes = marshal::dumps(payload);
    use std::fmt::Write as _; let mut hex = String::with_capacity(bytes.len() * 2); for byte in &bytes { write!(&mut hex, "{byte:02x}").unwrap(); }
    let scalar = match event {
        Some("OnOfficeRentalChange") => { let args = payload.items().unwrap(); assert_eq!(args.len(), 2); Some(args.iter().map(|value| {
            serde_json::json!({"type":"int", "value":value.as_int().expect("rental scalar").to_string()})
        }).collect::<Vec<_>>()) },
        Some("OnBillReceived") => { assert!(payload.items().unwrap().is_empty()); Some(Vec::new()) },
        _ => None,
    };
    println!("CLIENT_CORPORATION_ASSETS_INPUT_V1 {}", serde_json::json!({
        "version":1, "id":id, "kind":if event.is_some() {"notification"} else if symbol.ends_with("::error") {"error"} else {"response"},
        "scenario":"corporation_office", "sequence":seq.to_string(),
        "producer_test":"services::offices::tests::client_capture::corporation_assets_client_capture",
        "producer_symbols":[symbol], "payload_scope":if event.is_some() {"notification_args"} else if symbol.ends_with("::error") {"error_response"} else {"ordinary_response"},
        "wire_hex":hex, "wire_size":bytes.len().to_string(), "event":event,
        "recipient":recipient.map(|id| serde_json::json!({"kind":"pilot","id":id.to_string()})),
        "session":{"source":"fixture_context","charid":recipient.unwrap_or(ctx.actor).to_string(),"corpid":(if recipient == Some(ctx.observer) {ctx.observer_corp} else {ctx.corp}).to_string(),"stationid":(if recipient == Some(ctx.observer) {ctx.observer_station} else {ctx.station}).to_string(),"solarsystemid2":(if recipient == Some(ctx.observer) {ctx.observer_system} else {ctx.system}).to_string()},
        "before":{"source":"fixture_context","office_id":ctx.office.to_string(),"assets":ctx.items.map(|(stack, original, copy, nested, job, bp_type, stack_type)| serde_json::json!({"stack":stack.to_string(),"original":original.to_string(),"copy":copy.to_string(),"nested":nested.to_string(),"job":job.to_string(),"blueprint_type":bp_type.to_string(),"stack_type":stack_type.to_string()}))}, "after":{"source":"server_output"}, "scalar_args":scalar,
    }));
    *seq += 1;
}
async fn rpc(c: &mut crate::client::Client, seq: &mut usize, id: &str, service: &str, method: &str, args: &[Value], bind: Option<&Value>, ctx: &Context) -> Value {
    let kwargs = Value::None;
    let call = Call { service, method, args, kwargs: &kwargs, bound: bind.is_some(), bind_params: bind };
    let value = match services::dispatch(c, &call).await {
        Ok(value) => value,
        Err(CallError::User { msg, dict }) => panic!("unexpected user refusal {msg}: {dict:?}"),
        Err(error) => panic!("production dispatch failed: {error:?}"),
    };
    emit(seq, id, &format!("{service}::{method}"), &value, ctx, None, None);
    value
}
fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>, seq: &mut usize, ctx: &Context, who: i64, rental_id: &str, prime: bool) -> (bool, bool, bool) {
    let (mut rental, mut bill, mut primed) = (false, false, false);
    while let Ok((event, args)) = rx.try_recv() {
        let id = match event.as_str() {
            "OnOfficeRentalChange" => { assert_eq!(args, vec![Value::Int(ctx.corp), Value::Int(ctx.office)]); rental = true; rental_id },
            "OnBillReceived" => { bill = true; "bill-received" },
            "OnItemChange" if prime => { assert_eq!(services::item_descriptor().get(&args[0], "itemID"), Some(Value::Int(ctx.office))); assert_eq!(args[1], Value::Dict(vec![])); primed = true; "office-prime-item" },
            "OnCorpAssetChange" => { assert!(!args[0].items().unwrap().is_empty()); "asset-change" },
            _ => continue,
        };
        let symbol = match event.as_str() { "OnOfficeRentalChange" => "offices::notifications::office_rental_notice", "OnBillReceived" => "bills::settlement::pay", "OnItemChange" => "offices::prime_office_item", _ => "offices::notifications::corp_asset_notice" }; emit(seq, id, symbol, &Value::tuple(args), ctx, Some(&event), Some(who));
    }
    (rental, bill, primed)
}
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn corporation_assets_client_capture() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = services::tests::client(db.clone(), 0).await;
    let (actor, corp, station) = office_fixture(&db, &client.world).await; let observer = fixture::create(&db).await.unwrap();
    let observer_corp: i64 = db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&observer.character]).await.unwrap().get("corporation_id"); assert_ne!(observer_corp, corp); let (mut observer_client, _observer_peer) = services::tests::client(db.clone(), observer.character).await;
    observer_client.world = client.world.clone();
    let station_row = client.sde.map.station(station).unwrap(); let system = station_row.solar_system_id.unwrap();
    let landlord = station_row.owner_id.expect("station landlord");
    client.session.apply(vec![("charid", Value::Int(actor.character)), ("userid", Value::Int(actor.account)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system))]);
    observer_client.session.apply(vec![("userid", Value::Int(observer.account)), ("corpid", Value::Int(observer_corp)), ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system))]);
    let (actor_tx, mut actor_rx) = tokio::sync::mpsc::unbounded_channel();
    let (observer_tx, mut observer_rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = actor_tx.clone();
    observer_client.notify_tx = observer_tx.clone();
    client.world.register_pilot(actor.character, actor_tx);
    client.world.register_pilot(observer.character, observer_tx);
    client.world.set_online_session(actor.character, OnlineSession::from_session(&client.session));
    client.world.set_online_session(observer.character, OnlineSession::from_session(&observer_client.session));
    let station_bind = Value::Int(station);
    let initial = Context { actor: client.session.int("charid").unwrap(), observer: observer_client.session.int("charid").unwrap(), corp: client.session.int("corpid").unwrap(), observer_corp: observer_client.session.int("corpid").unwrap(), station: client.session.int("stationid").unwrap(), observer_station: observer_client.session.int("stationid").unwrap(), office: 0, system: client.session.int("solarsystemid2").unwrap(), observer_system: observer_client.session.int("solarsystemid2").unwrap(), items: None };
    let mut seq = 0;
    let quote = rent_price(&client.sde, station);
    rpc(&mut client, &mut seq, "office-rent", "officeManager", "RentOffice", &[Value::Float(quote)], Some(&station_bind), &initial).await;
    let lease = crate::dao::offices::identity(&db, corp, station).await.unwrap().unwrap();
    let office = lease.office_id;
    while actor_rx.try_recv().is_ok() {} while observer_rx.try_recv().is_ok() {}
    let bp_type = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_BLUEPRINT)).unwrap().type_id;
    let can_type = client.sde.types.iter().find(|ty| ty.group_id == Some(GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let stack = crate::dao::items::transfers::insert(&db, can_type, corp, office, FLAG_CORP_SAG1, infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT + 1).await.unwrap();
    let original = crate::dao::items::creation::rookie_ship(&db, bp_type, corp, office, FLAG_CORP_SAG1).await.unwrap();
    let copy = crate::dao::items::transfers::insert(&db, bp_type, corp, office, FLAG_CORP_SAG1, BLUEPRINT_COPY_QUANTITY).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&copy]).await.unwrap();
    crate::dao::industry::set_blueprint(&db, copy, 4, 6, 8).await.unwrap();
    let container = crate::dao::items::creation::rookie_ship(&db, can_type, corp, office, FLAG_CORP_SAG1).await.unwrap();
    let nested = crate::dao::items::creation::rookie_ship(&db, bp_type, corp, container, FLAG_CARGO).await.unwrap();
    let job: i64 = db.query_one("INSERT INTO game.industry_jobs (installer_id,owner_id,activity_id,blueprint_id,blueprint_type_id,blueprint_copy,facility_id,solar_system_id,runs,cost,time_seconds,start_date,end_date,status) VALUES ($1,$2,1,$3,$4,false,$5,$6,1,0,1,0,1,$7) RETURNING job_id", &[&actor.character, &corp, &original, &bp_type, &station, &system, &infinity_core::consts::INDUSTRY_JOB_STATUS_INSTALLED]).await.unwrap().get("job_id");
    let ctx = Context { office, items: Some((stack, original, copy, nested, job, bp_type, can_type)), ..initial };
    let denied = [Value::Int(corp + 1)]; let kwargs = Value::None;
    let call = Call { service: "officeManager", method: "GetPriceQuote", args: &denied, kwargs: &kwargs, bound: true, bind_params: Some(&station_bind) };
    if let Err(CallError::User { msg, dict }) = services::dispatch(&mut client, &call).await { emit(&mut seq, "office-quote-refusal", "officeManager::GetPriceQuote::error", &crate::client::user_error(&msg, dict), &ctx, None, None); } else { panic!("expected quote refusal"); }
    rpc(&mut client, &mut seq, "assets-office-locations", "corpmgr", "GetAssetInventory", &[Value::Int(corp), Value::str("offices")], None, &ctx).await;
    let detail = rpc(&mut client, &mut seq, "assets-office-detail", "corpmgr", "GetAssetInventoryForLocation", &[Value::Int(corp), Value::Int(station), Value::str("offices")], None, &ctx).await; let Value::Reduce { data, .. } = detail else { panic!("expected asset rowset") }; assert_eq!(data.items().unwrap()[1].items().unwrap()[1].items().unwrap().len(), 4);
    rpc(&mut client, &mut seq, "assets-search", "corpmgr", "SearchAssets", &[Value::str("offices"), Value::None, Value::None, Value::Int(i64::from(can_type)), Value::Int(1)], None, &ctx).await;
    let bp_reply = rpc(&mut client, &mut seq, "blueprints-original-copy", "blueprintManager", "GetBlueprintDataByOwner", &[Value::Int(corp), Value::None], None, &ctx).await; assert_eq!(bp_reply.items().unwrap()[0].items().unwrap().len(), 3);
    rpc(&mut client, &mut seq, "blueprints-facility", "blueprintManager", "GetBlueprintDataByOwner", &[Value::Int(corp), Value::Int(station)], None, &ctx).await;
    let before: i64 = db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1", &[&corp]).await.unwrap().get("n");
    rpc(&mut client, &mut seq, "office-prime-reply", "officeManager", "PrimeOfficeItem", &[], Some(&station_bind), &ctx).await;
    assert!(drain(&mut actor_rx, &mut seq, &ctx, actor.character, "office-rental-change-member", true).2);
    let office_bind = Value::tuple([Value::Int(office)]);
    let item_reply = rpc(&mut client, &mut seq, "office-self-item", "invInventory", "GetSelfInvItem", &[], Some(&office_bind), &ctx).await; assert_eq!(services::item_descriptor().get(&item_reply, "itemID"), Some(Value::Int(office)));
    assert_eq!(db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1", &[&corp]).await.unwrap().get::<_, i64>("n"), before);
    let amount = IskAmount::from_isk(quote).unwrap();
    assert!(crate::dao::bills::issue_renewal(&db, corp, landlord, &amount, lease.rented_until, office, station, lease.rented_until, lease.rented_until + RENTAL_DAYS * FILETIME_DAY, crate::client::filetime_now()).await.unwrap());
    let rental = crate::dao::bills::renewal_for(&db, office, corp, lease.rented_until).await.unwrap().unwrap();
    let personal_bill: i64 = db.query_one("INSERT INTO game.bills (bill_type,debtor_id,creditor_id,amount,due,created) VALUES (1,$1,$2,42,0,0) RETURNING bill_id", &[&actor.character, &landlord]).await.unwrap().get("bill_id");
    let personal_row = crate::dao::bills::get(&db, personal_bill).await.unwrap().unwrap();
    let character_wire = crate::services::bills::tests::bill_row_for_capture(&personal_row);
    emit(&mut seq, "bill-route-character", "bills::bill_row", &character_wire, &ctx, None, None);
    db.execute("INSERT INTO game.bills (bill_type,debtor_id,creditor_id,amount,due,created) VALUES (1,$1,$2,50,0,0)", &[&corp, &landlord]).await.unwrap();
    let alliance: i64 = db.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('capture-' || nextval('game.alliances_alliance_id_seq')::text,'C' || currval('game.alliances_alliance_id_seq')::text,$1,$1,$2,0) RETURNING alliance_id", &[&corp, &actor.character]).await.unwrap().get("alliance_id");
    db.execute("UPDATE game.corporations SET alliance_id=$2 WHERE corporation_id=$1", &[&corp, &alliance]).await.unwrap();
    db.execute("INSERT INTO game.bills (bill_type,debtor_id,creditor_id,amount,due,created) VALUES (1,$1,$2,75,0,0)", &[&alliance, &landlord]).await.unwrap();
    client.session.apply(vec![("allianceid", Value::Int(alliance))]);
    crate::dao::bills::set_setting(&db, corp, infinity_core::consts::BILL_TYPE_RENTAL, true).await.unwrap();
    crate::dao::bills::set_setting(&db, alliance, 1, true).await.unwrap();
    let bills = rpc(&mut client, &mut seq, "bill-rental-row", "billMgr", "GetCorporationBills", &[], None, &ctx).await; assert!(bills.items().unwrap().iter().any(|row| services::value_field(row, "billID") == Some(&Value::Int(rental.bill_id))));
    rpc(&mut client, &mut seq, "bill-route-corporation", "billMgr", "GetCorporationBills", &[], None, &ctx).await;
    let alliance_bind = Value::tuple([Value::Int(alliance), Value::Bool(true)]);
    rpc(&mut client, &mut seq, "bill-route-alliance", "allianceRegistry", "GetBills", &[], Some(&alliance_bind), &ctx).await;
    rpc(&mut client, &mut seq, "autopay-settings", "billMgr", "GetAutomaticPaySettings", &[], None, &ctx).await;
    rpc(&mut client, &mut seq, "bill-payment", "billMgr", "PayCorporationBill", &[Value::Int(rental.bill_id), Value::Int(i64::from(ACCOUNT_CASH))], None, &ctx).await;
    assert!(drain(&mut actor_rx, &mut seq, &ctx, actor.character, "office-renewal-member", false).1);
    while observer_rx.try_recv().is_ok() {}
    rpc(&mut client, &mut seq, "office-unrent", "officeManager", "UnrentOffice", &[], Some(&station_bind), &ctx).await;
    assert!(drain(&mut actor_rx, &mut seq, &ctx, actor.character, "office-rental-change-member", false).0);
    assert!(drain(&mut observer_rx, &mut seq, &ctx, observer.character, "office-rental-change-observer", false).0);
    rpc(&mut client, &mut seq, "assets-impounded-detail", "corpmgr", "GetAssetInventoryForLocation", &[Value::Int(corp), Value::Int(station), Value::str("impounded")], None, &ctx).await;
    rpc(&mut client, &mut seq, "blueprints-impounded", "blueprintManager", "GetBlueprintDataByOwner", &[Value::Int(corp), Value::None], None, &ctx).await;
    assert_eq!(crate::dao::items::loading::load_items(&db, ItemQuery::Id(stack)).await.unwrap().len(), 1);
    db.execute("DELETE FROM game.industry_jobs WHERE job_id=$1", &[&job]).await.unwrap();
    db.execute("DELETE FROM game.blueprints WHERE item_id=ANY($1)", &[&vec![original, copy, nested]]).await.unwrap();
    db.execute("DELETE FROM game.bill_autopay WHERE owner_id=$1", &[&alliance]).await.unwrap();
    db.execute("DELETE FROM game.bills WHERE debtor_id=$1", &[&alliance]).await.unwrap();
    db.execute("UPDATE game.corporations SET alliance_id=NULL WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.alliances WHERE alliance_id=$1", &[&alliance]).await.unwrap();
    client.world.unregister_pilot_generation(actor.character, None);
    client.world.unregister_pilot_generation(observer.character, None);
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, observer).await.unwrap();
}
