//! Input boundaries for inventory PLEX conversions.
use super::*;

#[test]
fn quantity_requires_positive_i64_integer() {
    assert_eq!(quantity(None), None);
    assert_eq!(quantity(Some(&Value::Int(0))), None);
    assert_eq!(quantity(Some(&Value::Int(-1))), None);
    assert_eq!(quantity(Some(&Value::Int(i64::from(i32::MAX) + 1))), Some(i64::from(i32::MAX) + 1));
    assert_eq!(quantity(Some(&Value::Bool(true))), None);
    assert_eq!(quantity(Some(&Value::str("1"))), None);
    assert_eq!(quantity(Some(&Value::Int(1))), Some(1));
    assert_eq!(quantity(Some(&Value::Int(i64::MAX))), Some(i64::MAX));
}

use infinity_core::consts::GM_ROLES;
use crate::{client::Client, dao::accounts::plex::tests::{ConversionFixture, ledger_counts, ledger_references, store_purchase_count, cleanup_store}, db::Db, services::Call};

async fn setup(balance: &str) -> (Db, Client, ConversionFixture, tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), 0).await;
    let station = client.sde.map.stations().next().expect("SDE station").station_id;
    let fixture = ConversionFixture::create(&db, station, balance).await.unwrap();
    client.login.user_id = fixture.user;
    client.session.apply(vec![("charid", Value::Int(fixture.character)), ("stationid", Value::Int(station))]);
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = sender;
    (db, client, fixture, receiver)
}

async fn request(client: &mut Client, method: &str, args: &[Value]) -> Result<Value, CallError> {
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "invbroker", method, args, kwargs: &kwargs, bound: false, bind_params: None };
    super::super::inv_broker::broker(client, &call).await.expect("broker route")
}

fn withdrawal(station: i64, quantity: i64, flag: i64) -> Vec<Value> {
    vec![Value::Int(quantity), Value::Int(station), Value::str("VaultToMarket"), Value::Int(flag)]
}

fn deposit(station: i64, item: i64, quantity: i64) -> Vec<Value> {
    vec![Value::Int(station), Value::Int(item), Value::Int(quantity), Value::str("InventoryToVault")]
}

