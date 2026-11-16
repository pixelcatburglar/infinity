//! Unlimited-cargo server integration test: GM refusals, two legal M-sized
//! deposits, one clipped Q move, reset refusal and withdrawal, the C wire
//! representation and its finite restore, all inside one fixture lifecycle.

use crate::services::tests::fixture;
use crate::services::{Call, CallError, CallResult};
use crate::services::slash::tests::support::rpc;
use crate::services::inventory::{CONTAINER_HANGAR, INVENTORY};
use infinity_core::consts::{CLIENT_MAX_SHORT_AMOUNT, FLAG_CARGO, FLAG_HANGAR, GM_ROLES, MAX_STACK_QUANTITY};
use marshal::Value;

fn dec(v: i64) -> Value { Value::Int(v) }
fn unit(sde: &crate::sde::Sde, type_id: i32) -> f64 {
    crate::services::item_volume::item_volume(sde, type_id, false, 1, None)
        .unwrap_or_else(|e| panic!("item volume for type {type_id}: {e:?}"))
}
fn info_capacity(info: &Value) -> Option<f64> {
    let Value::Instance(_, state) = info else { return None };
    let Value::Dict(attrs) = state.get("attributes")? else { return None };
    attrs.iter().find(|(k, _)| k.as_int() == Some(i64::from(infinity_core::consts::dogma::ATTR_CAPACITY)))
        .and_then(|(_, v)| v.as_float())
}

