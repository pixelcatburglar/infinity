//! Mutation admission and original-input snapshots for ordinary containers.
use super::*;
use crate::services::{inventory::hold_storage, item_volume::item_volume};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with loaded SDE; run serially"]
async fn stale_original_input_metadata_is_not_recaptured() {
    let f = Arc::new(Fixture::new().await);
    let scenario = f.clone();
    let outcome = tokio::spawn(async move {
        let f = scenario;
        for id in [f.mutator, f.source] {
            for column in ["type_id", "owner_id", "location_id", "quantity"] {
                let original_mutator = crate::services::items::load_items(&f.db, &f.sde, crate::dao::items::loading::ItemQuery::Id(f.mutator)).await.unwrap().into_iter().next().unwrap();
                let original_source = crate::services::items::load_items(&f.db, &f.sde, crate::dao::items::loading::ItemQuery::Id(f.source)).await.unwrap().into_iter().next().unwrap();
                let draws = f.draws();
                let update = format!("UPDATE game.items SET {column}={column}+1 WHERE item_id=$1");
                f.db.execute(&update, &[&id]).await.unwrap();
                let before = f.fingerprint().await;
                let dispatched = Arc::new(AtomicBool::new(false));
                let sent = dispatched.clone();
                let sde = f.sde.clone();
                let context = f.context();
                let result = f.db.transaction(move |tx, outbox: &mut Outbox| {
                    let sde = sde.clone(); let context = context.clone(); let draws = draws.clone();
                    let mutator = original_mutator.clone(); let source = original_source.clone();
                    Box::pin(async move { mutation::create(tx, &sde, outbox, &context, &mutator, &source, &draws).await })
                }, move |id, _| async move { sent.store(true, Ordering::SeqCst); Ok(id) }).await;
                assert!(matches!(result, Err(CallError::User { .. })));
                assert_eq!(f.fingerprint().await, before);
                assert!(!dispatched.load(Ordering::SeqCst));
                let restore = format!("UPDATE game.items SET {column}={column}-1 WHERE item_id=$1");
                f.db.execute(&restore, &[&id]).await.unwrap();
            }
        }
    }).await;
    f.cleanup().await;
    outcome.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with loaded SDE; run serially"]
async fn same_box_overfull_exchange_keeps_remnant_and_singleton_result() {
    same_box_exchange(0).await;
    same_box_exchange(1).await;
    same_box_exchange(2).await;
}

async fn same_box_exchange(mode: u8) {
    let mut fixture = Fixture::new().await;
    let mapping = fixture.sde.types.iter().find_map(|ty| {
        let mutation = fixture.sde.dynamic_items.get(ty.type_id)?;
        let mutator = item_volume(&fixture.sde, ty.type_id, false, 1, None).ok()?;
        mutation.mappings.iter().find_map(|(&source, &result)| {
            let source_volume = item_volume(&fixture.sde, source, true, 1, None).ok()?;
            let result_volume = item_volume(&fixture.sde, result, true, 1, None).ok()?;
            if result_volume > source_volume + mutator { return None; }
            if mode != 2 { return Some((ty.type_id, source, result, None)); }
            if mutator <= 0.0 { return None; }
            fixture.sde.types.iter().find_map(|box_type| {
                if !hold_storage::is_ordinary_container(&fixture.sde, box_type.type_id) { return None; }
                let capacity = hold_storage::static_capacity(&fixture.sde, box_type.type_id).ok()?;
                let q = (capacity - source_volume) / mutator;
                if !capacity.is_finite() || capacity <= 0.0 || q < 2.0 || q > f64::from(i32::MAX) || q.fract() != 0.0 { return None; }
                let quantity = q as i64;
                let stack = item_volume(&fixture.sde, ty.type_id, false, quantity, None).ok()?;
                (source_volume + stack == capacity).then_some((ty.type_id, source, result, Some((box_type.type_id, quantity))))
            })
        })
    }).expect("loaded SDE non-increasing mutation and, for full case, exact positive ordinary capacity");
    fixture.mutator_type = mapping.0;
    fixture.source_type = mapping.1;
    fixture.result_type = mapping.2;
    fixture.db.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1", &[&fixture.mutator, &mapping.0]).await.unwrap();
    fixture.db.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1", &[&fixture.source, &mapping.1]).await.unwrap();
    let f = Arc::new(fixture);
    let scenario = f.clone();
    let outcome = tokio::spawn(async move {
        let f = scenario;
        let box_type = mapping.3.map(|pair| pair.0).unwrap_or_else(|| f.sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&f.sde, ty.type_id)
            && hold_storage::static_capacity(&f.sde, ty.type_id).is_ok_and(|capacity| if mode == 1 { capacity > 0.0 } else { capacity == 0.0 }))
            .expect("ordinary container with requested capacity in loaded SDE").type_id);
        let result_volume = item_volume(&f.sde, f.result_type, true, 1, None).ok().unwrap();
        let source_volume = item_volume(&f.sde, f.source_type, true, 1, None).ok().unwrap();
        let mutator_volume = item_volume(&f.sde, f.mutator_type, false, 1, None).ok().unwrap();
        assert!(result_volume <= source_volume + mutator_volume);
        let parent: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,2,true) RETURNING item_id", &[&box_type, &f.owner, &f.station, &FLAG_HANGAR]).await.unwrap().get("item_id");
        f.db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=ANY($1)", &[&vec![f.mutator, f.source], &parent, &FLAG_CARGO]).await.unwrap();
        let quantity = mapping.3.map_or(3, |pair| pair.1);
        f.db.execute("UPDATE game.items SET quantity=$2 WHERE item_id=$1", &[&f.mutator, &quantity]).await.unwrap();
        let filler = if mode == 1 {
            let capacity = hold_storage::static_capacity(&f.sde, box_type).unwrap();
            assert!(mutator_volume > 0.0, "positive mutator volume required for filler");
            let quantity = (capacity / mutator_volume).ceil() as i64;
            Some(f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,false) RETURNING item_id", &[&f.mutator_type, &f.owner, &parent, &FLAG_CARGO, &quantity]).await.unwrap().get::<_, i64>("item_id"))
        } else { None };
        let snapshot = crate::dao::items::loading::capacity_snapshot(&f.db, parent).await.unwrap();
        let room = hold_storage::container_room(&f.sde, snapshot.parent.as_ref().unwrap(), &snapshot.items, &snapshot.reloads).unwrap().unwrap();
        let before_used = room.used;
        if mode == 2 { assert!(room.capacity > 0.0); assert_eq!(room.used, room.capacity); }
        if mode == 1 { assert!(room.used > room.capacity); }
        if mode == 0 { assert_eq!(room.capacity, 0.0); }
        let dispatched = Arc::new(AtomicBool::new(false));
        let before = f.fingerprint().await;
        assert!(matches!(f.create(f.context(), f.mutator, f.source, true, dispatched.clone()).await, Err(CallError::Internal(_))));
        assert_eq!(f.fingerprint().await, before);
        assert!(!dispatched.load(Ordering::SeqCst));
        let result = f.create(f.context(), f.mutator, f.source, false, dispatched.clone()).await.ok().unwrap();
        assert!(dispatched.load(Ordering::SeqCst));
        assert!(f.db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&f.source]).await.unwrap().is_none());
        let remnant = f.db.query_one("SELECT quantity,location_id FROM game.items WHERE item_id=$1", &[&f.mutator]).await.unwrap();
        assert_eq!((remnant.get::<_, i64>("quantity"), remnant.get::<_, i64>("location_id")), (quantity - 1, parent));
        let created = f.db.query_one("SELECT type_id,location_id,quantity,singleton FROM game.items WHERE item_id=$1", &[&result]).await.unwrap();
        assert_eq!((created.get::<_, i32>("type_id"), created.get::<_, i64>("location_id"), created.get::<_, i64>("quantity"), created.get::<_, bool>("singleton")), (f.result_type, parent, 1, true));
        let provenance = dynamic_items::get(&f.db, result).await.unwrap().unwrap();
        assert_eq!((provenance.source_item_id, provenance.mutator_item_id), (f.source, f.mutator));
        let snapshot = crate::dao::items::loading::capacity_snapshot(&f.db, parent).await.unwrap();
        let after = hold_storage::container_room(&f.sde, snapshot.parent.as_ref().unwrap(), &snapshot.items, &snapshot.reloads).unwrap().unwrap();
        assert_eq!(after.used, before_used - source_volume - mutator_volume + result_volume);
        assert!(after.used <= before_used);
        if let Some(filler) = filler {
            let row = f.db.query_one("SELECT quantity,location_id FROM game.items WHERE item_id=$1", &[&filler]).await.unwrap();
            assert!(row.get::<_, i64>("quantity") > 0);
            assert_eq!(row.get::<_, i64>("location_id"), parent);
        }
    }).await;
    f.cleanup().await;
    outcome.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with loaded SDE; run serially"]
