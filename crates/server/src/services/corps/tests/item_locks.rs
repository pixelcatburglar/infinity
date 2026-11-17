//! Corporation blueprint vote reservations and enacted lock lifecycle.
use super::fixtures::{cleanup_votes, typed_vote_options, vote_call};
use crate::{
    client::Client,
    dao::corporations::{registry, shares, votes as vote_dao},
    db::{outbox::Outbox, Db},
    sde::Sde,
    services::{Call, CallResult},
};
use marshal::Value;

async fn proposal(client: &mut Client, item: i64, kind: i32, station: i64, unlock: bool) -> i64 {
    let labels = if unlock {
        ["UI/Corporations/CorporationWindow/Politics/UnlockItem", "UI/Corporations/CorporationWindow/Politics/DontUnlockItem"]
    } else {
        ["UI/Corporations/CorporationWindow/Politics/LockdownItem", "UI/Corporations/CorporationWindow/Politics/DontLockdownItem"]
    };
    let options = typed_vote_options(labels, [Some(item), Some(i64::from(kind)), Some(station)]);
    vote_call(client, "InsertVoteCase", &[
        Value::unicode("Blueprint"), Value::unicode(""),
        Value::Int(if unlock { super::super::vote_request::VOTE_UNLOCK } else { super::super::vote_request::VOTE_LOCKDOWN }),
        options, Value::Int(1),
    ]).await.unwrap().as_int().unwrap()
}

async fn close(db: &Db, sde: &std::sync::Arc<Sde>, corporation: i64, id: i64, now: i64) {
    let sde = sde.clone();
    db.transaction(move |tx, outbox: &mut Outbox| {
        let sde = sde.clone();
        Box::pin(async move {
            assert!(registry::lock_admission(tx, &[], &[corporation], &[]).await.unwrap());
            super::super::vote_close::close_inner(tx, &sde, outbox, id, now).await
        })
    }, |value, _: Outbox| async move { Ok(value) }).await.unwrap();
}

async fn rpc(client: &mut Client, service: &str, method: &str, args: &[Value], station: i64) -> CallResult {
    let kwargs = Value::None;
    let bound = Value::Int(station);
    crate::services::dispatch(client, &Call {
        service, method, args, kwargs: &kwargs, bound: true, bind_params: Some(&bound),
    }).await
}

async fn eligible(db: &Db, sde: &std::sync::Arc<Sde>, corp: i64, target: super::super::item_locks::Target) -> bool {
    let sde = sde.clone();
    db.transaction(move |tx, _: &mut ()| {
        let sde = sde.clone();
        Box::pin(async move {
            assert!(registry::lock_admission(tx, &[], &[corp], &[]).await.unwrap());
            super::super::item_locks::admit_target(
                tx, &sde, corp, target, super::super::item_locks::Action::Lock, None, crate::client::filetime_now(),
            ).await.map(|_| ())
        })
    }, |(), ()| async move { Ok(()) }).await.is_ok()
}

