pub(crate) mod fixture;

use super::*;
use marshal::Value;
use crate::services::{Call, CallError};

async fn rpc(client: &mut Client, host: &Arc<reload::Host>, method: &str, args: &[Value]) -> Result<Value, CallError> {
    let kwargs = Value::None;
    let call = Call { service: "shipAdaptionMgr", method, args, kwargs: &kwargs, bound: false, bind_params: None };
    dispatch_host(client, &call, host).await.expect("claimed adaptation method")
}
fn chip(type_id: i32, quantity: u32) -> Value {
    Value::instance("utillib.KeyVal", [("type_id", Value::Int(i64::from(type_id))), ("quantity", Value::Int(i64::from(quantity)))])
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn injector_stack_credits_distinct_attribute_currency() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), character.character).await;
    client.login.user_id = character.account;
    let fixture = fixture::create(&db, client.sde.clone(), client.world.clone()).await;
    client.session.apply(vec![("stationid", Value::Int(fixture.station))]);
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&character.character, &fixture.station]).await.unwrap();
    let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = sender.clone();
    client.world.register_pilot(character.character, sender);
    let bulk = crate::dao::items::transfers::insert(&db, fixture.injector2, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, 4).await.unwrap();
    rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(bulk), Value::Int(3)]).await.unwrap();
    let (event, item_args) = notices.recv().await.unwrap();
    assert_eq!(event, "OnItemChange");
    assert!(matches!(&item_args[1], Value::Dict(changes) if changes.contains(&(Value::Int(infinity_core::consts::IX_STACK_SIZE), Value::Int(4)))));
    let (event, args) = notices.recv().await.unwrap();
    assert_eq!(event, "OnUserResponsed");
    let expected = [
        crate::lua::content::protocol::Chip { type_id: fixture.chip, quantity: 0 },
        crate::lua::content::protocol::Chip { type_id: fixture.chip2, quantity: 30 },
        crate::lua::content::protocol::Chip { type_id: fixture.chip_unquoted, quantity: 0 },
    ];
    let identity = crate::lua::content::protocol::Identity { user: character.account, character: character.character };
    let text = crate::lua::content::protocol::encode_content_text(crate::lua::content::protocol::Response::User { identity, chips: &expected }).unwrap();
    assert_eq!(args[0].as_bytes().unwrap(), text.as_bytes());
    assert_eq!(crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Id(bulk)).await.unwrap()[0].quantity, 1);
    let capital = crate::dao::items::transfers::insert(&db, fixture.injector, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(capital), Value::Int(1)]).await.unwrap();
    assert_eq!(notices.recv().await.unwrap().0, "OnItemChange");
    assert_eq!(notices.recv().await.unwrap().0, "OnUserResponsed");
    let snapshot = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert_eq!(snapshot.chips.iter().map(|row| (row.type_id, row.balance)).collect::<Vec<_>>(), vec![(fixture.chip, 1), (fixture.chip2, 30)]);
    assert_ne!(fixture.injector, fixture.chip);
    assert_ne!(fixture.injector2, fixture.chip2);
    let lp = crate::dao::wallets::loyalty::balance(&db, character.character, fixture.issuer).await.unwrap();
    let overflow = crate::dao::items::transfers::insert(&db, fixture.injector2, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, i64::from(u32::MAX) + 1).await.unwrap();
    client.session.apply(vec![("stationid", Value::None)]);
    assert!(rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(overflow), Value::Int(1)]).await.is_err());
    assert!(notices.try_recv().is_err());
    assert_eq!(crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap(), snapshot);
    client.session.apply(vec![("stationid", Value::Int(fixture.station))]);
    assert!(rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(overflow), Value::Int(i64::from(u32::MAX) + 1)]).await.is_err());
    assert!(notices.try_recv().is_err());
    assert_eq!(crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap(), snapshot);
    assert_eq!(crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Id(overflow)).await.unwrap()[0].quantity, i64::from(u32::MAX) + 1);
    assert_eq!(crate::dao::wallets::loyalty::balance(&db, character.character, fixture.issuer).await.unwrap(), lp);
    db.execute("DELETE FROM game.ship_adaption_chips WHERE character_id=$1", &[&character.character]).await.unwrap();
    db.execute("DELETE FROM game.ship_adaption_characters WHERE character_id=$1", &[&character.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, character).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn separate_package_admission_rejects_foreign_prepared_plan() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = crate::services::tests::fixture::create(&db).await.unwrap();
    let (client, _peer) = crate::services::tests::client(db.clone(), character.character).await;
    let a = fixture::create(&db, client.sde.clone(), client.world.clone()).await;
    let b = fixture::create(&db, client.sde.clone(), client.world.clone()).await;
    let item = crate::dao::items::transfers::insert(&db, a.injector, character.character, a.station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let pin_a = content::PinnedPolicy::pin(a.host.snapshot(), "ship_adaption").unwrap();
    let pin_b = content::PinnedPolicy::pin(b.host.snapshot(), "ship_adaption").unwrap();
    let plan = fixture::inject_plan(&pin_a, &a.sde, a.injector, 1);
    let before = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    let lp = crate::dao::wallets::loyalty::balance(&db, character.character, a.issuer).await.unwrap();
    let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
    client.world.register_pilot(character.character, sender);
    assert!(plan.admit(Arc::new(b.host.admit(&pin_b).unwrap())).is_err());
    assert_eq!(crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap(), before);
    assert_eq!(crate::dao::wallets::loyalty::balance(&db, character.character, a.issuer).await.unwrap(), lp);
    assert_eq!(crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Id(item)).await.unwrap()[0].quantity, 1);
    assert!(notices.try_recv().is_err());
    crate::services::tests::fixture::cleanup(&db, character).await.unwrap();
}