async fn separate_source_box_cannot_credit_growth_and_room_permits_it() {
    let mut fixture = Fixture::new().await;
    let mapping = fixture.sde.types.iter().find_map(|ty| {
        let mutation = fixture.sde.dynamic_items.get(ty.type_id)?;
        let consumed = item_volume(&fixture.sde, ty.type_id, false, 1, None).ok()?;
        mutation.mappings.iter().find_map(|(&source, &result)| {
            let result_volume = item_volume(&fixture.sde, result, true, 1, None).ok()?;
            (result_volume > consumed && fixture.sde.types.get(source).is_some())
                .then_some((ty.type_id, source, result, result_volume))
        })
    }).expect("loaded SDE mutation with positive destination growth");
    fixture.mutator_type = mapping.0;
    fixture.source_type = mapping.1;
    fixture.result_type = mapping.2;
    fixture.db.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1", &[&fixture.mutator, &mapping.0]).await.unwrap();
    fixture.db.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1", &[&fixture.source, &mapping.1]).await.unwrap();
    let f = Arc::new(fixture);
    let scenario = f.clone();
    let outcome = tokio::spawn(async move {
        let f = scenario;
        let zero = f.sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&f.sde, ty.type_id)
            && hold_storage::static_capacity(&f.sde, ty.type_id).is_ok_and(|capacity| capacity == 0.0))
            .expect("ordinary zero-capacity type in loaded SDE").type_id;
        let needed = item_volume(&f.sde, mapping.0, false, 2, None).ok().unwrap() + mapping.3;
        let roomy = f.sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&f.sde, ty.type_id)
            && hold_storage::static_capacity(&f.sde, ty.type_id).is_ok_and(|capacity| capacity >= needed))
            .expect("ordinary container with room for result").type_id;
        let insert = "INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id";
        let destination: i64 = f.db.query_one(insert, &[&zero, &f.owner, &f.station, &FLAG_HANGAR]).await.unwrap().get("item_id");
        let source_parent: i64 = f.db.query_one(insert, &[&zero, &f.owner, &f.station, &FLAG_HANGAR]).await.unwrap().get("item_id");
        f.db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&f.mutator, &destination, &FLAG_CARGO]).await.unwrap();
        f.db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&f.source, &source_parent, &FLAG_CARGO]).await.unwrap();
        let dispatched = Arc::new(AtomicBool::new(false));
        let before = f.fingerprint().await;
        assert!(matches!(f.create(f.context(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        assert!(!dispatched.load(Ordering::SeqCst));
        f.db.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1", &[&destination, &roomy]).await.unwrap();
        let capacity = hold_storage::static_capacity(&f.sde, roomy).unwrap();
        let unit = item_volume(&f.sde, mapping.0, false, 1, None).unwrap();
        assert!(capacity > 0.0 && unit > 0.0, "positive capacity and filler volume required");
        let filler_quantity = (capacity / unit).ceil() as i64;
        assert!(filler_quantity > 0 && item_volume(&f.sde, mapping.0, false, filler_quantity, None).unwrap() >= capacity);
        let filler: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,false) RETURNING item_id", &[&mapping.0, &f.owner, &destination, &FLAG_CARGO, &filler_quantity]).await.unwrap().get("item_id");
        let before = f.fingerprint().await;
        assert!(matches!(f.create(f.context(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        assert!(!dispatched.load(Ordering::SeqCst));
        f.db.execute("DELETE FROM game.items WHERE item_id=$1", &[&filler]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(f.context(), f.mutator, f.source, true, dispatched.clone()).await, Err(CallError::Internal(_))));
        assert_eq!(f.fingerprint().await, before);
        assert!(!dispatched.load(Ordering::SeqCst));
        let result = f.create(f.context(), f.mutator, f.source, false, dispatched.clone()).await.ok().unwrap();
        assert!(dispatched.load(Ordering::SeqCst));
        let row = f.db.query_one("SELECT type_id,location_id,singleton FROM game.items WHERE item_id=$1", &[&result]).await.unwrap();
        assert_eq!((row.get::<_, i32>("type_id"), row.get::<_, i64>("location_id"), row.get::<_, bool>("singleton")), (mapping.2, destination, true));
        assert!(f.db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&f.source]).await.unwrap().is_none());
        assert_eq!(f.db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&f.mutator]).await.unwrap().get::<_, i64>("quantity"), 2);
    }).await;
    f.cleanup().await;
    outcome.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with loaded SDE; run serially"]