async fn reservation_blocks_waiting_move(db: &Db, sde: &std::sync::Arc<Sde>, corp: i64, actor: i64, target: super::super::item_locks::Target, office: i64) {
    use crate::dao::items::{loading::ItemQuery, parent_locks::{LockRequest, LockStrength}};
    use crate::services::inventory::{destination::{self, Custody, Destination}, movement};
    let mut first = super::fixtures::connect().await;
    let mut second = super::fixtures::connect().await;
    let tx = first.transaction().await.unwrap();
    assert!(registry::lock_admission(&tx, &[], &[corp], &[actor]).await.unwrap());
    let now = crate::client::filetime_now();
    let case = vote_dao::create(&tx, &vote_dao::NewCase {
        corporation_id: corp, creator_id: actor, title: "lock", description: "",
        vote_type: super::super::vote_request::VOTE_LOCKDOWN, starts: now,
        ends: now + infinity_core::consts::FILETIME_DAY, previous_ceo_id: None, previous_ceo_was_active: None,
    }).await.unwrap();
    let locks = super::super::item_locks::admit_target(&tx, sde, corp, target, super::super::item_locks::Action::Lock, None, now).await.unwrap();
    assert!(vote_dao::reserve_item(&tx, case, target.item_id).await.unwrap());
    drop(locks);
    let (send, receive) = tokio::sync::oneshot::channel();
    let sde = sde.clone();
    let task = tokio::spawn(async move {
        let tx = second.transaction().await.unwrap();
        let pid: i32 = tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
        let item = crate::services::items::load_items(&tx, &sde, ItemQuery::Id(target.item_id)).await.unwrap().pop().unwrap();
        let ancestors = crate::dao::items::deliveries::ancestry(&tx, office).await.unwrap();
        let mut requests = destination::parent_requests(&sde, ancestors.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share).unwrap();
        requests.push(LockRequest { item_id: target.item_id, strength: LockStrength::Update });
        let descendants = crate::dao::items::inventory::descendants(&tx, &[target.item_id]).await.unwrap();
        requests.extend(descendants.iter().map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }));
        send.send(pid).unwrap();
        let locks = destination::lock_items(&tx, &sde, &requests, &[]).await.unwrap();
        assert!(crate::dao::items::deliveries::ancestry(&tx, office).await.unwrap() == ancestors);
        assert_eq!(crate::dao::items::inventory::descendants(&tx, &[target.item_id]).await.unwrap(), descendants);
        let dest = Destination::owned(corp, office, infinity_core::consts::FLAG_CORP_SAG1 + 1);
        let mut outbox = Outbox::<()>::default();
        let result = movement::move_core(&tx, &sde, &mut outbox, &item, &dest, None, Custody::Preserve, &locks, None, None, actor).await;
        assert!(matches!(result, Err(crate::services::CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(infinity_core::messages::corporations::ITEM_LOCKED_OR_RESERVED))]));
        drop(locks);
        tx.rollback().await.unwrap();
    });
    let pid = receive.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: bool = tx.query_one("SELECT pg_backend_pid()=ANY(pg_blocking_pids($1)) AS waiting", &[&pid]).await.unwrap().get("waiting");
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("move did not wait on the reservation's item lock");
    tx.commit().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), task).await.unwrap().unwrap();
    let row = first.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&target.item_id]).await.unwrap();
    assert_eq!(row.get::<_, i64>("location_id"), office);
    assert_eq!(vote_dao::case(db, case).await.unwrap().unwrap().reserved_item_id, Some(target.item_id));
    cleanup_votes(db, &[corp]).await.unwrap();
}