async fn svc(client: &mut crate::client::Client, service: &str, method: &str, args: &[Value], kwargs: &Value, bind: Option<&Value>) -> CallResult {
    crate::services::dispatch(client, &Call { service, method, args, kwargs, bound: bind.is_some(), bind_params: bind }).await
}
async fn add(client: &mut crate::client::Client, bind: &Value, item: i64, source: i64, flag: i32, qty: i64) -> CallResult {
    let kwargs = Value::Dict(vec![(Value::unicode("flag"), dec(i64::from(flag))), (Value::unicode("qty"), dec(qty))]);
    svc(client, INVENTORY, "Add", &[dec(item), dec(source)], &kwargs, Some(bind)).await
}
async fn capacity_of(client: &mut crate::client::Client, owner: i64, ship: i64, bind: &Value) -> (crate::services::inventory::holds::CapacityLimit, f64, f64) {
    let reply = svc(client, INVENTORY, "GetCapacity", &[], &Value::Dict(vec![]), Some(bind)).await.unwrap();
    let Value::Instance(_, state) = reply else { panic!("capacity keyval") };
    let cap = state.get("capacity").and_then(Value::as_float).expect("capacity");
    let used = state.get("used").and_then(Value::as_float).expect("used");
    let (limit, server_used, _) = crate::services::inventory::hold_storage::capacity(&client.db, &client.sde, owner, ship, FLAG_CARGO).await.unwrap().unwrap();
    assert_eq!(server_used, used);
    let item_info = svc(client, "dogmaIM", "ItemGetInfo", &[dec(ship)], &Value::Dict(vec![]), None).await.unwrap();
    assert_eq!(info_capacity(&item_info), Some(cap));
    let ship_info = svc(client, "dogmaIM", "ShipGetInfo", &[], &Value::Dict(vec![]), None).await.unwrap();
    let Value::Dict(entries) = &ship_info else { panic!("ShipGetInfo must return a dict") };
    assert_eq!(entries.iter().find(|(k, _)| k.as_int() == Some(ship)).and_then(|(_, v)| info_capacity(v)), Some(cap));
    (limit, cap, used)
}
async fn assert_row(db: &crate::db::Db, item: i64, owner: i64, location: i64, flag: i32, type_id: i32, quantity: i64) {
    let row = db.query_one("SELECT owner_id,location_id,flag_id,type_id,quantity FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
    assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"),
        row.get::<_, i32>("type_id"), row.get::<_, i64>("quantity")),
        (owner, location, flag, type_id, quantity), "row {item} fields");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn unlimited_cargo_preserves_exact_stacks_and_restores_finite_admission() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let mut cleanup_world = None;
    let result = crate::tests::catch_scenario(async {
        let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
        cleanup_world = Some(client.world.clone());
        let sde = client.sde.clone();
        let station = sde.map.stations().next().unwrap();
        let station_id = station.station_id;
        // Select the smallest ship with positive real static capacity;
        // the live Fit must agree on the same finite limit.
        let (ship_type, base) = sde.types.iter()
            .filter(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP))
            .filter_map(|ty| crate::services::inventory::hold_storage::static_capacity(&sde, ty.type_id).ok()
                .filter(|v| v.is_finite() && *v > 0.0).map(|v| (ty.type_id, v)))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .expect("SDE must contain a ship with cargo capacity");
        let ship = crate::dao::items::creation::rookie_ship(&db, ship_type, actor.character, station_id, FLAG_HANGAR).await.unwrap();
        let fit = crate::services::fit::Fit::load_unpiloted(&db, sde.clone(), ship).await.unwrap();
        assert!(matches!(crate::services::inventory::holds::effective_capacity(&fit, FLAG_CARGO),
            crate::services::inventory::holds::CapacityLimit::Finite(v) if (v - base).abs() < f64::EPSILON.max(base * 1e-9)),
            "live fit must confirm the static finite capacity");
        let m = MAX_STACK_QUANTITY;
        let q = crate::services::dogma::info::cap_client_quantity(m);
        assert_eq!(q, CLIENT_MAX_SHORT_AMOUNT);
        // Admission is decided by the production check at the real M quantity:
        // `big` is the largest legal positive-volume cargo type and `small` the
        // smallest whose single unit honestly fits the finite quota.
        let (mut big, mut small): (Option<(i32, f64)>, Option<(i32, f64)>) = (None, None);
        for ty in sde.types.iter() {
            if crate::services::inventory::holds::check(&sde, FLAG_CARGO, ty.type_id, false, m,
                crate::services::inventory::holds::CapacityLimit::Unlimited, 0.0).is_err() { continue; }
            let u = unit(&sde, ty.type_id);
            if !u.is_finite() || u <= 0.0 { continue; }
            if big.is_none_or(|(_, volume)| u > volume) { big = Some((ty.type_id, u)); }
            if u <= base && small.is_none_or(|(_, volume)| u < volume) {
                small = Some((ty.type_id, u));
            }
        }
        let big = big.expect("SDE must contain a positive-volume cargo type");
        let small = small.expect("SDE must contain a cargo type whose unit fits the hull");
        assert!(big.1 * q as f64 > base, "big-stack move must exceed the finite quota");
        let a = crate::dao::items::transfers::insert(&db, big.0, actor.character, station_id, FLAG_HANGAR, m).await.unwrap();
        let r = crate::dao::items::transfers::insert(&db, small.0, actor.character, station_id, FLAG_HANGAR, 2).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = tx.clone();
        client.world.register_pilot(actor.character, tx);
        client.session.apply(vec![("charid", Value::Int(actor.character)), ("shipid", Value::Int(ship)), ("stationid", Value::Int(station_id))]);
        crate::dao::characters::selection::set_ship(&db, actor.character, ship).await.unwrap();
        client.login.user_id = actor.account;
        let ship_bind = Value::list([dec(ship)]);
        let hangar_bind = Value::list([dec(station_id), dec(CONTAINER_HANGAR), dec(actor.character)]);
        // Control prelude: role, attribute, target and flag refusals come first.
        client.login.role = 0;
        assert!(rpc(&mut client, "/dogma ship capacity infinity").await.is_err(), "non-GM refused");
        client.login.role = GM_ROLES;
        assert!(rpc(&mut client, "/dogma ship mass infinity").await.is_err(), "non-capacity infinity refused");
        assert!(rpc(&mut client, &format!("/dogma {r} capacity infinity")).await.is_err(), "non-ship item refused");
        assert!(rpc(&mut client, "/dogma flag:1 capacity infinity").await.is_err(), "flag target refused");
        assert!(crate::dao::items::dogma::overrides(&db, ship).await.unwrap().is_empty());
        // Finite state: GetCapacity shows the real finite capacity; one unit of
        // R moves in through the real Add reply and its source keeps 1.
        let (limit, cap, used) = capacity_of(&mut client, actor.character, ship, &ship_bind).await;
        assert!(matches!(limit, crate::services::inventory::holds::CapacityLimit::Finite(v) if v == cap));
        assert!(cap.is_finite() && cap > 0.0 && used == 0.0, "finite baseline {cap}/{used}");
        let Ok(Value::Int(r_moved)) = add(&mut client, &ship_bind, r, station_id, FLAG_CARGO, 1).await else {
            panic!("finite R add must return the moved row id");
        };
        assert_row(&db, r_moved, actor.character, ship, FLAG_CARGO, small.0, 1).await;
        assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&r]).await.unwrap().get::<_, i64>("quantity"), 1);
        // Command lifecycle while unlimited: query and list see the mode, the
        // repeat write is a no-op, a same-base finite set clears the mode, and
        // the numeric attribute route resolves identically to the name route.
        let reply = rpc(&mut client, "/dogma ship capacity infinity").await.unwrap();
        assert!(reply.as_text().unwrap().contains(infinity_core::messages::gm::GM_DOGMA_UNLIMITED));
        let fit = crate::services::fit::Fit::load_unpiloted(&db, sde.clone(), ship).await.unwrap();
        assert!(fit.cargo_unlimited());
        assert!(matches!(crate::services::inventory::holds::effective_capacity(&fit, FLAG_CARGO),
            crate::services::inventory::holds::CapacityLimit::Unlimited));
        assert!(rpc(&mut client, "/dogma ship capacity").await.unwrap().as_text().unwrap()
            .contains(infinity_core::messages::gm::GM_DOGMA_UNLIMITED));
        let mass = rpc(&mut client, "/dogma ship mass").await.unwrap();
        assert!(!mass.as_text().unwrap().contains(infinity_core::messages::gm::GM_DOGMA_UNLIMITED));
        assert!(rpc(&mut client, "/dogma ship list").await.unwrap().as_text().unwrap()
            .contains(&infinity_core::consts::dogma::ATTR_CAPACITY.to_string()), "list must show the capacity override");
        while rx.try_recv().is_ok() {}
        rpc(&mut client, &format!("/dogma ship {} infinity", infinity_core::consts::dogma::ATTR_CAPACITY)).await.unwrap();
        assert!(rx.try_recv().is_err(), "repeating infinity must not publish");
        assert_eq!(crate::dao::items::dogma::overrides(&db, ship).await.unwrap().iter()
            .filter(|row| row.attribute_id == infinity_core::consts::dogma::ATTR_CAPACITY).count(), 1,
            "repeat infinity through the numeric route stays a no-op");
        let persisted = crate::dao::items::dogma::hull_override(&db, ship, infinity_core::consts::dogma::ATTR_CAPACITY).await.unwrap().unwrap();
        let base_value = persisted.state.unwrap().value;
        rpc(&mut client, &format!("/dogma ship capacity {base_value}")).await.unwrap();
        assert_eq!(crate::dao::items::dogma::hull_override(&db, ship, infinity_core::consts::dogma::ATTR_CAPACITY).await.unwrap().unwrap().state,
            Some(crate::dao::items::dogma::OverrideState { value: base_value, cargo_unlimited: false }),
            "a same-base finite set clears the mode");
        rpc(&mut client, "/dogma ship capacity infinity").await.unwrap();
        // Two legal M deposits through real transactions; each receipt resolves
        // to a distinct real row of quantity M owned in the cargo flag.
        let world = client.world.clone();
        let sender = client.notify_tx.clone();
        let mut deposited = Vec::new();
        for _ in 0..2 {
            let receipt = db.transaction(
                |tx, outbox: &mut crate::db::outbox::Outbox| {
                    let sde = sde.clone();
                    let destination = crate::services::inventory::destination::Destination::owned(actor.character, ship, FLAG_CARGO);
                    Box::pin(async move { crate::services::items::deposit(tx, &sde, outbox, &destination, big.0, m, 0).await })
                },
                |value, outbox| {
                    let (world, sender) = (world.clone(), sender.clone());
                    async move { crate::db::outbox::dispatch_sender(value, outbox, &world, Some(&sender)) }
                },
            ).await.unwrap();
            let row = receipt.last();
            assert_eq!(row.quantity, m);
            deposited.push(row.item_id);
        }
        assert_ne!(deposited[0], deposited[1], "two M stacks stay distinct rows");
        for id in &deposited {
            assert_row(&db, *id, actor.character, ship, FLAG_CARGO, big.0, m).await;
        }
        // The Q-clipped move through the real Add reply leaves M−Q on the source.
        let Ok(Value::Int(moved)) = add(&mut client, &ship_bind, a, station_id, FLAG_CARGO, q).await else {
            panic!("Q add must return the moved row id");
        };
        assert_row(&db, moved, actor.character, ship, FLAG_CARGO, big.0, q).await;
        assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&a]).await.unwrap().get::<_, i64>("quantity"), m - q);
        let cargo_total: i128 = db.query("SELECT quantity FROM game.items WHERE location_id=$1 AND flag_id=$2 AND type_id=$3",
            &[&ship, &FLAG_CARGO, &big.0]).await.unwrap()
            .into_iter().map(|r| i128::from(r.get::<_, i64>("quantity"))).sum();
        assert_eq!(cargo_total, i128::from(m) * 2 + i128::from(q), "two legal M deposits plus the Q move conserve volume");
        // All three capacity surfaces report the wire constant and the
        // production used agrees with the RPC reply while the mode is on.
        let (limit, cap, used) = capacity_of(&mut client, actor.character, ship, &ship_bind).await;
        assert_eq!(cap, crate::services::fit::cargo::UNLIMITED_CARGO_WIRE_CAPACITY);
        assert!(matches!(limit, crate::services::inventory::holds::CapacityLimit::Unlimited));
        assert!(used.is_finite() && used > base, "two-M occupancy exceeds finite quota: {used}");
        // The republished brain carries exactly one terminal ship-target
        // capacity POST_ASSIGN with the wire capacity, not an event name alone.
        let capacity_effect = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>| -> Vec<(i64, f64)> {
            let args = std::iter::from_fn(|| rx.try_recv().ok())
                .filter_map(|(event, args)| (event == crate::services::dogma::brain::UPDATED_EVENT).then_some(args)).last().expect("brain update");
            let Value::Tuple(envelope) = args.first().expect("brain envelope") else { panic!("brain envelope tuple") };
            let Value::Bytes(bytes) = envelope.get(1).expect("brain bytes") else { panic!("brain bytes") };
            let Value::Tuple(inner) = marshal::loads(bytes).expect("brain marshal") else { panic!("brain tuple") };
            let Value::List(ship_effects) = inner.get(1).expect("ship effects") else { panic!("ship effects list") };
            ship_effects.iter().filter_map(|effect| {
                let Value::Reduce { data, .. } = effect else { panic!("brain effect") };
                let Value::Tuple(parts) = data.as_ref() else { panic!("brain effect tuple") };
                let state = parts.get(2).expect("brain effect state");
                let target = state.get("toItemID").and_then(Value::as_int).expect("brain target");
                let attribute = state.get("toAttribID").and_then(Value::as_int).expect("brain attribute");
                let operation = state.get("operation").and_then(Value::as_int).expect("brain operation");
                let value = state.get("value").and_then(Value::as_float).expect("brain value");
                (target == ship && attribute == i64::from(infinity_core::consts::dogma::ATTR_CAPACITY)).then_some((operation, value))
            }).collect()
        };
        let effects = capacity_effect(&mut rx);
        let assign = i64::from(crate::services::fit::OP_POST_ASSIGN);
        assert_eq!(effects.iter().filter(|(op, _)| *op == assign).count(), 1);
        assert_eq!(effects.last(), Some(&(assign, crate::services::fit::cargo::UNLIMITED_CARGO_WIRE_CAPACITY)));
        // Reset clears the mode, restores finite admission, republishes a brain
        // without the terminal effect, keeps the goods and still lets an
        // explicit qty=1 withdrawal out as its own row.
        rpc(&mut client, "/dogma ship capacity reset").await.unwrap();
        assert!(crate::dao::items::dogma::overrides(&db, ship).await.unwrap().iter().all(|r| r.attribute_id != infinity_core::consts::dogma::ATTR_CAPACITY));
        assert!(capacity_effect(&mut rx).iter().all(|(op, _)| *op != assign),
            "the finite brain carries no terminal capacity POST_ASSIGN");
        let (limit, cap, used) = capacity_of(&mut client, actor.character, ship, &ship_bind).await;
        assert!(matches!(limit, crate::services::inventory::holds::CapacityLimit::Finite(v) if v == cap));
        assert!(cap.is_finite() && cap > 0.0 && used > cap, "restored finite quota below honest used {cap}/{used}");
        let refused = add(&mut client, &ship_bind, r, station_id, FLAG_CARGO, 1).await;
        match refused {
            Err(CallError::User { msg, .. }) => assert_eq!(msg, infinity_core::messages::MSG_NOT_ENOUGH_CARGO_SPACE),
            other => panic!("post-reset add must refuse the exact quota: {other:?}"),
        }
        assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&r]).await.unwrap().get::<_, i64>("quantity"), 1);
        let Ok(Value::Int(withdrawn)) = add(&mut client, &hangar_bind, deposited[0], ship, FLAG_HANGAR, 1).await else {
            panic!("qty-1 withdrawal must return the new hangar row id");
        };
        assert_row(&db, withdrawn, actor.character, station_id, FLAG_HANGAR, big.0, 1).await;
        assert!(withdrawn != deposited[0], "the withdrawn unit lands as its own hangar stack");
        assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&deposited[0]]).await.unwrap().get::<_, i64>("quantity"), m - 1);
        let (limit, restored_cap, used) = capacity_of(&mut client, actor.character, ship, &ship_bind).await;
        assert!(matches!(limit, crate::services::inventory::holds::CapacityLimit::Finite(v) if v == restored_cap && v == cap));
        let fit = crate::services::fit::Fit::load_unpiloted(&db, sde.clone(), ship).await.unwrap();
        assert!(matches!(crate::services::inventory::holds::effective_capacity(&fit, FLAG_CARGO),
            crate::services::inventory::holds::CapacityLimit::Finite(v) if v > 0.0));
        assert!(used.is_finite(), "used stays an honest finite value after restore");
        while rx.try_recv().is_ok() {}
    }).await;
    if let Some(world) = cleanup_world { world.unregister_pilot_generation(actor.character, None); }
    fixture::cleanup(&db, actor).await.unwrap();
    result.expect("unlimited cargo flow failed");
}