#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn real_script_reads_and_unquoted_injection() {
    use crate::lua::content::protocol::{self, Chip, Identity, Response};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), character.character).await;
    client.login.user_id = character.account;
    let fixture = fixture::create(&db, client.sde.clone(), client.world.clone()).await;
    client.session.apply(vec![("stationid", Value::Int(fixture.station))]);
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&character.character, &fixture.station]).await.unwrap();
    let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = sender.clone();
    client.world.register_pilot(character.character, sender);
    let identity = || Identity { user: character.account, character: character.character };
    let zeros = [fixture.chip, fixture.chip2, fixture.chip_unquoted].map(|type_id| Chip { type_id, quantity: 0 });
    for (method, args, expected) in [
        ("fetch_user_data", vec![], protocol::encode_content_text(Response::User { identity: identity(), chips: &zeros }).unwrap()),
        ("fetch_tree_data", vec![Value::Int(1)], protocol::encode_content_text(Response::Tree { identity: identity(), tree: 1 }).unwrap()),
        ("fetch_missions", vec![], protocol::encode_content_text(Response::Mission { identity: identity() }).unwrap()),
    ] {
        assert_eq!(rpc(&mut client, &fixture.host, method, &args).await.unwrap(), Value::None);
        let packet = client.outbox.pop().unwrap();
        let envelope = packet.body[0].items().unwrap();
        let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
        assert_eq!(payload.items().unwrap()[1].items().unwrap()[0].as_bytes().unwrap(), expected.as_bytes());
    }
    assert!(rpc(&mut client, &fixture.host, "fetch_tree_data", &[Value::Int(2)]).await.is_err());
    assert!(client.outbox.is_empty());
    assert!(crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap().subject.is_none());
    let item = crate::dao::items::transfers::insert(&db, fixture.chip_unquoted, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    assert!(rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(item), Value::Int(1)]).await.is_err());
    assert!(notices.try_recv().is_err());
    let injector = crate::dao::items::transfers::insert(&db, fixture.injector, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(injector), Value::Int(1)]).await.unwrap();
    assert_eq!(notices.recv().await.unwrap().0, "OnItemChange");
    assert_eq!(notices.recv().await.unwrap().0, "OnUserResponsed");
    db.execute("INSERT INTO game.ship_adaption_chips (character_id,type_id,balance) VALUES ($1,$2,1)", &[&character.character, &fixture.chip_unquoted]).await.unwrap();
    let before = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert_eq!(before.chips.iter().find(|row| row.type_id == fixture.chip_unquoted).unwrap().balance, 1);
    let lp = crate::dao::wallets::loyalty::balance(&db, character.character, fixture.issuer).await.unwrap();
    assert!(rpc(&mut client, &fixture.host, "call_chip_exchange", &[Value::Int(fixture.issuer), Value::List(vec![chip(fixture.chip_unquoted, 1)])]).await.is_err());
    assert!(notices.try_recv().is_err());
    assert_eq!(crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap(), before);
    assert_eq!(crate::dao::wallets::loyalty::balance(&db, character.character, fixture.issuer).await.unwrap(), lp);
    assert_eq!(crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Id(item)).await.unwrap()[0].quantity, 1);
    db.execute("DELETE FROM game.ship_adaption_chips WHERE character_id=$1", &[&character.character]).await.unwrap();
    db.execute("DELETE FROM game.ship_adaption_characters WHERE character_id=$1", &[&character.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, character).await.unwrap();
}
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn repository_script_inject_exchange_and_retained_read() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), character.character).await;
    client.login.user_id = character.account;
    let fixture = fixture::create(&db, client.sde.clone(), client.world.clone()).await;
    client.session.apply(vec![("stationid", Value::Int(fixture.station))]);
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&character.character, &fixture.station]).await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = tx.clone();
    client.world.register_pilot(character.character, tx);
    let registry = fixture.host.snapshot();
    for method in ["fetch_tree_data", "fetch_missions", "fetch_user_data", "InjectChips", "call_chip_exchange"] {
        assert!(registry.claims.rpc.contains_key(&("shipAdaptionMgr".into(), method.into())));
    }
    let item = crate::dao::items::transfers::insert(&db, fixture.injector, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, 5).await.unwrap();
    assert_eq!(rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(item), Value::Int(2)]).await.unwrap(), Value::None);
    let (event, item_args) = rx.recv().await.unwrap();
    assert_eq!(event, "OnItemChange");
    assert!(matches!(&item_args[1], Value::Dict(changes) if changes.contains(&(Value::Int(infinity_core::consts::IX_STACK_SIZE), Value::Int(5)))));
    let (event, args) = rx.recv().await.unwrap();
    assert_eq!(event, "OnUserResponsed");
    let desc = crate::gateway::message::pool().get_message_by_name("eve.shipadaption.user.Responsed").unwrap();
    let text = std::str::from_utf8(args[0].as_bytes().unwrap()).unwrap();
    let parsed = prost_reflect::DynamicMessage::parse_text_format(desc, text).unwrap();
    let values = parsed.get_field_by_name("chips").unwrap();
    let prost_reflect::Value::List(rows) = values.as_ref() else { panic!("expected chips") };
    assert_eq!(rows.len(), 3);
    let prost_reflect::Value::Message(zero) = &rows[1] else { panic!("expected zero chip") };
    assert_eq!(zero.get_field_by_name("quantity").unwrap().as_ref(), &prost_reflect::Value::U32(0));
    let snapshot = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert_eq!(snapshot.revision(), 1);
    assert_eq!(snapshot.chips[0].balance, 2);
    for revision in ["true", "1.0", "'1'"] {
        let source = format!("local ctx=...; ctx:content_snapshot('ship_adaption'); return ctx:content_commit('ship_adaption', {revision})");
        let function = registry.lua.load(&source).into_function().unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        let invocation = vm::budgeted_invocation(&registry.lua, function, &fixture.host.config, deadline.into_std()).unwrap();
        let pin = content::PinnedPolicy::pin(registry.clone(), "ship_adaption").unwrap();
        let identity = crate::services::identity::Identity::of_client(&client);
        let metadata = host::client::AdaptionInvocation {
            host: &fixture.host, pin, identity, user: character.account,
            request: crate::services::ship_adaption::model::Request::Inject { item, quantity: 1 },
        };
        let result = host::client::invoke(&mut client, &registry.lua, invocation, vec![], deadline, None, Some(metadata)).await;
        assert!(matches!(result, Err(CallError::User { .. })));
    }
    assert!(rx.try_recv().is_err());
    assert!(rpc(&mut client, &fixture.host, "call_chip_exchange", &[Value::Int(fixture.issuer), Value::List(vec![chip(fixture.chip, 3)])]).await.is_err());
    assert!(rx.try_recv().is_err());
    assert_eq!(crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap().revision(), 1);
    assert!(rpc(&mut client, &fixture.host, "call_chip_exchange", &[Value::Int(fixture.issuer), Value::List(vec![chip(fixture.chip, 1), chip(fixture.chip2, 1)])]).await.is_err());
    assert!(rx.try_recv().is_err());
    let rolled_back = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert_eq!((rolled_back.revision(), rolled_back.chips[0].balance), (1, 2));
    assert_eq!(rpc(&mut client, &fixture.host, "call_chip_exchange", &[Value::Int(fixture.issuer), Value::List(vec![chip(fixture.chip, 2)])]).await.unwrap(), Value::None);
    assert_eq!(rx.recv().await.unwrap().0, "OnLPChange");
    assert_eq!(rx.recv().await.unwrap().0, "OnUserResponsed");
    assert_eq!(rx.recv().await.unwrap().0, "OnChipExchangeSuccessed");
    let snapshot = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert_eq!(snapshot.revision(), 2);
    assert_eq!(snapshot.chips[0].balance, 0);
    assert_eq!(crate::dao::wallets::loyalty::balance(&db, character.character, fixture.issuer).await.unwrap(), Some(4));
    let facts = db.query_one("SELECT count(*) AS count FROM game.progress_events WHERE subject_character_id=$1 AND kind='LoyaltyPointsEarned'", &[&character.character]).await.unwrap();
    assert_eq!(facts.get::<_, i64>("count"), 0);
    assert_eq!(rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(item), Value::Int(3)]).await.unwrap(), Value::None);
    let (event, item_args) = rx.recv().await.unwrap();
    assert_eq!(event, "OnItemChange");
    assert!(matches!(&item_args[1], Value::Dict(changes) if changes.contains(&(Value::Int(infinity_core::consts::IX_LOCATION_ID), Value::Int(fixture.station)))));
    assert_eq!(rx.recv().await.unwrap().0, "OnUserResponsed");
    assert!(crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Id(item)).await.unwrap().is_empty());
    let guarded = crate::dao::items::transfers::insert(&db, fixture.injector, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let container = crate::dao::items::transfers::insert(&db, fixture.chip2, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let other_station = fixture.sde.map.stations().find(|station| station.station_id != fixture.station).expect("test SDE must contain a second station").station_id;
    for persisted_station in [fixture.station, other_station] {
        db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&character.character, &persisted_station]).await.unwrap();
        db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&guarded, &other_station]).await.unwrap();
        let before_item = crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Id(guarded)).await.unwrap();
        let before = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
        let lp = crate::dao::wallets::loyalty::balance(&db, character.character, fixture.issuer).await.unwrap();
        assert!(rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(guarded), Value::Int(1)]).await.is_err());
        assert!(rx.try_recv().is_err());
        assert_eq!(crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap(), before);
        assert_eq!(crate::dao::wallets::loyalty::balance(&db, character.character, fixture.issuer).await.unwrap(), lp);
        let after_item = crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Id(guarded)).await.unwrap();
        assert_eq!(after_item.len(), 1);
        let (a, b) = (&before_item[0], &after_item[0]);
        assert_eq!((a.item_id, a.type_id, a.owner_id, a.location_id, a.flag_id, a.quantity, a.singleton, &a.custom_info),
                   (b.item_id, b.type_id, b.owner_id, b.location_id, b.flag_id, b.quantity, b.singleton, &b.custom_info));
    }
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&character.character, &fixture.station]).await.unwrap();
    db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&guarded, &fixture.station]).await.unwrap();
    for (owner, location, flag, singleton) in [
        (character.character + 1, fixture.station, infinity_core::consts::FLAG_HANGAR, false),
        (fixture.issuer, fixture.station, infinity_core::consts::FLAG_HANGAR, false),
        (character.character, container, infinity_core::consts::FLAG_HANGAR, false),
        (character.character, character.character, infinity_core::consts::FLAG_HANGAR, false),
        (character.character, fixture.station, 0, false),
        (character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, true),
    ] {
        db.execute("UPDATE game.items SET owner_id=$2, location_id=$3, flag_id=$4, singleton=$5 WHERE item_id=$1", &[&guarded, &owner, &location, &flag, &singleton]).await.unwrap();
        assert!(rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(guarded), Value::Int(1)]).await.is_err());
        assert!(rx.try_recv().is_err());
        assert_eq!(crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap().revision(), 3);
    }
    db.execute("UPDATE game.items SET owner_id=$2, location_id=$3, flag_id=$4, singleton=false WHERE item_id=$1", &[&guarded, &character.character, &fixture.station, &infinity_core::consts::FLAG_HANGAR]).await.unwrap();
    let trade = db.query_one("INSERT INTO game.trades (station_id,char_a,char_b,state,created_at,updated_at) VALUES ($1,$2,$3,'open',0,0) RETURNING trade_id", &[&fixture.station, &character.character, &(character.character + 1)]).await.unwrap().get::<_, i64>("trade_id");
    db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&guarded, &trade]).await.unwrap();
    assert!(rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(guarded), Value::Int(1)]).await.is_err());
    assert!(rx.try_recv().is_err());
    db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&guarded, &fixture.station]).await.unwrap();
    db.execute("DELETE FROM game.trades WHERE trade_id=$1", &[&trade]).await.unwrap();
    let clone = db.query_one("INSERT INTO game.jump_clones (owner_id,location_id) VALUES ($1,$2) RETURNING clone_id", &[&character.character, &fixture.station]).await.unwrap().get::<_, i64>("clone_id");
    db.execute("UPDATE game.items SET location_id=$2, flag_id=89, singleton=true, brain_slot=1, jump_clone_id=$3 WHERE item_id=$1", &[&guarded, &character.character, &clone]).await.unwrap();
    assert!(rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(guarded), Value::Int(1)]).await.is_err());
    assert!(rx.try_recv().is_err());
    db.execute("UPDATE game.items SET location_id=$2, flag_id=$3, singleton=false, brain_slot=NULL, jump_clone_id=NULL WHERE item_id=$1", &[&guarded, &fixture.station, &infinity_core::consts::FLAG_HANGAR]).await.unwrap();
    db.execute("DELETE FROM game.jump_clones WHERE clone_id=$1", &[&clone]).await.unwrap();
    db.execute("UPDATE game.ship_adaption_chips SET balance=$3 WHERE character_id=$1 AND type_id=$2", &[&character.character, &fixture.chip, &i64::from(u32::MAX)]).await.unwrap();
    let before_overflow = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert_eq!(before_overflow.chips.iter().find(|row| row.type_id == fixture.chip).unwrap().balance, u32::MAX);
    let lp_before_overflow = crate::dao::wallets::loyalty::balance(&db, character.character, fixture.issuer).await.unwrap();
    assert!(rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(guarded), Value::Int(1)]).await.is_err());
    assert!(rx.try_recv().is_err());
    assert_eq!(crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap(), before_overflow);
    assert_eq!(crate::dao::wallets::loyalty::balance(&db, character.character, fixture.issuer).await.unwrap(), lp_before_overflow);
    assert_eq!(crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Id(guarded)).await.unwrap()[0].quantity, 1);
    db.execute("UPDATE game.ship_adaption_chips SET balance=3 WHERE character_id=$1 AND type_id=$2", &[&character.character, &fixture.chip]).await.unwrap();
    db.execute("UPDATE game.ship_adaption_characters SET revision=$2 WHERE character_id=$1", &[&character.character, &i64::MAX]).await.unwrap();
    assert!(rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(guarded), Value::Int(1)]).await.is_err());
    assert!(rx.try_recv().is_err());
    let blocked = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert_eq!((blocked.revision(), blocked.chips[0].balance), (i64::MAX, 3));
    assert_eq!(crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Id(guarded)).await.unwrap()[0].quantity, 1);
    db.execute("UPDATE game.ship_adaption_characters SET revision=3 WHERE character_id=$1", &[&character.character]).await.unwrap();
    db.execute("UPDATE game.lp_balances SET amount=$3 WHERE char_id=$1 AND corp_id=$2", &[&character.character, &fixture.issuer, &i64::MAX]).await.unwrap();
    assert!(rpc(&mut client, &fixture.host, "call_chip_exchange", &[Value::Int(fixture.issuer), Value::List(vec![chip(fixture.chip, 1)])]).await.is_err());
    assert!(rx.try_recv().is_err());
    let unchanged = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert_eq!((unchanged.revision(), unchanged.chips[0].balance), (3, 3));
    let old_pin = content::PinnedPolicy::pin(registry.clone(), "ship_adaption").unwrap();
    let old_plan = fixture::inject_plan(&old_pin, &fixture.sde, fixture.injector, 1);
    let admitted = fixture.host.admit(&old_pin).unwrap();
    fixture.host.reload().await.unwrap();
    let new_pin = content::PinnedPolicy::pin(fixture.host.snapshot(), "ship_adaption").unwrap();
    let mismatched_plan = fixture::inject_plan(&old_pin, &fixture.sde, fixture.injector, 1);
    assert!(matches!(mismatched_plan.admit(Arc::new(fixture.host.admit(&new_pin).unwrap())), Err(CallError::User { .. })));
    let forged = crate::services::ship_adaption::policy::Input {
        definitions: Arc::new((**new_pin.adaption().unwrap()).clone()), item_type: None, quantity: 0, credit: None,
        issuer: fixture.issuer, chips: Arc::new(vec![(fixture.chip, 1)]),
    };
    assert!(crate::services::ship_adaption::policy::plan(&new_pin, forged, std::time::Instant::now() + std::time::Duration::from_secs(30)).is_err());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let handler = &registry.claims.rpc[&("shipAdaptionMgr".into(), "InjectChips".into())];
    let invocation = vm::budgeted_invocation(&registry.lua, handler.function.clone(), &fixture.host.config, deadline.into_std()).unwrap();
    let identity = crate::services::identity::Identity::of_client(&client);
    let metadata = host::client::AdaptionInvocation {
        host: &fixture.host, pin: old_pin, identity, user: character.account,
        request: crate::services::ship_adaption::model::Request::Inject { item: guarded, quantity: 1 },
    };
    let result = host::client::invoke(&mut client, &registry.lua, invocation, vec![], deadline, None, Some(metadata)).await;
    assert!(matches!(result, Err(CallError::User { .. })));
    assert_eq!(admitted.policy().version(), unchanged.subject.as_ref().unwrap().content_version);
    assert!(rx.try_recv().is_err());
    assert_eq!(crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap().revision(), 3);
    {
        let _market = crate::services::market::lock().await;
        crate::services::ship_adaption::operations::commit(
            &db, &client.world, &client.notify_tx, client.sde.clone(), identity, character.account,
            3, Arc::new(old_plan.admit(Arc::new(admitted)).unwrap().unwrap()), Some(guarded),
        ).await.unwrap();
    }
    assert_eq!(rx.recv().await.unwrap().0, "OnItemChange");
    assert_eq!(rx.recv().await.unwrap().0, "OnUserResponsed");
    let kwargs = Value::None;
    let call = Call { service: "shipAdaptionMgr", method: "fetch_user_data", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
    assert_eq!(client.outbox.len(), 1);
    let packet = client.outbox.pop().unwrap();
    let envelope = packet.body[0].items().unwrap();
    let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
    let text = payload.items().unwrap()[1].items().unwrap()[0].as_bytes().unwrap();
    let desc = crate::gateway::message::pool().get_message_by_name("eve.shipadaption.user.Responsed").unwrap();
    let decoded = prost_reflect::DynamicMessage::parse_text_format(desc, std::str::from_utf8(text).unwrap()).unwrap();
    let values = decoded.get_field_by_name("chips").unwrap();
    let prost_reflect::Value::List(rows) = values.as_ref() else { panic!("expected chips") };
    let prost_reflect::Value::Message(row) = &rows[0] else { panic!("expected chip") };
    assert_eq!(row.get_field_by_name("quantity").unwrap().as_ref(), &prost_reflect::Value::U32(4));
    db.execute("INSERT INTO game.ship_adaption_chips (character_id,type_id,balance) VALUES ($1,$2,0)", &[&character.character, &i32::MAX]).await.unwrap();
    assert!(matches!(rpc(&mut client, &fixture.host, "fetch_user_data", &[]).await, Err(CallError::Internal(_))));
    assert!(client.outbox.is_empty());
    db.execute("DELETE FROM game.ship_adaption_chips WHERE character_id=$1", &[&character.character]).await.unwrap();
    db.execute("DELETE FROM game.ship_adaption_characters WHERE character_id=$1", &[&character.character]).await.unwrap();
    db.execute("DELETE FROM game.lp_balances WHERE char_id=$1", &[&character.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, character).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn dropped_waiter_does_not_cancel_admitted_commit() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), character.character).await;
    client.login.user_id = character.account;
    let fixture = fixture::create(&db, client.sde.clone(), client.world.clone()).await;
    client.session.apply(vec![("stationid", Value::Int(fixture.station))]);
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&character.character, &fixture.station]).await.unwrap();
    let item = crate::dao::items::transfers::insert(&db, fixture.injector, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = sender.clone();
    client.world.register_pilot(character.character, sender);
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let release = Arc::new(tokio::sync::Notify::new());
    let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
    let pid_tx = Arc::new(std::sync::Mutex::new(Some(pid_tx)));
    let lock_db = db.clone();
    let lock_barrier = barrier.clone();
    let lock_release = release.clone();
    let locked = tokio::spawn(async move {
        lock_db.transaction(|tx, _: &mut ()| {
            let barrier = lock_barrier.clone();
            let release = lock_release.clone();
            let pid_tx = pid_tx.clone();
            Box::pin(async move {
                crate::dao::characters::selection::lock_character(tx, character.character).await?;
                let pid = crate::dao::items::parent_locks::tests::backend_pid(tx).await;
                if let Some(sender) = pid_tx.lock().unwrap().take() { let _ = sender.send(pid); }
                barrier.wait().await;
                release.notified().await;
                Ok::<_, crate::db::Error>(())
            })
        }, |(), ()| async { Ok(()) }).await.unwrap();
    });
    barrier.wait().await;
    let host = fixture.host.clone();
    let caller = tokio::spawn(async move {
        rpc(&mut client, &host, "InjectChips", &[Value::Int(item), Value::Int(1)]).await
    });
    crate::dao::items::parent_locks::tests::wait_blocked(&db, pid_rx.await.unwrap()).await;
    caller.abort();
    release.notify_one();
    locked.await.unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(20), notices.recv()).await.unwrap().unwrap();
    let second = tokio::time::timeout(std::time::Duration::from_secs(20), notices.recv()).await.unwrap().unwrap();
    assert_eq!((first.0.as_str(), second.0.as_str()), ("OnItemChange", "OnUserResponsed"));
    let snapshot = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert_eq!((snapshot.revision(), snapshot.chips[0].balance), (1, 1));
    db.execute("DELETE FROM game.ship_adaption_chips WHERE character_id=$1", &[&character.character]).await.unwrap();
    db.execute("DELETE FROM game.ship_adaption_characters WHERE character_id=$1", &[&character.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, character).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn concurrent_prepared_revisions_serialize_and_one_rolls_back() {
    use crate::services::ship_adaption::operations;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), character.character).await;
    client.login.user_id = character.account;
    let fixture = fixture::create(&db, client.sde.clone(), client.world.clone()).await;
    client.session.apply(vec![("stationid", Value::Int(fixture.station))]);
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&character.character, &fixture.station]).await.unwrap();
    let first = crate::dao::items::transfers::insert(&db, fixture.injector, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let second = crate::dao::items::transfers::insert(&db, fixture.injector, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
    client.world.register_pilot(character.character, sender.clone());
    let identity = crate::services::identity::Identity::of_client(&client);
    let registry = fixture.host.snapshot();
    let first_pin = content::PinnedPolicy::pin(registry.clone(), "ship_adaption").unwrap();
    let second_pin = content::PinnedPolicy::pin(registry, "ship_adaption").unwrap();
    let first_plan = fixture::inject_plan(&first_pin, &fixture.sde, fixture.injector, 1);
    let second_plan = fixture::inject_plan(&second_pin, &fixture.sde, fixture.injector, 1);
    let apply = |pin: content::PinnedPolicy, plan: crate::services::ship_adaption::policy::PreparedPlan, item| {
        let db = db.clone();
        let world = client.world.clone();
        let sde = client.sde.clone();
        let sender = sender.clone();
        let host = fixture.host.clone();
        async move {
            let _market = crate::services::market::lock().await;
            let admitted = host.admit(&pin).unwrap();
            operations::commit(&db, &world, &sender, sde, identity, character.account, 0, Arc::new(plan.admit(Arc::new(admitted)).unwrap().unwrap()), Some(item)).await
        }
    };
    let (a, b) = tokio::join!(apply(first_pin, first_plan, first), apply(second_pin, second_plan, second));
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(matches!(a, Err(CallError::User { .. })) || matches!(b, Err(CallError::User { .. })));
    assert_eq!(notices.recv().await.unwrap().0, "OnItemChange");
    assert_eq!(notices.recv().await.unwrap().0, "OnUserResponsed");
    assert!(notices.try_recv().is_err());
    let snapshot = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert_eq!((snapshot.revision(), snapshot.chips[0].balance), (1, 1));
    let items = crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Ids(&[first, second])).await.unwrap();
    assert_eq!(items.len(), 1);
    db.execute("DELETE FROM game.ship_adaption_chips WHERE character_id=$1", &[&character.character]).await.unwrap();
    db.execute("DELETE FROM game.ship_adaption_characters WHERE character_id=$1", &[&character.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, character).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn opposite_order_exchanges_are_normalized_and_stale_request_rolls_back() {
    use crate::services::ship_adaption::{model, operations, policy::{self, Input}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), character.character).await;
    client.login.user_id = character.account;
    let fixture = fixture::create(&db, client.sde.clone(), client.world.clone()).await;
    client.session.apply(vec![("stationid", Value::Int(fixture.station))]);
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&character.character, &fixture.station]).await.unwrap();
    let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = sender.clone();
    client.world.register_pilot(character.character, sender.clone());
    for type_id in [fixture.injector, fixture.injector2] {
        let item = crate::dao::items::transfers::insert(&db, type_id, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, 2).await.unwrap();
        rpc(&mut client, &fixture.host, "InjectChips", &[Value::Int(item), Value::Int(2)]).await.unwrap();
        assert_eq!(notices.recv().await.unwrap().0, "OnItemChange");
        assert_eq!(notices.recv().await.unwrap().0, "OnUserResponsed");
    }
    let pin = content::PinnedPolicy::pin(fixture.host.snapshot(), "ship_adaption").unwrap();
    let prepare = |rows| {
        let args = [Value::Int(fixture.issuer), Value::List(rows)];
        let kwargs = Value::None;
        let call = Call { service: "shipAdaptionMgr", method: "call_chip_exchange", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let model::Request::Exchange { issuer, chips } = model::decode(&call, character.account, character.character, 2).unwrap() else { panic!("exchange") };
        assert_eq!(chips, vec![(fixture.chip, 1), (fixture.chip2, 1)]);
        let input = Input { definitions: pin.adaption().unwrap().clone(), item_type: None, quantity: 0, credit: None, issuer, chips: Arc::new(chips) };
        policy::plan(&pin, input, std::time::Instant::now() + std::time::Duration::from_secs(30)).unwrap().unwrap()
    };
    let first = prepare(vec![chip(fixture.chip, 1), chip(fixture.chip2, 1)]);
    let second = prepare(vec![chip(fixture.chip2, 1), chip(fixture.chip, 1)]);
    let identity = crate::services::identity::Identity::of_client(&client);
    let apply = |plan: policy::PreparedPlan| {
        let db = db.clone();
        let world = client.world.clone();
        let sde = client.sde.clone();
        let sender = sender.clone();
        let host = fixture.host.clone();
        let pin = pin.clone();
        async move {
            let _market = crate::services::market::lock().await;
            let admitted = plan.admit(Arc::new(host.admit(&pin).unwrap())).unwrap().unwrap();
            operations::commit(&db, &world, &sender, sde, identity, character.account, 2, Arc::new(admitted), None).await
        }
    };
    let (a, b) = tokio::join!(apply(first), apply(second));
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(matches!(a, Err(CallError::User { .. })) || matches!(b, Err(CallError::User { .. })));
    assert_eq!(notices.recv().await.unwrap().0, "OnLPChange");
    assert_eq!(notices.recv().await.unwrap().0, "OnUserResponsed");
    assert_eq!(notices.recv().await.unwrap().0, "OnChipExchangeSuccessed");
    assert!(notices.try_recv().is_err());
    let snapshot = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert_eq!(snapshot.revision(), 3);
    assert_eq!(snapshot.chips.iter().map(|row| (row.type_id, row.balance)).collect::<Vec<_>>(), vec![(fixture.chip, 1), (fixture.chip2, 19)]);
    assert_eq!(crate::dao::wallets::loyalty::balance(&db, character.character, fixture.issuer).await.unwrap(), Some(4));
    db.execute("DELETE FROM game.ship_adaption_chips WHERE character_id=$1", &[&character.character]).await.unwrap();
    db.execute("DELETE FROM game.ship_adaption_characters WHERE character_id=$1", &[&character.character]).await.unwrap();
    db.execute("DELETE FROM game.lp_balances WHERE char_id=$1", &[&character.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, character).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn canonical_character_movement_wins_before_injection_revalidation() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), character.character).await;
    client.login.user_id = character.account;
    let fixture = fixture::create(&db, client.sde.clone(), client.world.clone()).await;
    let system = client.sde.map.station(fixture.station).unwrap().solar_system_id.unwrap();
    client.session.apply(vec![("stationid", Value::Int(fixture.station))]);
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&character.character, &fixture.station]).await.unwrap();
    let item = crate::dao::items::transfers::insert(&db, fixture.injector, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = sender.clone();
    client.world.register_pilot(character.character, sender);
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let release = Arc::new(tokio::sync::Notify::new());
    let (pid_tx, pid_rx) = tokio::sync::oneshot::channel();
    let pid_tx = Arc::new(std::sync::Mutex::new(Some(pid_tx)));
    let moving_db = db.clone();
    let moving_barrier = barrier.clone();
    let moving_release = release.clone();
    let moving = tokio::spawn(async move {
        moving_db.transaction(|tx, _: &mut ()| {
            let barrier = moving_barrier.clone();
            let release = moving_release.clone();
            let pid_tx = pid_tx.clone();
            Box::pin(async move {
                crate::dao::characters::selection::lock_character(tx, character.character).await?;
                crate::dao::characters::ship::undock(tx, character.character, system).await?;
                let pid = crate::dao::items::parent_locks::tests::backend_pid(tx).await;
                if let Some(sender) = pid_tx.lock().unwrap().take() { let _ = sender.send(pid); }
                barrier.wait().await;
                release.notified().await;
                Ok::<_, crate::db::Error>(())
            })
        }, |(), ()| async { Ok(()) }).await.unwrap();
    });
    barrier.wait().await;
    let host = fixture.host.clone();
    let caller = tokio::spawn(async move {
        rpc(&mut client, &host, "InjectChips", &[Value::Int(item), Value::Int(1)]).await
    });
    crate::dao::items::parent_locks::tests::wait_blocked(&db, pid_rx.await.unwrap()).await;
    release.notify_one();
    moving.await.unwrap();
    assert!(matches!(caller.await.unwrap(), Err(CallError::User { .. })));
    assert!(notices.try_recv().is_err());
    let snapshot = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert!(snapshot.subject.is_none() && snapshot.chips.is_empty());
    assert_eq!(crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Id(item)).await.unwrap()[0].quantity, 1);
    crate::services::tests::fixture::cleanup(&db, character).await.unwrap();
}
