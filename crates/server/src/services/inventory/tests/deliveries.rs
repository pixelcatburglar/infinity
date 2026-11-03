use super::{inventory, list};
use super::super::{movement, CONTAINER_CORP_MARKET, INVENTORY};
use super::super::access::{container_of, corp_hangar_role, may_take_corp_hangar, may_view_corp_hangar, Container};
use super::super::inv_broker::broker;
use super::super::notifications::notify_corp_item_change;
use super::super::stacks::{plan_merges, trash_items};
use super::super::tests::item;
use crate::consts::{CORP_HANGAR_DIVISIONS, FLAG_CARGO, FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1, FLAG_HANGAR, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD, ROLE_DELIVERIES_TAKE, ROLE_DELIVERIES_VIEW, ROLE_DIRECTOR, ROLE_HANGAR_CAN_QUERY_1, ROLE_HANGAR_CAN_TAKE_1};
use crate::dao::items::loading::{Item, ItemQuery};
use crate::db::outbox::Outbox;
use crate::messages;
use crate::services::{items::load_items, notify_error, Call, CallError};
use marshal::Value;

#[test]
fn deliveries_binding_and_division_boundaries() {
    let params = Value::tuple([Value::Int(42), Value::Int(CONTAINER_CORP_MARKET), Value::Int(99)]);
    assert!(matches!(container_of(Some(&params)), Some(Container::Deliveries { station_id: 42, owner_id: 99 })));
    let bad = Value::tuple([Value::Int(42), Value::Int(CONTAINER_CORP_MARKET)]);
    assert!(container_of(Some(&bad)).is_none());
    assert_eq!(corp_hangar_role(FLAG_CORP_DELIVERIES, false), Some(ROLE_DELIVERIES_VIEW));
    assert_eq!(corp_hangar_role(FLAG_CORP_DELIVERIES, true), Some(ROLE_DELIVERIES_TAKE));
    assert_eq!(corp_hangar_role(FLAG_CORP_SAG1 - 1, true), None);
    assert_eq!(corp_hangar_role(FLAG_CORP_SAG1 + CORP_HANGAR_DIVISIONS, false), None);
    assert_eq!(corp_hangar_role(FLAG_CORP_SAG1 + CORP_HANGAR_DIVISIONS - 1, true), Some(ROLE_HANGAR_CAN_TAKE_1 << (CORP_HANGAR_DIVISIONS - 1)));
    assert_eq!(corp_hangar_role(FLAG_CORP_SAG1, false), Some(ROLE_HANGAR_CAN_QUERY_1));
    assert_eq!(corp_hangar_role(FLAG_CORP_SAG1, true), Some(ROLE_HANGAR_CAN_TAKE_1));
}