async fn exercise_votes(client: &mut Client, db: &Db, sde: &std::sync::Arc<Sde>, corp: i64, station: i64, office: i64, item: i64, kind: i32, nested: super::super::item_locks::Target, container: i64) {
    let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
    let actor = client.session.int("charid").unwrap();
    client.world.register_pilot(actor, sender);
    for (option, late) in [(None, false), (Some(1), false), (Some(0), true)] {
        let case = proposal(client, item, kind, station, false).await;
        if let Some(option) = option {
            vote_call(client, "InsertVote", &[Value::Int(corp), Value::Int(case), Value::Int(option)]).await.unwrap();
        }
        let deadline = vote_dao::case(db, case).await.unwrap().unwrap().ends;
        close(db, sde, corp, case, if late { i64::MAX } else { deadline }).await;
        assert_eq!(vote_dao::case(db, case).await.unwrap().unwrap().reserved_item_id, None);
    }
    let expiry = proposal(client, item, kind, station, false).await;
    vote_call(client, "InsertVote", &[Value::Int(corp), Value::Int(expiry), Value::Int(0)]).await.unwrap();
    let deadline = vote_dao::case(db, expiry).await.unwrap().unwrap().ends;
    close(db, sde, corp, expiry, deadline).await;
    assert_eq!(vote_dao::case(db, expiry).await.unwrap().unwrap().reserved_item_id, Some(item));
    db.execute("UPDATE game.corp_sanctioned_actions SET expires=$2 WHERE case_id=$1", &[&expiry, &crate::client::filetime_now()]).await.unwrap();
    let job = crate::scheduler::Job { id: format!("{}:{expiry}", super::super::sanctions::KIND), kind: super::super::sanctions::KIND.into(), due_at: 0, payload: expiry.to_string() };
    super::super::sanctions::handle(db.clone(), client.world.clone(), job).await.unwrap();
    assert_eq!(vote_dao::case(db, expiry).await.unwrap().unwrap().reserved_item_id, None);
    assert!(crate::dao::corporations::sanctions::get(db, expiry).await.unwrap().unwrap().expired_at.is_some());
    let expiry_bind = Value::tuple([Value::Int(office)]);
    let expiry_args = [Value::Int(item), Value::Int(office)];
    for flag in [infinity_core::consts::FLAG_CORP_SAG1 + 1, infinity_core::consts::FLAG_CORP_SAG1] {
        let kwargs = Value::dict([("flag", Value::Int(i64::from(flag)))]);
        crate::services::dispatch(client, &Call { service: "invInventory", method: "Add", args: &expiry_args, kwargs: &kwargs, bound: true, bind_params: Some(&expiry_bind) }).await.unwrap();
    }
    let case = proposal(client, item, kind, station, false).await;
    assert_eq!(vote_dao::case(db, case).await.unwrap().unwrap().reserved_item_id, Some(item));
    assert!(crate::dao::items::corporation_locks::get(db, item).await.unwrap().is_none());
    assert_eq!(rpc(client, "itemLocking", "GetItemsByLocation", &[Value::Int(station)], station).await.unwrap(), Value::Dict(vec![]));
    assert!(std::iter::from_fn(|| notices.try_recv().ok()).all(|event| event.0 != "OnLockedItemChange"));
    let kwargs = Value::dict([("flag", Value::Int(i64::from(infinity_core::consts::FLAG_CORP_SAG1 + 1)))]);
    let bind = Value::tuple([Value::Int(office)]);
    let args = [Value::Int(item), Value::Int(office)];
    let call = Call { service: "invInventory", method: "Add", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert!(matches!(crate::services::dispatch(client, &call).await, Err(crate::services::CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(infinity_core::messages::corporations::ITEM_LOCKED_OR_RESERVED))]));
    vote_call(client, "InsertVote", &[Value::Int(corp), Value::Int(case), Value::Int(0)]).await.unwrap();
    let deadline = vote_dao::case(db, case).await.unwrap().unwrap().ends;
    close(db, sde, corp, case, deadline).await;
    assert_eq!(vote_dao::case(db, case).await.unwrap().unwrap().reserved_item_id, Some(item));
    vote_call(client, "ActivateSanctionedAction", &[Value::Int(case)]).await.unwrap();
    assert_eq!(crate::dao::items::corporation_locks::get(db, item).await.unwrap().unwrap().case_id, case);
    assert_eq!(vote_dao::case(db, case).await.unwrap().unwrap().reserved_item_id, None);
    assert!(std::iter::from_fn(|| notices.try_recv().ok()).any(|event| event.0 == "OnLockedItemChange"));
    assert!(matches!(crate::services::dispatch(client, &call).await, Err(crate::services::CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(infinity_core::messages::corporations::ITEM_LOCKED_OR_RESERVED))]));
    let trash = [Value::list([Value::Int(item)]), Value::Int(station)];
    assert!(matches!(rpc(client, "invbroker", "TrashItems", &trash, station).await, Err(crate::services::CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(infinity_core::messages::corporations::ITEM_LOCKED_OR_RESERVED))]));
    let contract = Value::dict([
        ("contractType", Value::Int(1)), ("startStationID", Value::Int(station)),
        ("startStationDivision", Value::Int(i64::from(infinity_core::consts::FLAG_CORP_SAG1))),
        ("minutesExpire", Value::Int(1440)), ("forCorp", Value::Bool(true)),
        ("itemList", Value::list([Value::tuple([Value::Int(item), Value::Int(1)])])),
        ("requestItemTypeList", Value::list([])),
    ]);
    assert!(matches!(rpc(client, "contractProxy", "CreateContract", &[contract, Value::Bool(false)], station).await, Err(crate::services::CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(infinity_core::messages::corporations::ITEM_LOCKED_OR_RESERVED))]));
    let stack_kind = sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)).unwrap().type_id;
    for _ in 0..2 {
        crate::dao::items::transfers::insert(db, stack_kind, corp, office, infinity_core::consts::FLAG_CORP_SAG1, 1).await.unwrap();
    }
    let stack_args = [Value::Int(i64::from(infinity_core::consts::FLAG_CORP_SAG1))];
    let stack_call = Call { method: "StackAll", args: &stack_args, kwargs: &Value::None, ..call };
    crate::services::dispatch(client, &stack_call).await.unwrap();
    let stack = db.query_one("SELECT count(*) AS n, sum(quantity)::bigint AS quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&corp, &office, &infinity_core::consts::FLAG_CORP_SAG1, &stack_kind]).await.unwrap();
    assert_eq!((stack.get::<_, i64>("n"), stack.get::<_, i64>("quantity")), (1, 2));
    rpc(client, "officeManager", "UnrentOffice", &[], station).await.unwrap();
    assert!(eligible(db, sde, corp, nested).await);
    assert!(crate::dao::items::corporation_locks::get(db, item).await.unwrap().is_some());
    let quote = rpc(client, "officeManager", "GetImpoundReleasePrice", &[], station).await.unwrap();
    let balance = crate::dao::wallets::corporations::balance(db, corp, infinity_core::consts::ACCOUNT_CASH).await.unwrap();
    assert!(rpc(client, "officeManager", "GetItemsFromImpound", &[quote.clone()], station).await.is_err());
    assert!(rpc(client, "officeManager", "TrashImpoundedOffice", &[], station).await.is_err());
    assert_eq!(crate::dao::wallets::corporations::balance(db, corp, infinity_core::consts::ACCOUNT_CASH).await.unwrap(), balance);
    let location: i64 = db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get("location_id");
    assert_eq!(location, office);
    let nested_case = proposal(client, nested.item_id, nested.type_id, station, false).await;
    vote_call(client, "InsertVote", &[Value::Int(corp), Value::Int(nested_case), Value::Int(0)]).await.unwrap();
    let deadline = vote_dao::case(db, nested_case).await.unwrap().unwrap().ends;
    close(db, sde, corp, nested_case, deadline).await;
    vote_call(client, "ActivateSanctionedAction", &[Value::Int(nested_case)]).await.unwrap();
    let rent = rpc(client, "officeManager", "GetPriceQuote", &[Value::Int(corp)], station).await.unwrap();
    rpc(client, "officeManager", "RentOffice", &[rent], station).await.unwrap();
    let parent_args = [Value::Int(container), Value::Int(office)];
    let parent_move = Call { service: "invInventory", method: "Add", args: &parent_args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    assert!(matches!(crate::services::dispatch(client, &parent_move).await, Err(crate::services::CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(infinity_core::messages::corporations::ITEM_LOCKED_OR_RESERVED))]));
    let parent_trash = [Value::list([Value::Int(container)]), Value::Int(station)];
    assert!(matches!(rpc(client, "invbroker", "TrashItems", &parent_trash, station).await, Err(crate::services::CallError::User { dict, .. }) if dict == vec![("notify".into(), Value::unicode(infinity_core::messages::corporations::ITEM_LOCKED_OR_RESERVED))]));
    rpc(client, "officeManager", "UnrentOffice", &[], station).await.unwrap();
    let nested_unlock = proposal(client, nested.item_id, nested.type_id, station, true).await;
    vote_call(client, "InsertVote", &[Value::Int(corp), Value::Int(nested_unlock), Value::Int(0)]).await.unwrap();
    let deadline = vote_dao::case(db, nested_unlock).await.unwrap().unwrap().ends;
    close(db, sde, corp, nested_unlock, deadline).await;
    vote_call(client, "ActivateSanctionedAction", &[Value::Int(nested_unlock)]).await.unwrap();
    let unlocked = proposal(client, item, kind, station, true).await;
    vote_call(client, "InsertVote", &[Value::Int(corp), Value::Int(unlocked), Value::Int(0)]).await.unwrap();
    let deadline = vote_dao::case(db, unlocked).await.unwrap().unwrap().ends;
    close(db, sde, corp, unlocked, deadline).await;
    vote_call(client, "ActivateSanctionedAction", &[Value::Int(unlocked)]).await.unwrap();
    assert!(crate::dao::items::corporation_locks::get(db, item).await.unwrap().is_none());
    let old = crate::dao::corporations::sanctions::get(db, case).await.unwrap().unwrap();
    let new = crate::dao::corporations::sanctions::get(db, unlocked).await.unwrap().unwrap();
    assert!(old.acted_upon && !old.in_effect && old.time_rescinded.is_some());
    assert!(new.acted_upon);
    let rent = rpc(client, "officeManager", "GetPriceQuote", &[Value::Int(corp)], station).await.unwrap();
    rpc(client, "officeManager", "RentOffice", &[rent], station).await.unwrap();
    crate::services::dispatch(client, &Call { service: "invInventory", method: "Add", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) }).await.unwrap();
    let moved = db.query_one("SELECT flag_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
    assert_eq!(moved.get::<_, i32>("flag_id"), infinity_core::consts::FLAG_CORP_SAG1 + 1);
    rpc(client, "officeManager", "UnrentOffice", &[], station).await.unwrap();
    rpc(client, "officeManager", "GetItemsFromImpound", &[quote], station).await.unwrap();
    client.world.unregister_pilot_generation(actor, None);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn blueprint_vote_reservation_lock_and_impound_unlock() {
    use crate::services::tests::fixture;
    use infinity_core::consts::{ACCOUNT_CASH, ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_HANGAR_CAN_TAKE_1};
    use std::sync::Arc;

    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|station| station.solar_system_id.is_some()).unwrap();
    let station_id = station.station_id;
    let actor = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, actor.character, station_id).await.unwrap();
    fixture::assign_corporation(&db, actor.character, corp, ROLE_DIRECTOR | ROLE_ACCOUNTANT | ROLE_HANGAR_CAN_TAKE_1).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&actor.character, &station_id]).await.unwrap();
    db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,1000000000) ON CONFLICT (corporation_id,account_key) DO UPDATE SET balance=EXCLUDED.balance", &[&corp, &ACCOUNT_CASH]).await.unwrap();
    assert!(shares::initialize(&db, corp, 100).await.unwrap());
    assert!(shares::transfer(&db, corp, corp, actor.character, 100).await.unwrap());
    let (mut client, _peer) = crate::services::tests::client_with_sde(db.clone(), actor.character, sde.clone()).await;
    client.session.apply(vec![("corpid", Value::Int(corp)), ("stationid", Value::Int(station_id)), ("corpAccountKey", Value::Int(i64::from(ACCOUNT_CASH)))]);
    let quote = rpc(&mut client, "officeManager", "GetPriceQuote", &[Value::Int(corp)], station_id).await.unwrap();
    rpc(&mut client, "officeManager", "RentOffice", &[quote], station_id).await.unwrap();
    let office = crate::dao::offices::identity(&db, corp, station_id).await.unwrap().unwrap().office_id;
    let (item, kind) = super::fixtures::original_blueprint(&db, &sde, corp, office).await;
    let target = super::super::item_locks::Target { item_id: item, type_id: kind, station_id };
    assert!(eligible(&db, &sde, corp, target).await);
    reservation_blocks_waiting_move(&db, &sde, corp, actor.character, target, office).await;
    let (child, child_kind) = super::fixtures::original_blueprint(&db, &sde, corp, office).await;
    let container_type = sde.types.iter().find(|ty| ty.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let container = crate::dao::items::transfers::insert(&db, container_type, corp, office, infinity_core::consts::FLAG_CORP_SAG1, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&container]).await.unwrap();
    db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&child, &container, &infinity_core::consts::FLAG_CARGO]).await.unwrap();
    let nested = super::super::item_locks::Target { item_id: child, type_id: child_kind, station_id };
    assert!(!eligible(&db, &sde, corp, nested).await);
    exercise_votes(&mut client, &db, &sde, corp, station_id, office, item, kind, nested, container).await;
    cleanup_votes(&db, &[corp]).await.unwrap();
    db.execute("DELETE FROM game.blueprints WHERE item_id=ANY($1)", &[&&[item, child][..]]).await.unwrap();
    db.execute("DELETE FROM game.corp_shares WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}