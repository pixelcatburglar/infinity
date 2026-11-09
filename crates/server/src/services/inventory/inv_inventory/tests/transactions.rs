use super::*;
use crate::dao::items::{inventory as item_inventory, parent_locks::tests::{backend_pid, wait_blocked}, quantities::{self, QuantityChange}, ship, transfers};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn bound_split_refuses_stale_locked_hull_owner_and_lineage() {
    for change_owner in [true, false] {
        let mut f = setup().await;
        let stack = load_items(&f.db, &f.client.sde, ItemQuery::Id(f.mine)).await.unwrap().pop().unwrap();
        assert!(quantities::apply(&f.db, &[(f.mine, QuantityChange::new(&stack, Some(stack.quantity + 4)))].into_iter().collect()).await.unwrap());
        let hull = load_items(&f.db, &f.client.sde, ItemQuery::Id(f.host)).await.unwrap().pop().unwrap();
        let before = load_items(&f.db, &f.client.sde, ItemQuery::LocationFlags(f.host, &[FLAG_FLEET_HANGAR])).await.unwrap();
        let (notify_tx, mut notices) = tokio::sync::mpsc::unbounded_channel();
        f.client.notify_tx = notify_tx;
        f.client.outbox.clear();
        let cfg = crate::config::tests::test_database().to_pg_config().unwrap();
        let (mut holder, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
        let driver = tokio::spawn(async move { connection.await.unwrap(); });
        let tx = holder.transaction().await.unwrap();
        let pid = backend_pid(&tx).await;
        assert!(ship::lock_reload_ship(&tx, f.host).await.unwrap());
        let client = f.client;
        let host = f.host;
        let stack = f.mine;
        let owner = f.actor.character;
        let task = tokio::spawn(async move {
            let mut client = client;
            let bind = Value::tuple([Value::Int(host)]);
            let args = [Value::Int(host), Value::Int(stack), Value::Int(1), Value::Int(owner)];
            let kwargs = Value::Dict(vec![]);
            let result = crate::services::dispatch(&mut client, &Call { service: "invbroker", method: "SplitStack", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) }).await;
            (client, result)
        });
        wait_blocked(&f.db, pid).await;
        let new_owner = if change_owner { f.outsider.character } else { f.owner.character };
        let new_location = if change_owner { f.system } else { f.system - 1 };
        assert!(item_inventory::move_item(&tx, &hull, new_location, hull.flag_id, new_owner, true).await.unwrap());
        tx.commit().await.unwrap();
        let (client, result) = task.await.unwrap();
        f.client = client;
        assert!(matches!(result, Err(CallError::User { .. })));
        let after = load_items(&f.db, &f.client.sde, ItemQuery::LocationFlags(f.host, &[FLAG_FLEET_HANGAR])).await.unwrap();
        let fingerprint = |items: Vec<Item>| items.into_iter().map(|item| (item.item_id, item.owner_id, item.location_id, item.flag_id, item.quantity, item.singleton)).collect::<Vec<_>>();
        assert_eq!(fingerprint(before), fingerprint(after));
        assert!(f.client.outbox.is_empty());
        assert!(notices.try_recv().is_err());
        drop(holder);
        driver.await.unwrap();
        cleanup(f).await;
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn shared_take_deadlock_retries_or_refuses_stale_closure_without_losing_refunds() {
    use infinity_core::consts::{CATEGORY_MODULE, GROUP_CARGO_CONTAINER, FLAG_HI_SLOT_FIRST};
    use crate::dao::items::{creation, parent_locks, pending_reloads, ship_configuration::{self, Patch}};
    use crate::db::outbox::{Effect, Notification, Outbox};
    use crate::services::combat::reload_recovery::TakePreparation;
    use crate::services::identity::Identity;
    use crate::services::inventory::{access::Container, destination::Custody, holds, movement, notifications::Audience, reachability::{Endpoint, Reachability}};
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    use tokio::sync::Barrier;

    let f = setup().await;
    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch { maintenance_corp: Some(true), ..Patch::default() }).await.unwrap();
    let fit = crate::services::fit::Fit::load_unpiloted(&f.db, f.client.sde.clone(), f.pilot).await.unwrap();
    let capacity = holds::effective_capacity(&fit, FLAG_SHIP_HANGAR);
    let small_ship = f.client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
        && holds::accepts(&f.client.sde, FLAG_SHIP_HANGAR, ty.type_id)
        && ty.volume.is_some_and(|volume| volume.is_finite() && volume > 0.0 && volume < capacity)).unwrap().type_id;
    let box_type = f.client.sde.types.iter().find(|ty| ty.group_id == Some(GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let module_type = f.client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE)).unwrap().type_id;
    let root = creation::rookie_ship(&f.db, small_ship, f.actor.character, f.host, FLAG_SHIP_HANGAR).await.unwrap();
    let box_id = creation::rookie_ship(&f.db, box_type, f.actor.character, root, FLAG_CARGO).await.unwrap();
    let quantity = transfers::insert(&f.db, f.commodity, f.actor.character, box_id, FLAG_CARGO, 5).await.unwrap();
    let guest_ship = creation::rookie_ship(&f.db, small_ship, f.owner.character, root, FLAG_CARGO).await.unwrap();
    let module = creation::rookie_ship(&f.db, module_type, f.owner.character, guest_ship, FLAG_HI_SLOT_FIRST).await.unwrap();
    let private_stack = transfers::insert(&f.db, f.commodity, f.owner.character, f.host, FLAG_CARGO, 2).await.unwrap();
    let reload = pending_reloads::insert(&f.db, f.owner.character, guest_ship, module, FLAG_HI_SLOT_FIRST, f.commodity, 3, FLAG_CARGO, f.host).await.unwrap();
    let outside_ids = [f.host, f.pilot, f.stored, f.mine, f.theirs, f.private, f.own_cargo];
    let outside_before: Vec<_> = load_items(&f.db, &f.client.sde, ItemQuery::Ids(&outside_ids)).await.unwrap().iter().map(Item::row).collect();

    let item = load_items(&f.db, &f.client.sde, ItemQuery::Id(root)).await.unwrap().pop().unwrap();
    let identity = Identity::of_client(&f.client);
    let source = Endpoint { location: f.host, flag: FLAG_SHIP_HANGAR };
    let destination = Endpoint { location: f.pilot, flag: FLAG_SHIP_HANGAR };
    let reach = Reachability::capture(&identity, &f.db, &f.client.world, Some(f.system), &[source, destination]).await.unwrap();
    let preparation = TakePreparation::capture(&f.db, &f.client.sde, root).await.unwrap();
    let mut endpoints = vec![source, destination];
    endpoints.extend_from_slice(preparation.endpoints());
    let audience = Audience::capture(&identity, &f.db, &f.client.world, Some(f.system), &endpoints).await.unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let producer_attempts = Arc::new(AtomicUsize::new(0));
    let take_attempts = Arc::new(AtomicUsize::new(0));
    let take_dispatches = Arc::new(AtomicUsize::new(0));
    let dispatch_count = take_dispatches.clone();
    let producer_db = f.db.clone();
    let take_db = f.db.clone();
    let producer_barrier = barrier.clone();
    let take_barrier = barrier.clone();
    let producer_count = producer_attempts.clone();
    let take_count = take_attempts.clone();
    let sde = f.client.sde.clone();
    let actor = f.actor.character;
    let pilot = f.pilot;
    let commodity = f.commodity;
    let producer = producer_db.transaction(
        move |tx, _: &mut ()| {
            let attempt = producer_count.fetch_add(1, Ordering::SeqCst);
            let barrier = producer_barrier.clone();
            Box::pin(async move {
                let _quantity_lock = parent_locks::lock(tx, &[], &[parent_locks::LockRequest { item_id: quantity, strength: parent_locks::LockStrength::Update }]).await.map_err(db_err)?;
                if attempt == 0 { barrier.wait().await; }
                let parents = parent_locks::lock(tx, &[], &[parent_locks::LockRequest { item_id: box_id, strength: parent_locks::LockStrength::Share }]).await.map_err(db_err)?;
                if !parents.rows().iter().any(|parent| parent.item_id == box_id && parent.owner_id == actor && parent.location_id == root) {
                    return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                }
                let change = QuantityChange { type_id: commodity, expected_quantity: 5, quantity: Some(4), owner_id: actor, location_id: box_id, flag_id: FLAG_CARGO, singleton: false };
                if !quantities::apply(tx, &std::collections::BTreeMap::from([(quantity, change)])).await.map_err(db_err)? {
                    return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY));
                }
                transfers::insert(tx, commodity, actor, box_id, FLAG_CARGO, 1).await.map_err(db_err)
            })
        },
        |id, ()| async move { Ok::<_, CallError>(id) },
    );
    let take = take_db.transaction(
        move |tx, outbox: &mut Outbox| {
            let attempt = take_count.fetch_add(1, Ordering::SeqCst);
            let barrier = take_barrier.clone();
            let mut preparation = preparation.fresh_attempt();
            let reach = reach.clone();
            let audience = audience.clone();
            let sde = sde.clone();
            let item = item.clone();
            Box::pin(async move {
                if !ship::lock_reload_ship(tx, box_id).await.map_err(db_err)? {
                    return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                }
                if attempt == 0 { barrier.wait().await; }
                let ancestors = parent_locks::capture(tx, preparation.ancestors()).await.map_err(db_err)?;
                let mut requests = crate::services::inventory::destination::parent_requests(&sde,
                    ancestors.iter().map(|row| (row.item_id, row.type_id)), parent_locks::LockStrength::Update, parent_locks::LockStrength::Share)?;
                requests.extend(preparation.closure().iter().chain([pilot].iter())
                    .map(|&item_id| parent_locks::LockRequest { item_id, strength: parent_locks::LockStrength::Update }));
                let mut mandatory: Vec<_> = requests.iter().map(|request| request.item_id).collect();
                mandatory.extend(preparation.refund().mandatory_ids());
                requests.extend_from_slice(preparation.refund().requests());
                let optional: Vec<_> = preparation.refund().candidate_ids().into_iter().filter(|id| !mandatory.contains(id)).collect();
                let locked = crate::services::inventory::destination::lock_items(tx, &sde, &requests, &optional).await?;
                if preparation.closure().iter().any(|id| !locked.is_write_locked(*id)) {
                    return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
                }
                preparation.revalidate(tx).await?;
                let container = Container::Item(pilot);
                let (destination, custody) = movement::authorize_move(tx, &sde, &identity, &reach, &item, Some(source.location), &container, Some(FLAG_SHIP_HANGAR)).await?
                    .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                if custody != Custody::TakeSubtree { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
                let audience = audience.resolve(tx, &sde).await?;
                preparation.revalidate(tx).await?;
                movement::move_core(tx, &sde, outbox, &item, &destination, None, custody, &locked, Some(&mut preparation), Some(&audience), actor).await?
                    .ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))
            })
        },
        move |id, outbox: Outbox| async move {
            dispatch_count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CallError>((id, outbox.into_entries()))
        },
    );
    let (produced, taken) = tokio::join!(producer, take);
    let produced = produced.unwrap();
    assert_eq!(producer_attempts.load(Ordering::SeqCst) + take_attempts.load(Ordering::SeqCst), 3);
    let outside_after: Vec<_> = load_items(&f.db, &f.client.sde, ItemQuery::Ids(&outside_ids)).await.unwrap().iter().map(Item::row).collect();
    assert_eq!(outside_after, outside_before);
    let rows = load_items(&f.db, &f.client.sde, ItemQuery::Ids(&[root, box_id, quantity, produced, guest_ship, module, private_stack])).await.unwrap();
    assert_eq!(rows.len(), 7);
    let row = |id| rows.iter().find(|item| item.item_id == id).unwrap();
    assert_eq!((row(box_id).owner_id, row(box_id).location_id, row(box_id).flag_id), (actor, root, FLAG_CARGO));
    assert_eq!((row(private_stack).owner_id, row(private_stack).location_id, row(private_stack).flag_id), (f.owner.character, f.host, FLAG_CARGO));
    assert_eq!((row(root).quantity, row(root).singleton, row(box_id).quantity, row(box_id).singleton,
        row(guest_ship).quantity, row(guest_ship).singleton, row(module).quantity, row(module).singleton),
        (1, true, 1, true, 1, true, 1, true));
    assert_eq!((row(quantity).owner_id, row(quantity).location_id, row(quantity).flag_id, row(quantity).quantity, row(quantity).singleton), (actor, box_id, FLAG_CARGO, 4, false));
    assert_eq!((row(produced).owner_id, row(produced).location_id, row(produced).flag_id, row(produced).quantity, row(produced).singleton), (actor, box_id, FLAG_CARGO, 1, false));
    match taken {
        Ok((moved, notices)) => {
            assert_eq!((producer_attempts.load(Ordering::SeqCst), take_attempts.load(Ordering::SeqCst)), (2, 1));
            assert_eq!(take_dispatches.load(Ordering::SeqCst), 1);
            assert_eq!(moved, root);
            assert_eq!((row(root).owner_id, row(root).location_id, row(root).flag_id), (actor, pilot, FLAG_SHIP_HANGAR));
            assert_eq!((row(guest_ship).owner_id, row(guest_ship).location_id, row(guest_ship).flag_id), (actor, root, FLAG_CARGO));
            assert_eq!((row(module).owner_id, row(module).location_id, row(module).flag_id), (actor, guest_ship, FLAG_HI_SLOT_FIRST));
            assert_eq!(row(private_stack).quantity, 5);
            assert!(pending_reloads::for_source(&f.db, f.host).await.unwrap().is_empty());
            let private_row = row(private_stack).row();
            assert!(notices.iter().any(|entry| matches!(entry, Effect::Notification(Notification::Pilot { char_id, event, args }) if *char_id == f.owner.character && event == "OnItemChange" && args.first() == Some(&private_row))));
            assert!(!notices.iter().any(|entry| matches!(entry, Effect::Notification(Notification::DirectClient { args, .. }) if args.first() == Some(&private_row))));
            assert!(!notices.iter().any(|entry| matches!(entry, Effect::Notification(Notification::Pilot { char_id, args, .. }) if *char_id == actor && args.first() == Some(&private_row))));
        }
        Err(CallError::User { .. }) => {
            assert_eq!((producer_attempts.load(Ordering::SeqCst), take_attempts.load(Ordering::SeqCst)), (1, 2));
            assert_eq!(take_dispatches.load(Ordering::SeqCst), 0);
            assert_eq!((row(root).owner_id, row(root).location_id, row(root).flag_id), (actor, f.host, FLAG_SHIP_HANGAR));
            assert_eq!((row(guest_ship).owner_id, row(guest_ship).location_id, row(guest_ship).flag_id), (f.owner.character, root, FLAG_CARGO));
            assert_eq!((row(module).owner_id, row(module).location_id, row(module).flag_id), (f.owner.character, guest_ship, FLAG_HI_SLOT_FIRST));
            assert_eq!(row(private_stack).quantity, 2);
            let pending = pending_reloads::for_source(&f.db, f.host).await.unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!((pending[0].reload_id, pending[0].owner_id, pending[0].ship_id, pending[0].module_id,
                pending[0].flag_id, pending[0].type_id, pending[0].quantity, pending[0].source_flag, pending[0].source_location_id),
                (reload, f.owner.character, guest_ship, module, FLAG_HI_SLOT_FIRST, commodity, 3, FLAG_CARGO, f.host));
        }
        Err(error) => panic!("unexpected take error: {error:?}"),
    }
    pending_reloads::take(&f.db, reload).await.unwrap();
    for id in [produced, quantity, box_id, module, guest_ship, root, private_stack] {
        item_inventory::delete(&f.db, id).await.unwrap();
    }
    cleanup(f).await;
}