async fn changing_source_parent_while_mutation_waits_refuses_without_effects() {
    let f = Arc::new(Fixture::new().await);
    let scenario = f.clone();
    let outcome = tokio::spawn(async move {
        let f = scenario;
        let box_type = f.sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&f.sde, ty.type_id)).unwrap().type_id;
        let parent: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&box_type, &f.owner, &f.station, &FLAG_HANGAR]).await.unwrap().get("item_id");
        f.db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&f.source, &parent, &FLAG_CARGO]).await.unwrap();
        let destination: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&box_type, &f.owner, &f.station, &FLAG_HANGAR]).await.unwrap().get("item_id");
        f.db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&f.mutator, &destination, &FLAG_CARGO]).await.unwrap();
        for parent in [parent, destination] {
        let original_mutator = crate::services::items::load_items(&f.db, &f.sde, crate::dao::items::loading::ItemQuery::Id(f.mutator)).await.unwrap().pop().unwrap();
        let original_source = crate::services::items::load_items(&f.db, &f.sde, crate::dao::items::loading::ItemQuery::Id(f.source)).await.unwrap().pop().unwrap();
        let draws = f.draws();
        let config = crate::config::tests::test_database();
        let (mut connection, driver) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { driver.await.unwrap(); });
        let holder = connection.transaction().await.unwrap();
        let pid = crate::dao::items::parent_locks::tests::backend_pid(&holder).await;
        holder.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&parent, &(f.owner + 1)]).await.unwrap();
        let db = f.db.clone(); let sde = f.sde.clone(); let context = f.context();
        let dispatched = Arc::new(AtomicBool::new(false)); let sent = dispatched.clone();
        let worker = tokio::spawn(async move {
            db.transaction(move |tx, outbox: &mut Outbox| {
                let sde = sde.clone(); let context = context.clone(); let draws = draws.clone();
                let mutator = original_mutator.clone(); let source = original_source.clone();
                Box::pin(async move { mutation::create(tx, &sde, outbox, &context, &mutator, &source, &draws).await })
            }, move |result, _| async move { sent.store(true, Ordering::SeqCst); Ok(result) }).await
        });
        crate::dao::items::parent_locks::tests::wait_blocked(&f.db, pid).await;
        holder.commit().await.unwrap();
        let before = f.fingerprint().await;
        let result = worker.await;
        let after = f.fingerprint().await;
        let did_dispatch = dispatched.load(Ordering::SeqCst);
        f.db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&parent, &f.owner]).await.unwrap();
        assert!(matches!(result.unwrap(), Err(CallError::User { .. })));
        assert_eq!(after, before);
        assert!(!did_dispatch);
        }
    }).await;
    f.cleanup().await;
    outcome.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with loaded SDE; run serially"]