#[test]
fn merge_cannot_move_quantities_between_custodians() {
    let first = item(1, 10);
    let second = Item { owner_id: 2, ..item(2, 5) };
    assert!(plan_merges(&[first, second], &[(1, 2, 1)]).is_err());
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn deliveries_roles_listing_and_take() {
    use crate::consts::{ROLE_ACCOUNTANT, ROLE_JUNIOR_ACCOUNTANT, ROLE_TRADER};
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    let type_id = client.sde.types.iter().find(|t| t.category_id != Some(crate::consts::CATEGORY_MODULE) && t.category_id != Some(crate::consts::CATEGORY_SUBSYSTEM)).unwrap().type_id;
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name, ticker, ceo_id, creator_id, station_id, created) VALUES ('delivery-' || nextval('game.corporations_corporation_id_seq')::text, 'D' || nextval('game.corporations_corporation_id_seq')::text, $1, $1, $2, 0) RETURNING corporation_id", &[&ceo.character, &station]).await.unwrap().get("corporation_id");
    db.execute("UPDATE game.characters SET corporation_id = $1 WHERE character_id = ANY($2)", &[&corp, &vec![actor.character, ceo.character, outsider.character]]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles = 0 WHERE character_id = $1", &[&outsider.character]).await.unwrap();
    client.session.apply(vec![("corpid", Value::Int(corp)), ("stationid", Value::Int(station))]);
    let id = crate::dao::items::transfers::insert(&db, type_id, corp, station, FLAG_CORP_DELIVERIES, 10).await.unwrap();
    let deliveries = Container::Deliveries { station_id: station, owner_id: corp };
    let hangar = Container::Hangar { station_id: station, owner_id: actor.character };
    for (role, view, take) in [(0, false, false), (ROLE_JUNIOR_ACCOUNTANT, true, false), (ROLE_ACCOUNTANT, true, true), (ROLE_TRADER, true, true), (ROLE_DIRECTOR, true, true)] {
        db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor.character, &role]).await.unwrap();
        assert_eq!(may_view_corp_hangar(&db, actor.character, corp, FLAG_CORP_DELIVERIES).await.ok().unwrap(), view);
        assert_eq!(may_take_corp_hangar(&db, actor.character, corp, FLAG_CORP_DELIVERIES).await.ok().unwrap(), take);
        assert_eq!(list(&client, &deliveries, None).await.ok().unwrap().len(), usize::from(view));
        if !take { assert!(movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &hangar, id, Some(station), None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_err()); }
        assert!(!may_view_corp_hangar(&db, actor.character, corp + 1, FLAG_CORP_DELIVERIES).await.ok().unwrap());
    }
    assert!(may_take_corp_hangar(&db, ceo.character, corp, FLAG_CORP_DELIVERIES).await.ok().unwrap());
    for (role, view, take) in [(ROLE_HANGAR_CAN_QUERY_1, true, false), (ROLE_HANGAR_CAN_TAKE_1, false, true)] {
        db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor.character, &role]).await.unwrap();
        assert_eq!(may_view_corp_hangar(&db, actor.character, corp, FLAG_CORP_SAG1).await.ok().unwrap(), view);
        assert_eq!(may_take_corp_hangar(&db, actor.character, corp, FLAG_CORP_SAG1).await.ok().unwrap(), take);
    }
    db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor.character, &ROLE_DIRECTOR]).await.unwrap();
    let params = Value::tuple([Value::Int(station), Value::Int(CONTAINER_CORP_MARKET), Value::Int(corp)]);
    let kwargs = Value::Dict(vec![]);
    let bind_args = [Value::Int(CONTAINER_CORP_MARKET), Value::Int(corp)];
    let bind_call = Call { service: "invbroker", method: "GetInventory", args: &bind_args, kwargs: &kwargs, bound: false, bind_params: None };
    assert!(broker(&mut client, &bind_call).await.unwrap().is_ok());
    let bad_bind_args = [Value::Int(CONTAINER_CORP_MARKET), Value::Int(corp + 1)];
    let bad_bind_call = Call { service: "invbroker", method: "GetInventory", args: &bad_bind_args, kwargs: &kwargs, bound: false, bind_params: None };
    assert!(broker(&mut client, &bad_bind_call).await.unwrap().is_err());
    let list_call = Call { service: INVENTORY, method: "List", args: &[], kwargs: &kwargs, bound: true, bind_params: Some(&params) };
    assert_eq!(inventory(&mut client, &list_call).await.ok().unwrap().items().unwrap().len(), 1);
    let spoof = Value::tuple([Value::Int(station), Value::Int(CONTAINER_CORP_MARKET), Value::Int(corp + 1)]);
    let spoof_call = Call { service: INVENTORY, method: "List", args: &[], kwargs: &kwargs, bound: true, bind_params: Some(&spoof) };
    assert!(inventory(&mut client, &spoof_call).await.is_err());
    let row = load_items(&db, &client.sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap();
    let mut outbox: Outbox = Outbox::default();
    notify_corp_item_change(&db, &mut outbox, &row, vec![(IX_LOCATION_ID, Value::Int(0))]).await.ok().unwrap();
    let mut recipients = Vec::new();
    for entry in outbox.into_entries() {
        if let crate::db::outbox::Effect::Notification(crate::db::outbox::Notification::Pilot { char_id, .. }) = entry {
            recipients.push(char_id);
        } else { panic!("corporate notifications must target pilots"); }
    }
    recipients.sort_unstable();
    let mut expected = vec![actor.character, ceo.character];
    expected.sort_unstable();
    assert_eq!(recipients, expected);
    let direct = Value::tuple([Value::Int(station)]);
    let flags = [Value::Int(i64::from(FLAG_CORP_DELIVERIES))];
    let direct_call = Call { service: INVENTORY, method: "List", args: &flags, kwargs: &kwargs, bound: true, bind_params: Some(&direct) };
    assert_eq!(inventory(&mut client, &direct_call).await.ok().unwrap().items().unwrap().len(), 1);
    db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor.character, &0_i64]).await.unwrap();
    assert!(inventory(&mut client, &direct_call).await.ok().unwrap().items().unwrap().is_empty());
    assert!(inventory(&mut client, &list_call).await.is_err());
    let split_args = [Value::Int(station), Value::Int(id), Value::Int(1), Value::Int(corp)];
    let split_call = Call { service: "invbroker", method: "SplitStack", args: &split_args, kwargs: &kwargs, bound: false, bind_params: None };
    assert!(broker(&mut client, &split_call).await.unwrap().is_err());
    let merge_args = [Value::list([Value::tuple([Value::Int(id), Value::Int(id), Value::Int(1)])])];
    let merge_call = Call { service: INVENTORY, method: "MultiMerge", args: &merge_args, kwargs: &kwargs, bound: true, bind_params: Some(&direct) };
    assert!(inventory(&mut client, &merge_call).await.is_err());
    assert!(trash_items(&mut client, &[id]).await.is_err());
    assert!(movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &hangar, id, Some(station), None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_err());
    db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor.character, &ROLE_DIRECTOR]).await.unwrap();
    let (actor_tx, mut actor_rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = actor_tx;
    let (ceo_tx, mut ceo_rx) = tokio::sync::mpsc::unbounded_channel();
    let (outsider_tx, mut outsider_rx) = tokio::sync::mpsc::unbounded_channel();
    client.world.register_pilot(ceo.character, ceo_tx);
    client.world.register_pilot(outsider.character, outsider_tx);
    let dispatched = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let rollback: Result<(), CallError> = db.transaction(
        {
            let row = row.clone();
            let sde = client.sde.clone();
            move |tx, outbox: &mut Outbox| {
                let row = row.clone();
                let sde = sde.clone();
                Box::pin(async move {
                    let mut effects: Outbox = Outbox::default();
                    movement::move_core(tx, &sde, &mut effects, &row, actor.character, station, FLAG_HANGAR, None).await?;
                    let mut pilots = Vec::new();
                    let mut actor_notice = false;
                    for effect in effects.into_entries() {
                        match effect {
                            crate::db::outbox::Effect::Notification(crate::db::outbox::Notification::Pilot { char_id, event, .. }) if event == "OnItemChange" => pilots.push(char_id),
                            crate::db::outbox::Effect::Notification(crate::db::outbox::Notification::DirectClient { event, .. }) if event == "OnItemChange" => actor_notice = true,
                            _ => {}
                        }
                    }
                    pilots.sort_unstable();
                    let mut expected = vec![actor.character, ceo.character];
                    expected.sort_unstable();
                    assert_eq!(pilots, expected);
                    assert!(actor_notice);
                    outbox.notify_client("OnItemChange", vec![]);
                    Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))
                })
            }
        },
        {
            let dispatched = dispatched.clone();
            move |(), _| async move { dispatched.store(true, std::sync::atomic::Ordering::SeqCst); Ok(()) }
        },
    ).await;
    assert!(rollback.is_err());
    assert!(!dispatched.load(std::sync::atomic::Ordering::SeqCst));
    assert!(actor_rx.try_recv().is_err());
    assert!(ceo_rx.try_recv().is_err());
    assert!(outsider_rx.try_recv().is_err());
    let unchanged = load_items(&db, &client.sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap();
    assert_eq!((unchanged.owner_id, unchanged.location_id, unchanged.flag_id, unchanged.quantity), (corp, station, FLAG_CORP_DELIVERIES, 10));
    let add_args = [Value::Int(id), Value::Int(station)];
    let add_call = Call { service: INVENTORY, method: "Add", args: &add_args, kwargs: &kwargs, bound: true, bind_params: Some(&params) };
    assert!(inventory(&mut client, &add_call).await.is_err());
    let multi_args = [Value::list([Value::Int(id)]), Value::Int(station)];
    let multi_call = Call { service: INVENTORY, method: "MultiAdd", args: &multi_args, kwargs: &kwargs, bound: true, bind_params: Some(&params) };
    assert!(inventory(&mut client, &multi_call).await.is_err());
    assert!(movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &deliveries, id, None, None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_err());
    assert!(movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &hangar, id, Some(station), Some(FLAG_CORP_DELIVERIES), None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.is_err());
    let root = crate::dao::items::transfers::insert(&db, type_id, corp, station, FLAG_CORP_DELIVERIES, 1).await.unwrap();
    let child = crate::dao::items::transfers::insert(&db, type_id, corp, root, FLAG_CARGO, 1).await.unwrap();
    let nested = crate::dao::items::transfers::insert(&db, type_id, corp, child, FLAG_CARGO, 1).await.unwrap();
    let leaf = crate::dao::items::transfers::insert(&db, type_id, corp, nested, FLAG_CARGO, 2).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = ANY($1)", &[&vec![root, child, nested]]).await.unwrap();
    let nested_params = Value::tuple([Value::Int(nested)]);
    for method in ["GetSelfInvItem", "List", "ListByFlags"] {
        let call = Call { service: INVENTORY, method, args: &[], kwargs: &kwargs, bound: true, bind_params: Some(&nested_params) };
        assert!(inventory(&mut client, &call).await.is_ok());
        db.execute("UPDATE game.characters SET corp_roles = 0 WHERE character_id = $1", &[&actor.character]).await.unwrap();
        assert!(inventory(&mut client, &call).await.is_err());
        db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor.character, &ROLE_DIRECTOR]).await.unwrap();
        db.execute("UPDATE game.characters SET corporation_id = 1 WHERE character_id = $1", &[&actor.character]).await.unwrap();
        assert!(inventory(&mut client, &call).await.is_err());
        db.execute("UPDATE game.characters SET corporation_id = $2 WHERE character_id = $1", &[&actor.character, &corp]).await.unwrap();
    }
    let nested_row = load_items(&db, &client.sde, ItemQuery::Id(leaf)).await.unwrap().pop().unwrap();
    let mut nested_outbox: Outbox = Outbox::default();
    notify_corp_item_change(&db, &mut nested_outbox, &nested_row, vec![(IX_STACK_SIZE, Value::Int(3))]).await.ok().unwrap();
    let nested_recipients: Vec<_> = nested_outbox.into_entries().into_iter().filter_map(|entry| match entry {
        crate::db::outbox::Effect::Notification(crate::db::outbox::Notification::Pilot { char_id, .. }) => Some(char_id),
        _ => None,
    }).collect();
    assert!(nested_recipients.contains(&actor.character));
    assert!(nested_recipients.contains(&ceo.character));
    assert!(!nested_recipients.contains(&outsider.character));
    assert!(trash_items(&mut client, &[root]).await.is_err());
    assert_eq!(movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &hangar, id, Some(station), None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.ok().unwrap(), Some(id));
    let moved = load_items(&db, &client.sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap();
    assert_eq!((moved.owner_id, moved.location_id, moved.flag_id), (actor.character, station, FLAG_HANGAR));
    let notice = ceo_rx.try_recv().unwrap();
    assert_eq!(notice.0, "OnItemChange");
    assert_eq!(notice.1[0], Item { location_id: LOCATION_JUNKYARD, ..row.clone() }.row());
    assert!(outsider_rx.try_recv().is_err());
    let notices: Vec<_> = std::iter::from_fn(|| actor_rx.try_recv().ok()).collect();
    assert!(notices.iter().any(|(event, args)| event == "OnItemChange" && args[0] == Item { location_id: LOCATION_JUNKYARD, ..row.clone() }.row()));
    assert!(notices.iter().any(|(event, args)| event == "OnItemChange" && args[0] == moved.row()));
    assert_eq!(movement::move_item(&crate::services::identity::Identity::of_client(&client), &client.db, &client.world, &client.sde, client.session.int("solarsystemid"), client.notify_tx.clone(), &hangar, root, Some(station), None, None, |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))).await.ok().unwrap(), Some(root));
    for (id, parent) in [(child, root), (nested, child), (leaf, nested)] {
        let item = load_items(&db, &client.sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap();
        assert_eq!((item.owner_id, item.location_id, item.flag_id), (actor.character, parent, FLAG_CARGO));
    }
    client.world.unregister_pilot(ceo.character);
    client.world.unregister_pilot(outsider.character);
    db.execute("UPDATE game.characters SET corporation_id = 1 WHERE character_id = ANY($1)", &[&vec![actor.character, ceo.character, outsider.character]]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id = $1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, outsider).await.unwrap();
}