fn assert_no_direct_completion(client: &Client) {
    assert!(!client.outbox.iter().any(|packet| matches!(&packet.destination,
        crate::macho::Address::Broadcast { broadcast_id, .. }
            if matches!(broadcast_id.as_text().as_deref(), Some("OnPlexItemAddedToInventory" | "OnPlexDeposited")))));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn withdrawal_and_partial_full_deposit_conserve_balance_and_notify_sender() {
    let (db, mut client, fixture, mut receiver) = setup("20").await;
    let station = fixture.station;
    let first = request(&mut client, "WithdrawPlexFromVault", &withdrawal(station, 8, i64::from(FLAG_HANGAR))).await.unwrap();
    assert!(matches!(first, Value::Int(_)));
    let (event, args) = receiver.try_recv().unwrap();
    assert_eq!(event, "OnPlexItemAddedToInventory");
    assert_eq!(args.len(), 2);
    assert_eq!(args[1], first);
    let item = args[0].as_int().unwrap();
    let second = request(&mut client, "WithdrawPlexFromVault", &withdrawal(station, 8, i64::from(FLAG_HANGAR))).await.unwrap();
    assert!(matches!(second, Value::Int(_)));
    assert_ne!(first, second);
    let (event, args) = receiver.try_recv().unwrap();
    assert_eq!(event, "OnPlexItemAddedToInventory");
    assert_eq!(args.len(), 2);
    assert_eq!(args[1], second);
    let second_item = args[0].as_int().unwrap();
    assert_ne!(item, second_item);
    assert_eq!(plex::balance(&db, fixture.user).await.unwrap(), "4");
    let partial = request(&mut client, "DepositPlexToVault", &deposit(station, item, 3)).await.unwrap();
    assert!(matches!(partial, Value::Int(_)));
    assert_eq!(receiver.try_recv().unwrap(), ("OnPlexDeposited".into(), vec![partial.clone()]));
    let rows = load_items(&db, &client.sde, ItemQuery::Id(item)).await.unwrap();
    assert_eq!(rows[0].quantity, 5);
    let full = request(&mut client, "DepositPlexToVault", &deposit(station, item, 5)).await.unwrap();
    assert!(matches!(full, Value::Int(_)));
    assert_eq!(receiver.try_recv().unwrap(), ("OnPlexDeposited".into(), vec![full.clone()]));
    assert!(load_items(&db, &client.sde, ItemQuery::Id(item)).await.unwrap().is_empty());
    let last = request(&mut client, "DepositPlexToVault", &deposit(station, second_item, 8)).await.unwrap();
    assert!(matches!(last, Value::Int(_)));
    assert_eq!(receiver.try_recv().unwrap(), ("OnPlexDeposited".into(), vec![last.clone()]));
    assert_eq!(plex::balance(&db, fixture.user).await.unwrap(), "20");
    assert_eq!(ledger_references(&db, fixture.user).await.unwrap(), vec![None, first.as_int(), second.as_int(), partial.as_int(), full.as_int(), last.as_int()]);
    assert!(receiver.try_recv().is_err());
    assert_no_direct_completion(&client);
    fixture.cleanup(&db).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn custody_scope_reservations_and_invalid_input_are_refused() {
    let (db, mut client, mut fixture, mut receiver) = setup("5").await;
    let station = fixture.station;
    let owner = fixture.character;
    let (pilot_tx, mut pilot_rx) = tokio::sync::mpsc::unbounded_channel();
    client.world.register_pilot(owner, pilot_tx);
    let own = fixture.stack(&db, owner, station, FLAG_HANGAR, 4, false).await.unwrap();
    let foreign_owner = fixture.user;
    let wrong_owner = fixture.stack(&db, foreign_owner, station, FLAG_HANGAR, 4, false).await.unwrap();
    let wrong_location = fixture.stack(&db, owner, station + 1, FLAG_HANGAR, 4, false).await.unwrap();
    let wrong_flag = fixture.stack(&db, owner, station, infinity_core::consts::FLAG_CARGO, 4, false).await.unwrap();
    let singleton = fixture.stack(&db, owner, station, FLAG_HANGAR, 1, true).await.unwrap();
    let reserved = fixture.stack(&db, owner, station, FLAG_HANGAR, 4, false).await.unwrap();
    fixture.reserve(&db, reserved, station, FLAG_HANGAR).await.unwrap();
    assert_eq!(trades::reserved(&db, &[reserved]).await.unwrap(), vec![reserved]);
    let dormant = fixture.stack(&db, owner, station, FLAG_HANGAR, 1, true).await.unwrap();
    fixture.dormant(&db, dormant).await.unwrap();
    let other_type = client.sde.types.iter().find(|ty| ty.type_id != TYPE_PLEX).unwrap().type_id;
    let corporation = client.sde.npc_corporations.iter().next().expect("SDE NPC corporation").corporation_id;
    let corp_item = fixture.stack(&db, corporation, station, FLAG_HANGAR, 4, false).await.unwrap();
    let wrong_type = transfers::insert(&db, other_type, owner, station, FLAG_HANGAR, 4).await.unwrap();
    for (label, item) in [("wrong owner", wrong_owner), ("corporation owner", corp_item), ("wrong location", wrong_location), ("wrong flag", wrong_flag), ("singleton", singleton), ("reserved", reserved), ("dormant", dormant), ("wrong type", wrong_type)] {
        let result = request(&mut client, "DepositPlexToVault", &deposit(station, item, 1)).await;
        assert!(result.is_err(), "{label} item {item} unexpectedly deposited");
        if item == reserved {
            assert!(matches!(result, Err(CallError::User { msg, dict }) if msg == messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(messages::trade::TRADE_ITEM_RESERVED))]));
        }
    }
    for qty in [0, -1, 5, i64::from(i32::MAX) + 1] {
        assert!(request(&mut client, "DepositPlexToVault", &deposit(station, own, qty)).await.is_err());
    }
    assert!(request(&mut client, "DepositPlexToVault", &deposit(station + 1, own, 1)).await.is_err());
    assert!(request(&mut client, "WithdrawPlexFromVault", &withdrawal(station, 1, i64::from(infinity_core::consts::FLAG_CARGO))).await.is_err());
    assert!(request(&mut client, "WithdrawPlexFromVault", &withdrawal(station + 1, 1, i64::from(FLAG_HANGAR))).await.is_err());
    assert!(receiver.try_recv().is_err());
    assert!(pilot_rx.try_recv().is_err());
    assert!(client.outbox.is_empty());
    assert_eq!(plex::balance(&db, fixture.user).await.unwrap(), "5");
    assert!(ledger_references(&db, fixture.user).await.unwrap().is_empty());
    assert_eq!(load_items(&db, &client.sde, ItemQuery::Id(corp_item)).await.unwrap()[0].quantity, 4);
    assert_eq!(load_items(&db, &client.sde, ItemQuery::Id(own)).await.unwrap()[0].quantity, 4);
    fixture.cleanup(&db).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn insufficient_and_maximum_balance_refusals_leave_items_and_ledger_unchanged() {
    let (db, mut client, fixture, mut receiver) = setup("0").await;
    assert!(load_items(&db, &client.sde, ItemQuery::Owner(fixture.character)).await.unwrap().is_empty());
    assert!(request(&mut client, "WithdrawPlexFromVault", &withdrawal(fixture.station, 1, i64::from(FLAG_HANGAR))).await.is_err());
    assert!(receiver.try_recv().is_err());
    assert!(ledger_references(&db, fixture.user).await.unwrap().is_empty());
    assert!(load_items(&db, &client.sde, ItemQuery::Owner(fixture.character)).await.unwrap().is_empty());
    assert_eq!(plex::balance(&db, fixture.user).await.unwrap(), "0");
    assert!(client.outbox.is_empty());
    fixture.cleanup(&db).await.unwrap();

    let (db, mut client, mut fixture, mut receiver) = setup("999999999999999999999999999999999999").await;
    let station = fixture.station;
    let owner = fixture.character;
    let item = fixture.stack(&db, owner, station, FLAG_HANGAR, 1, false).await.unwrap();
    let (pilot_tx, mut pilot_rx) = tokio::sync::mpsc::unbounded_channel();
    client.world.register_pilot(owner, pilot_tx);
    assert!(request(&mut client, "DepositPlexToVault", &deposit(station, item, 1)).await.is_err());
    assert!(receiver.try_recv().is_err());
    assert!(pilot_rx.try_recv().is_err());
    assert!(ledger_references(&db, fixture.user).await.unwrap().is_empty());
    assert_eq!(load_items(&db, &client.sde, ItemQuery::Id(item)).await.unwrap()[0].quantity, 1);
    assert_eq!(plex::balance(&db, fixture.user).await.unwrap(), "999999999999999999999999999999999999");
    fixture.cleanup(&db).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn large_withdrawal_and_deposit_conserve_balance() {
    for amount in [1_i64 << 31, 1_i64 << 32, (1_i64 << 53) + 1, i64::MAX] {
        let (db, mut client, fixture, mut receiver) = setup(&amount.to_string()).await;
        let reference = request(&mut client, "WithdrawPlexFromVault", &withdrawal(fixture.station, amount, i64::from(FLAG_HANGAR))).await.unwrap();
        let (event, args) = receiver.try_recv().unwrap();
        assert_eq!(event, "OnPlexItemAddedToInventory");
        assert_eq!(args[1], reference);
        let item = args[0].as_int().unwrap();
        assert_eq!(load_items(&db, &client.sde, ItemQuery::Id(item)).await.unwrap()[0].quantity, amount);
        assert_eq!(plex::balance(&db, fixture.user).await.unwrap(), "0");
        let deposited = request(&mut client, "DepositPlexToVault", &deposit(fixture.station, item, amount)).await.unwrap();
        assert_eq!(receiver.try_recv().unwrap(), ("OnPlexDeposited".into(), vec![deposited.clone()]));
        assert!(load_items(&db, &client.sde, ItemQuery::Id(item)).await.unwrap().is_empty());
        assert_eq!(plex::balance(&db, fixture.user).await.unwrap(), amount.to_string());
        assert_eq!(ledger_references(&db, fixture.user).await.unwrap(), vec![None, reference.as_int(), deposited.as_int()]);
        fixture.cleanup(&db).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn failure_after_vault_debit_rolls_back_and_sends_nothing() {
    let (db, mut client, fixture, mut receiver) = setup("5").await;
    let (pilot_tx, mut pilot_rx) = tokio::sync::mpsc::unbounded_channel();
    client.world.register_pilot(fixture.character, pilot_tx);
    let user = fixture.user;
    let character = fixture.character;
    let station = fixture.station;
    let sender = client.notify_tx.clone();
    let result: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox| {
        let sender = sender.clone();
        Box::pin(async move {
        let item = transfers::insert(tx, TYPE_PLEX, character, station, FLAG_HANGAR, 1).await.map_err(db_err)?;
        let entry = plex_vault::change_in(tx, outbox, user, Some(character), VaultChange::Add(-1), VaultContext { kind: VaultKind::MarketWithdrawal, item_id: Some(item), reference: None }, VaultAudience::Account).await?;
        assert_eq!(entry.unwrap().balance, 4);
        assert_eq!(plex::balance(tx, user).await.map_err(db_err)?, "4");
        outbox.notify_sender(sender, "OnPlexItemAddedToInventory", vec![Value::Int(item), Value::Int(1)]);
        Err(CallError::Internal(anyhow::anyhow!("forced rollback after debit")))
    }) }, |value, outbox| std::future::ready(crate::db::outbox::dispatch_client(value, outbox, &mut client))).await;
    assert!(result.is_err());
    assert!(receiver.try_recv().is_err());
    assert!(pilot_rx.try_recv().is_err());
    assert_eq!(plex::balance(&db, user).await.unwrap(), "5");
    assert!(ledger_references(&db, user).await.unwrap().is_empty());
    assert!(load_items(&db, &client.sde, ItemQuery::Owner(character)).await.unwrap().is_empty());
    fixture.cleanup(&db).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn account_balance_notice_reaches_selected_character_not_actor() {
    let (db, mut client, fixture, mut receiver) = setup("5").await;
    let selected = fixture.another_character(&db).await.unwrap();
    client.session.apply(vec![("charid", Value::Int(selected))]);
    let (pilot_tx, mut pilot_rx) = tokio::sync::mpsc::unbounded_channel();
    client.world.register_pilot(selected, pilot_tx);
    let user = fixture.user;
    let actor = fixture.character;
    db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        plex_vault::change_in(tx, outbox, user, Some(actor), VaultChange::Add(1), VaultContext { kind: VaultKind::GmAdd, item_id: None, reference: None }, VaultAudience::Account).await?.unwrap();
        Ok(())
    }), |value, outbox| std::future::ready(crate::db::outbox::dispatch_client(value, outbox, &mut client))).await.unwrap();
    assert_eq!(pilot_rx.try_recv().unwrap(), ("OnAurumChangeFromVgs".into(), vec![Value::dict([("userid", Value::Int(user)), ("balance", Value::Int(6))])]));
    assert!(pilot_rx.try_recv().is_err());
    assert!(receiver.try_recv().is_err());
    fixture.cleanup(&db).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn unpublished_selected_and_online_actor_receive_clamped_account_notice() {
    let (db, mut client, fixture, mut receiver) = setup("999999999999999999999999999999999998").await;
    let selected = fixture.another_character(&db).await.unwrap();
    client.session.apply(vec![("charid", Value::Int(selected))]);
    let (pilot_tx, mut pilot_rx) = tokio::sync::mpsc::unbounded_channel();
    client.world.register_pilot(fixture.character, pilot_tx);
    let user = fixture.user;
    let actor = fixture.character;
    db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        plex_vault::change_in(tx, outbox, user, Some(actor), VaultChange::Add(1), VaultContext { kind: VaultKind::GmAdd, item_id: None, reference: None }, VaultAudience::UnpublishedCaller { character_id: Some(selected) }).await?.unwrap();
        Ok(())
    }), |value, outbox| std::future::ready(crate::db::outbox::dispatch_client(value, outbox, &mut client))).await.unwrap();
    let notice = Value::dict([("userid", Value::Int(user)), ("balance", Value::Int(infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT))]);
    assert_eq!(pilot_rx.try_recv().unwrap(), ("OnAurumChangeFromVgs".into(), vec![notice.clone()]));
    assert!(pilot_rx.try_recv().is_err());
    assert_eq!(client.outbox.len(), 1);
    let packet = &client.outbox[0];
    assert!(matches!(&packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnAurumChangeFromVgs")));
    let envelope = packet.body[0].items().unwrap();
    assert_eq!(marshal::loads(envelope[1].as_bytes().unwrap()).unwrap(), Value::tuple([Value::Int(1), Value::tuple([notice])]));
    assert!(receiver.try_recv().is_err());
    fixture.cleanup(&db).await.unwrap();
}
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn mixed_vault_writers_serialize_real_rpc_paths() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut probe, _peer) = crate::services::tests::client(db.clone(), 0).await;
    let station = probe.sde.map.stations().next().expect("SDE station").station_id;
    let kwargs = Value::Dict(vec![]);
    let offers = crate::services::dispatch(&mut probe, &Call {
        service: "storeManager", method: "get_offers", args: &[], kwargs: &kwargs, bound: false, bind_params: None,
    }).await.unwrap();
    let offer = offers.items().unwrap().iter().find(|row| {
        row.get("canPurchase") == Some(&Value::Bool(true))
            && row.get("periodPurchase").is_none()
            && row.get("offerPricings").and_then(|v| v.items()).and_then(|p| p.first())
                .and_then(|p| p.get("price")).and_then(Value::as_int).is_some_and(|price| price <= 1_000_000)
    }).expect("saleable store offer");
    let offer_id = offer.get("id").and_then(Value::as_int).expect("offer id");
    let offer_price = offer.get("offerPricings").and_then(|v| v.items()).and_then(|p| p.first())
        .and_then(|p| p.get("price")).and_then(Value::as_int).expect("offer price");

    let mut fixture = ConversionFixture::create(&db, station, "1000004").await.unwrap();
    let item = fixture.stack(&db, fixture.character, station, FLAG_HANGAR, 6, false).await.unwrap();
    let (mut deposit_client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let (mut withdraw_client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let (mut gm_client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let (mut store_client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    for client in [&mut deposit_client, &mut withdraw_client, &mut gm_client, &mut store_client] {
        client.login.user_id = fixture.user;
        client.session.apply(vec![("stationid", Value::Int(station))]);
    }
    gm_client.login.role = GM_ROLES;
    let deposit_args = deposit(station, item, 6);
    let withdraw_args = withdrawal(station, 4, i64::from(FLAG_HANGAR));
    let gm_args = [Value::str("/addplex 7")];
    let store_args = [Value::Int(offer_id), Value::str("PLX"), Value::Int(1)];
    let deposit_call = Call { service: "invbroker", method: "DepositPlexToVault", args: &deposit_args, kwargs: &kwargs, bound: false, bind_params: None };
    let withdraw_call = Call { service: "invbroker", method: "WithdrawPlexFromVault", args: &withdraw_args, kwargs: &kwargs, bound: false, bind_params: None };
    let gm_call = Call { service: "slash", method: "SlashCmd", args: &gm_args, kwargs: &kwargs, bound: false, bind_params: None };
    let store_call = Call { service: "storeManager", method: "buy_offer", args: &store_args, kwargs: &kwargs, bound: false, bind_params: None };
    let (deposit_result, withdraw_result, gm_result, store_result) = tokio::join!(
        crate::services::dispatch(&mut deposit_client, &deposit_call),
        crate::services::dispatch(&mut withdraw_client, &withdraw_call),
        crate::services::dispatch(&mut gm_client, &gm_call),
        crate::services::dispatch(&mut store_client, &store_call),
    );
    assert!(deposit_result.is_ok(), "deposit RPC failed: {deposit_result:?}");
    assert!(withdraw_result.is_ok(), "withdraw RPC failed: {withdraw_result:?}");
    assert!(gm_result.is_ok(), "GM RPC failed: {gm_result:?}");
    assert_eq!(store_result.unwrap(), Value::Bool(true));
    assert_eq!(plex::balance(&db, fixture.user).await.unwrap(), (1_000_004 + 6 - 4 + 7 - offer_price).to_string());
    assert!(load_items(&db, &probe.sde, ItemQuery::Id(item)).await.unwrap().is_empty());
    let withdrawn = load_items(&db, &probe.sde, ItemQuery::Owner(fixture.character)).await.unwrap();
    assert_eq!(withdrawn.iter().map(|row| i128::from(row.quantity)).sum::<i128>(), 4);
    assert_eq!(ledger_counts(&db, fixture.user).await, (5, 1));
    let references = ledger_references(&db, fixture.user).await.unwrap();
    assert_eq!(references.iter().filter(|reference| reference.is_some()).count(), 2);
    assert_eq!(references.iter().filter(|reference| reference.is_none()).count(), 3);
    assert_ne!(references.iter().find_map(|reference| *reference), references.iter().rev().find_map(|reference| *reference));
    assert_eq!(store_purchase_count(&db, fixture.user, offer_id as i32).await.unwrap(), 1);
    assert_eq!(crate::dao::store::purchases::tokens(&db, fixture.user).await.unwrap().len(), 1);
    cleanup_store(&db, fixture.user).await.unwrap();
    fixture.cleanup(&db).await.unwrap();
}