async fn mutation_locks_hull_inputs_and_parents_before_highest_parent() {
    let f = Arc::new(Fixture::new().await);
    let scenario = f.clone();
    let outcome = tokio::spawn(async move {
        let f = scenario;
        let mutator_volume = item_volume(&f.sde, f.mutator_type, false, 3, None).ok().unwrap();
        let result_volume = item_volume(&f.sde, f.result_type, true, 1, None).ok().unwrap();
        let needed = mutator_volume + result_volume;
        let box_type = f.sde.types.iter().find(|ty| hold_storage::is_ordinary_container(&f.sde, ty.type_id)
            && hold_storage::static_capacity(&f.sde, ty.type_id).is_ok_and(|capacity| capacity >= needed))
            .expect("ordinary container with room for remnant and result").type_id;
        let insert = "INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id";
        let source_parent: i64 = f.db.query_one(insert, &[&box_type, &f.owner, &f.station, &FLAG_HANGAR]).await.unwrap().get("item_id");
        let mutator_parent: i64 = f.db.query_one(insert, &[&box_type, &f.owner, &f.ship, &FLAG_CARGO]).await.unwrap().get("item_id");
        assert!(mutator_parent > source_parent && mutator_parent > f.source && mutator_parent > f.mutator && mutator_parent > f.ship);
        f.db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&f.source, &source_parent, &FLAG_CARGO]).await.unwrap();
        f.db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&f.mutator, &mutator_parent, &FLAG_CARGO]).await.unwrap();
        let config = crate::config::tests::test_database();
        let (mut connection, driver) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { driver.await.unwrap(); });
        let holder = connection.transaction().await.unwrap();
        let pid = crate::dao::items::parent_locks::tests::backend_pid(&holder).await;
        holder.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&mutator_parent]).await.unwrap();
        let dispatched = Arc::new(AtomicBool::new(false));
        let before = f.fingerprint().await;
        let worker = {
            let f = f.clone();
            let dispatched = dispatched.clone();
            tokio::spawn(async move { f.create(f.context(), f.mutator, f.source, false, dispatched).await })
        };
        crate::dao::items::parent_locks::tests::wait_blocked(&f.db, pid).await;
        let mut probes = Vec::new();
        for id in [f.ship, f.source, f.mutator, source_parent] {
            let (mut probe, driver) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
            tokio::spawn(async move { driver.await.unwrap(); });
            let tx = probe.transaction().await.unwrap();
            let state = tx.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR SHARE NOWAIT", &[&id]).await.err().and_then(|error| error.code().cloned());
            probes.push(state);
        }
        let blocked = f.fingerprint().await;
        holder.commit().await.unwrap();
        let result = worker.await.unwrap();
        assert!(probes.iter().all(|state| state.as_ref() == Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE)));
        assert_eq!(blocked, before);
        let created = result.ok().unwrap();
        assert!(dispatched.load(Ordering::SeqCst));
        let row = f.db.query_one("SELECT type_id,location_id,flag_id,quantity,singleton FROM game.items WHERE item_id=$1", &[&created]).await.unwrap();
        assert_eq!((row.get::<_, i32>("type_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"), row.get::<_, i64>("quantity"), row.get::<_, bool>("singleton")), (f.result_type, mutator_parent, FLAG_CARGO, 1, true));
        assert!(f.db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&f.source]).await.unwrap().is_none());
        let remnant = f.db.query_one("SELECT quantity,location_id,flag_id FROM game.items WHERE item_id=$1", &[&f.mutator]).await.unwrap();
        assert_eq!((remnant.get::<_, i64>("quantity"), remnant.get::<_, i64>("location_id"), remnant.get::<_, i32>("flag_id")), (2, mutator_parent, FLAG_CARGO));
        let provenance = dynamic_items::get(&f.db, created).await.unwrap().unwrap();
        assert_eq!((provenance.source_item_id, provenance.mutator_item_id), (f.source, f.mutator));
    }).await;
    f.cleanup().await;
    outcome.unwrap();
}
