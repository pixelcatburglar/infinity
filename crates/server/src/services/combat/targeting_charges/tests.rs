use super::*;
use crate::services::effects::{projection::Projection, EffectKind, JammingType};
use crate::space::System;

mod auto_reload_db;
mod charge_sync;
mod external_completion;
mod gm_reload;
mod container_capacity;
mod container_races;


#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn refused_unload_preserves_charge_and_emits_nothing() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).expect("published ship").type_id;
    let size_id = sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
    let (module_type, kind) = sde.types.iter()
        .filter(|m| m.published == Some(true) && m.category_id == Some(infinity_core::consts::CATEGORY_MODULE))
        .find_map(|m| {
            let groups: Vec<_> = CHARGE_GROUP_ATTRIBUTES.iter().flat_map(|name| sde.dogma.attributes_by_name(name))
                .filter_map(|a| m.attribute(a.attribute_id)).collect();
            let size = size_id.and_then(|id| m.attribute(id));
            let capacity = m.capacity.unwrap_or(0.0).max(m.attribute(ATTR_CAPACITY).unwrap_or(0.0));
            let charge = sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE)
                && charge_matches(groups.iter().copied(), c.group_id, size, size_id.and_then(|id| c.attribute(id)))
                && c.volume.is_some_and(|v| v > 0.0 && capacity >= v))?;
            Some((m.type_id, charge.type_id))
        }).expect("published module and compatible charge");
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let owner = 1_i64;
    let flag = infinity_core::consts::FLAG_HI_SLOT_FIRST;
    let ship = crate::dao::items::transfers::insert(&tx, ship_type, owner, 1, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let id = crate::dao::items::transfers::insert(&tx, module_type, owner, ship, flag, 1).await.unwrap();
    let source = crate::dao::items::transfers::insert(&tx, kind, owner, ship, FLAG_CARGO, 1).await.unwrap();
    tx.execute("UPDATE game.items SET singleton = true WHERE item_id = ANY($1)", &[&&[ship, id][..]]).await.unwrap();
    tx.execute("INSERT INTO game.module_charges (ship_id, flag_id, type_id, quantity) VALUES ($1, $2, $3, 1)", &[&ship, &flag, &kind]).await.unwrap();
    let module = load_items(&tx, &sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap();
    let before = load_items(&tx, &sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    let mut outbox: Outbox = Outbox::default();
    use crate::dao::items::{deliveries, parent_locks::{LockRequest, LockStrength}};
    let destination = crate::services::inventory::destination::Destination::owned(owner, ship, infinity_core::consts::FLAG_FRIGATE_ESCAPE_BAY);
    let mut deposit = crate::services::items::DepositPreparation::capture(&tx, &sde, &destination, kind).await.unwrap();
    let ancestry = deliveries::ancestry(&tx, ship).await.unwrap();
    let mut requests = crate::services::inventory::destination::parent_requests(&sde, ancestry.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share).unwrap();
    requests.push(LockRequest { item_id: id, strength: LockStrength::Share });
    let mandatory: Vec<_> = requests.iter().map(|request| request.item_id).collect();
    let optional: Vec<_> = deposit.candidate_id().filter(|id| !mandatory.contains(id)).into_iter().collect();
    requests.extend(deposit.candidate_request());
    let locks = crate::services::inventory::destination::lock_items(&tx, &sde, &requests, &optional).await.unwrap();
    assert!(deliveries::ancestry(&tx, ship).await.unwrap() == ancestry);
    assert!(matches!(unload(&tx, &sde, &mut outbox, owner, ship, &module, ship, infinity_core::consts::FLAG_FRIGATE_ESCAPE_BAY, &mut deposit, &locks).await,
        Err(CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_CANNOT_STORE_DESTINATION_RESTRICTED));
    assert!(outbox.into_entries().is_empty());
    assert_eq!(loaded(&tx, ship, flag).await.unwrap(), Some((kind, 1)));
    let after = load_items(&tx, &sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert_eq!((after.location_id, after.flag_id, after.quantity), (before.location_id, before.flag_id, before.quantity));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn replacement_reload_refuses_overfilled_cargo_atomically() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let size_id = sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
    let (module_type, charge_type, old_type) = sde.types.iter()
        .filter(|m| m.published == Some(true) && m.category_id == Some(infinity_core::consts::CATEGORY_MODULE))
        .find_map(|m| {
            let groups: Vec<_> = CHARGE_GROUP_ATTRIBUTES.iter().flat_map(|name| sde.dogma.attributes_by_name(name))
                .filter_map(|a| m.attribute(a.attribute_id)).collect();
            let size = size_id.and_then(|id| m.attribute(id));
            let matches = |c: &crate::sde::types::TypeData| charge_matches(groups.iter().copied(), c.group_id, size, size_id.and_then(|id| c.attribute(id)));
            let capacity = m.capacity.unwrap_or(0.0).max(m.attribute(ATTR_CAPACITY).unwrap_or(0.0));
            let charge = sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE)
                && matches(c) && c.volume.is_some_and(|v| v > 0.0 && capacity >= v))?;
            let old = sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE)
                && !matches(c) && c.volume.is_some_and(|v| v > 1.0))?;
            Some((m.type_id, charge.type_id, old.type_id))
        }).expect("module with compatible and incompatible charges");
    let charge_volume = sde.types.get(charge_type).unwrap().volume.unwrap();
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && t.capacity.unwrap_or(0.0).max(t.attribute(ATTR_CAPACITY).unwrap_or(0.0))
            >= 2.0 * charge_volume + sde.types.get(old_type).unwrap().volume.unwrap())
        .expect("published ship with cargo capacity").type_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('reload-hold-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let owner: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'reload-hold-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let ship: i64 = db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, 1, 0, 1, true) RETURNING item_id", &[&ship_type, &owner]).await.unwrap().get("item_id");
    let flag = infinity_core::consts::FLAG_FITTING_FIRST;
    let module: i64 = db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, 1, true) RETURNING item_id", &[&module_type, &owner, &ship, &flag]).await.unwrap().get("item_id");
    let old_quantity = i64::from(i32::MAX) / 2;
    db.execute("INSERT INTO game.module_charges (ship_id, flag_id, type_id, quantity) VALUES ($1, $2, $3, $4)", &[&ship, &flag, &old_type, &old_quantity]).await.unwrap();
    let pending = crate::dao::items::pending_reloads::insert(&db, owner, ship, module, flag, charge_type, 1, FLAG_CARGO, ship).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let (notify, mut rx) = tokio::sync::mpsc::unbounded_channel();
    for source_flag in [FLAG_CARGO, FLAG_SPECIALIZED_AMMO_HOLD] {
        for existing in [false, true] {
            let source: i64 = db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, 1, false) RETURNING item_id", &[&charge_type, &owner, &ship, &source_flag]).await.unwrap().get("item_id");
            let destination: Option<i64> = if existing {
                Some(db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, 1, false) RETURNING item_id", &[&old_type, &owner, &ship, &FLAG_CARGO]).await.unwrap().get("item_id"))
            } else { None };
            let result = reloading::reload(&db, &world, &notify, sde.clone(), ship, vec![module], vec![source], Some(ship), Some(ship), None, Some(owner), None, false, None, None, None).await;
            assert!(matches!(result, Err(CallError::User { .. })));
            assert_eq!(loaded(&db, ship, flag).await.unwrap(), Some((old_type, old_quantity)));
            let rows = crate::dao::items::pending_reloads::for_flags(&db, ship, &[flag]).await.unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].reload_id, pending);
            assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id = $1", &[&source]).await.unwrap().get::<_, i64>("quantity"), 1);
            if let Some(id) = destination {
                assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id = $1", &[&id]).await.unwrap().get::<_, i64>("quantity"), 1);
                db.execute("DELETE FROM game.items WHERE item_id = $1", &[&id]).await.unwrap();
            }
            assert!(rx.try_recv().is_err());
            db.execute("DELETE FROM game.items WHERE item_id = $1", &[&source]).await.unwrap();
        }
    }
    db.execute("DELETE FROM game.pending_reloads WHERE reload_id = $1", &[&pending]).await.unwrap();
    db.execute("DELETE FROM game.module_charges WHERE ship_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1 OR item_id = $2", &[&module, &ship]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}

#[test]
fn pending_locks_recheck_generation_validity_and_capacity() {
    let mut st = State::default();
    st.locking.insert((1, 10), 2);
    assert_eq!(finish_lock(&mut st, 100, 1, 10, 1, 0, true, 1, None), (false, false));
    assert_eq!(finish_lock(&mut st, 100, 1, 10, 2, 0, false, 1, None), (true, false));
    assert!(st.targets.get(&1).is_none());
    st.locking.insert((1, 10), 3);
    assert_eq!(finish_lock(&mut st, 100, 1, 10, 3, 0, true, 1, None), (true, true));
    st.locking.insert((1, 11), 4);
    assert_eq!(finish_lock(&mut st, 100, 1, 11, 4, 0, true, 1, None), (true, false));
    assert_eq!(st.targets[&1], HashSet::from([10]));
}

#[test]
fn cycle_lock_completion_survives_retirement_and_replacement_but_not_stop() {
    for case in 0..5 {
        let mut st = State::default();
        let generation = Arc::new(AtomicBool::new(false));
        let owner = (2, generation.clone());
        st.active.insert(2, ActiveModule { char_id: 1, stop: generation.clone(), forced_inactive: false });
        assert!(lock_owner_valid(&st, Some(&owner)));
        st.locking.insert((1, 10), 1);
        match case {
            0 => generation.store(true, Ordering::Relaxed),
            1 => st.active.get_mut(&2).unwrap().stop = Arc::new(AtomicBool::new(false)),
            2 => {
                st.active.get_mut(&2).unwrap().forced_inactive = true;
                generation.store(true, Ordering::Relaxed);
            }
            3 => { st.active.remove(&2); },
            _ => {}
        }
        let completes = case != 0 && case != 2;
        assert_eq!(lock_owner_valid(&st, Some(&owner)), case == 4);
        assert_eq!(finish_lock(&mut st, 100, 1, 10, 1, 0, true, 1, Some(&owner)), (true, completes));
        assert!(!st.locking.contains_key(&(1, 10)));
        assert_eq!(st.targets.get(&1).is_some_and(|targets| targets.contains(&10)), completes);
    }
}

#[test]
fn jam_rejects_cycle_lock_completion_after_owner_retirement() {
    crate::tests::init_config();
    let ship = NEXT_LOCK.fetch_add(4, Ordering::Relaxed) as i64;
    let pilot = ship + 1;
    let target = ship + 2;
    let module = ship + 3;
    let mut system = System::new(ship, Arc::default(), 1000);
    system.insert(Ball {
        id: ship, type_id: 1, owner_id: pilot, corp_id: 1, alliance_id: 1,
        char_id: Some(pilot), signature_radius: None, mode: crate::space::MODE_STOP,
        radius: 1.0, mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: false, global: false, massive: true, interactive: true,
        warp: crate::space::WarpState::default(), last_ms: 0,
        interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    });
    let generation = Arc::new(AtomicBool::new(false));
    let owner = (module, generation);
    jamming::apply(&mut system, target, module, ship, 100.0);
    let mut st = state();
    st.locking.insert((pilot, target), 1);
    assert_eq!(finish_lock(&mut st, ship, pilot, target, 1, 1000, true, 1, Some(&owner)), (true, false));
    assert!(!st.targets.contains_key(&pilot));
    drop(st);
    jamming::clear(&mut system, ship);
}

#[test]
fn auto_target_candidates_only_include_pilots_locking_the_ship() {
    let mut st = State::default();
    st.targets.insert(1, HashSet::from([10]));
    st.targets.insert(2, HashSet::from([11]));
    st.locking.insert((3, 10), 1);
    assert_eq!(targeters_in(&st, 10), vec![1]);
    assert_eq!(targeters_in(&st, 11), vec![2]);
    assert!(targeters_in(&st, 12).is_empty());
}

#[test]
fn passive_lock_notifies_owner_without_yellow_box() {
    for silent in [false, true] {
        let mut owner = Vec::new();
        let mut other = Vec::new();
        notify_lock(20, 10, silent, |args| owner = args, |args| other = args);
        assert_eq!(owner[0].as_bytes(), Some(b"add".as_slice()));
        assert_eq!(owner[1].as_int(), Some(20));
        if silent { assert!(other.is_empty()); } else {
            assert_eq!(other[0].as_bytes(), Some(b"otheradd".as_slice()));
            assert_eq!(other[1].as_int(), Some(10));
        }
    }
}

#[test]
fn loss_notices_only_consume_announced_locks() {
    let mut st = State::default();
    st.targets.insert(1, HashSet::from([20, 30]));
    st.announced_locks.insert((1, 20), (10, 2));
    assert!(take_announced(&mut st, |ch, target| ch == 1 && target == 30).is_empty());
    assert_eq!(take_announced(&mut st, |ch, target| ch == 1 && target == 20), vec![(10, 2)]);
    assert!(take_announced(&mut st, |_, _| true).is_empty());
}

#[test]
fn cloak_lock_invalidation_preserves_unrelated_locks_and_capacitor() {
    let mut st = State::default();
    st.targets.insert(1, HashSet::from([20, 30]));
    st.targets.insert(2, HashSet::from([10, 40]));
    st.locking.insert((1, 50), 1);
    st.locking.insert((2, 10), 2);
    st.locking.insert((2, 40), 3);
    st.capacitor.insert(10, (42.0, 1));
    let (incoming, outgoing) = clear_lock_state(&mut st, 10, 1);
    assert_eq!(incoming, vec![2]);
    assert_eq!(outgoing, HashSet::from([20, 30]));
    assert_eq!(st.targets[&2], HashSet::from([40]));
    assert_eq!(st.locking.len(), 1);
    assert_eq!(st.capacitor[&10], (42.0, 1));
}

#[test]
fn reload_groups_use_each_modules_deadline_and_charge_type() {
    let loads = vec![
        PendingLoad { owner: 1, reload_ids: vec![1, 5], external_sources: vec![], external_facts: Vec::new(), load: (1, 1, 10, 5, 0) },
        PendingLoad { owner: 1, reload_ids: vec![2], external_sources: vec![], external_facts: Vec::new(), load: (2, 2, 10, 3, 0) },
        PendingLoad { owner: 1, reload_ids: vec![3], external_sources: vec![], external_facts: Vec::new(), load: (3, 3, 10, 1, 0) },
        PendingLoad { owner: 1, reload_ids: vec![4], external_sources: vec![], external_facts: Vec::new(), load: (4, 4, 11, 1, 0) },
    ];
    let groups = group_loads(loads, |id| if id == 2 { 2000.0 } else { 1000.0 });
    let ids = |key| groups[&key].iter().map(|(load, _)| load.load.0).collect::<Vec<_>>();
    assert_eq!(ids((filetime_ticks_from_ms(1000.0), 10)), vec![1, 3]);
    assert_eq!(ids((filetime_ticks_from_ms(2000.0), 10)), vec![2]);
    assert_eq!(ids((filetime_ticks_from_ms(1000.0), 11)), vec![4]);
}

#[test]
fn exhausted_stock_only_counts_modules_with_reservations() {
    let item = Item { item_id: 1, type_id: 10, owner_id: 1, location_id: 2, flag_id: FLAG_CARGO,
        quantity: 6, singleton: false, group_id: 0, category_id: 0, custom_info: None };
    let mut items = BTreeMap::from([(1, (item, 6))]);
    let reservations: Vec<_> = (0..3).filter_map(|_| reserve_load(&mut items, &[1], 10, 5, 0).map(|(now, _)| now)).collect();
    assert_eq!(reservations, vec![5, 1]);
    assert_eq!(reservations.len(), 2);
    assert_eq!(items[&1].1, 0);
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized test database"]
async fn member_resolution_is_separate_from_master_only_bank() {

    let mut config = crate::config::tests::test_database();
    config.schema_on_start = false;
    let db = Db::connect(&config).await.expect("database pool");
    let fixture = crate::services::tests::fixture::create(&db).await.expect("character fixture");
    let owner = fixture.character;
    let setup = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let ship = crate::dao::items::creation::positioned(tx, 1, owner, 1, [0.0; 3]).await?;
        let master = crate::dao::items::creation::rookie_ship(tx, 1, owner, ship, 1).await?;
        let slave = crate::dao::items::creation::rookie_ship(tx, 1, owner, ship, 2).await?;
        let other_slave = crate::dao::items::creation::rookie_ship(tx, 1, owner, ship, 3).await?;
        crate::dao::items::combat::move_bank(tx, ship, master, slave).await?;
        crate::dao::items::combat::move_bank(tx, ship, master, other_slave).await?;
        Ok::<_, crate::db::Error>((ship, master, slave, other_slave))
    }), |ids, ()| async move { Ok(ids) }).await;
    let (ship, master_id, slave_id, other_slave) = match setup {
        Ok(ids) => ids,
        Err(error) => {
            crate::services::tests::fixture::cleanup(&db, fixture).await.expect("cleanup character fixture");
            panic!("create bank fixture: {error}");
        }
    };
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let rows = crate::dao::items::charges::banks(&test_db, ship).await.expect("banks");
        assert_eq!(rows.len(), 2);
        let resolved = master_for_member(&test_db, ship, slave_id).await.unwrap_or_else(|_| panic!("member master"));
        assert_eq!(resolved, master_id);
        assert_eq!(master_for_member(&test_db, ship, other_slave).await.unwrap_or_else(|_| panic!("other member master")), master_id);
        assert_eq!(master_for_member(&test_db, ship, master_id).await.unwrap_or_else(|_| panic!("master")), master_id);
        let master = bank(&test_db, ship, resolved).await.unwrap_or_else(|_| panic!("master bank"));
        assert_eq!(master[0], master_id);
        assert_eq!(master.len(), 3);
        for member in &rows {
            assert_eq!(member.master_id, master_id);
            assert!(master.contains(&member.slave_id));
        }
        assert!(master.contains(&slave_id));
        assert!(master.contains(&other_slave));
        let slave = bank(&test_db, ship, slave_id).await.unwrap_or_else(|_| panic!("slave bank"));
        assert_eq!(slave, vec![slave_id]);
    }).await;
    crate::dao::items::combat::clear_banks(&db, ship).await.expect("cleanup weapon bank");
    crate::services::tests::fixture::cleanup(&db, fixture).await.expect("cleanup character fixture");
    result.expect("bank assertions");
}

#[test]
fn removed_locks_clear_only_matching_outgoing_projections() {
    let mut s = System::new(0, Arc::default(), 1_000);
    let ball = Ball {
        id: 1, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: crate::space::MODE_STOP,
        radius: 1.0, mass: 1.0, max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3], follow_id: 0,
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0,
        cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    };
    for id in 1..=4 { s.insert(Ball { id, ..ball.clone() }); }
    let projection = |source_ship, source_module| Projection {
        source_ship, source_module, source_type: 1, effect_id: 1,
        kind: EffectKind::Projected { jamming: JammingType::Webify }, until_ms: i64::MAX,
        values: vec![], charge_values: vec![], charge_type: None, scramble: None,
    };
    s.upsert_projection(2, projection(1, 10));
    s.upsert_projection(3, projection(1, 11));
    s.upsert_projection(2, projection(4, 12));
    let revision = s.projection_revision(2);
    remove_target_projections_in_system(&mut s, 1, &[2]);
    assert!(s.projection_revision(2) > revision);
    assert_eq!(s.projections_on(2).iter().map(|p| p.source_module).collect::<Vec<_>>(), vec![12]);
    assert_eq!(s.projections_on(3).len(), 1);
    remove_target_projections_in_system(&mut s, 1, &[2, 3]);
    assert_eq!(s.projections_on(2).len(), 1);
    assert!(s.projections_on(3).is_empty());
}

#[test]
fn compatible_groups_and_sizes_are_required() {
    assert!(charge_matches([10.0, 20.0].into_iter(), Some(20), Some(2.0), Some(2.0)));
    assert!(!charge_matches([10.0].into_iter(), Some(20), Some(2.0), Some(2.0)));
    assert!(!charge_matches([20.0].into_iter(), Some(20), Some(2.0), Some(1.0)));
    assert!(!charge_matches([20.0].into_iter(), Some(20), Some(2.0), None));
    assert!(!charge_matches([].into_iter(), None, None, None));
    assert!(charge_matches([20.0].into_iter(), Some(20), None, None));
}

mod shared_bays;

pub(in crate::services) fn compatible_ammunition(sde: &Sde) -> (i32, i32) {
    let size_id = sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
    sde.types.iter().filter(|m| m.published == Some(true) && m.category_id == Some(infinity_core::consts::CATEGORY_MODULE) && sde.is_tradable(m.type_id))
        .find_map(|m| {
            let groups: Vec<_> = CHARGE_GROUP_ATTRIBUTES.iter().flat_map(|name| sde.dogma.attributes_by_name(name)).filter_map(|a| m.attribute(a.attribute_id)).collect();
            let size = size_id.and_then(|id| m.attribute(id));
            let capacity = m.capacity.unwrap_or(0.0).max(m.attribute(ATTR_CAPACITY).unwrap_or(0.0));
            let charge = sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE) && sde.is_tradable(c.type_id)
                && charge_matches(groups.iter().copied(), c.group_id, size, size_id.and_then(|id| c.attribute(id)))
                && c.volume.is_some_and(|v| v > 0.0 && capacity >= v && capacity / v < 100_000.0))?;
            Some((m.type_id, charge.type_id))
        }).expect("compatible module and charge")
}

async fn shared_reload_fixture() -> (crate::services::drones::tests::fixture::Fixture, i64, i64, i32, i64) {
    let mut f = crate::services::drones::tests::fixture::Fixture::with_hold_attributes(&[
        infinity_core::consts::dogma::ATTR_FLEET_HANGAR_CAPACITY,
        infinity_core::consts::dogma::ATTR_SPECIAL_FUEL_BAY_CAPACITY,
    ]).await;
    let (guest, _) = f.add_pilot().await;
    let (module_type, charge_type) = compatible_ammunition(&f.sde);
    let ship = f.ship;
    let module = crate::dao::items::creation::rookie_ship(&f.db, module_type, f.owner, ship, infinity_core::consts::FLAG_FITTING_FIRST).await.unwrap();
    f.set(ship, ATTR_CAPACITY, 1_000_000.0).await;
    f.set(ship, infinity_core::consts::dogma::ATTR_FLEET_HANGAR_CAPACITY, 1_000_000_000.0).await;
    f.set(module, ATTR_RELOAD_TIME, 60_000.0).await;
    let source = crate::dao::items::transfers::insert(&f.db, charge_type, guest, ship, FLAG_FLEET_HANGAR, 100_000).await.unwrap();
    (f, guest, module, charge_type, source)
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn shared_guest_reload_reservation_refunds_actor_without_changing_guest_remainder() {
    let (mut f, guest, module, charge_type, source) = shared_reload_fixture().await;
    let ship = f.ship;
    let args = [Value::Int(ship), Value::Int(module), Value::list([Value::Int(source)]), Value::Int(ship)];
    let kwargs = Value::None;
    let call = Call { service: "dogmaIM", method: "LoadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    let pending = crate::dao::items::pending_reloads::for_ship(&f.db, ship).await.unwrap();
    assert_eq!(pending.len(), 1);
    let amount = pending[0].quantity;
    assert!(amount > 0 && amount < 100_000);
    assert_eq!(pending[0].owner_id, f.owner);
    let remaining = load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert_eq!((remaining.owner_id, remaining.quantity), (guest, 100_000 - amount));
    let ids = vec![pending[0].reload_id];
    f.db.transaction(|tx, outbox: &mut Outbox| {
        let ids = ids.clone();
        let sde = f.sde.clone();
        let pending = pending.clone();
        Box::pin(async move {
            use crate::dao::items::parent_locks::{LockRequest, LockStrength};
            use crate::services::combat::reload_recovery::{RefundPreparation, return_pending};
            let mut refund = RefundPreparation::capture_pending(tx, &sde, &pending).await?;
            let mut requests: Vec<_> = pending.iter().flat_map(|row| [row.ship_id, row.source_location_id])
                .map(|item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
            let mut mandatory: Vec<_> = requests.iter().map(|request| request.item_id).collect();
            mandatory.extend(refund.mandatory_ids());
            let optional: Vec<_> = refund.candidate_ids().into_iter().filter(|id| !mandatory.contains(id)).collect();
            requests.extend_from_slice(refund.requests());
            let locks = crate::services::inventory::destination::lock_items(tx, &sde, &requests, &optional).await?;
            refund.revalidate(tx, &sde, &locks).await?;
            return_pending(tx, &sde, outbox, &ids, true, &locks, &mut refund).await
        })
    }, |value, _: Outbox| async move { Ok(value) }).await.unwrap();
    let unchanged = load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert_eq!((unchanged.owner_id, unchanged.quantity), (guest, remaining.quantity));
    let restored = load_items(&f.db, &f.sde, ItemQuery::LocationFlagTypeOwnerStack(ship, FLAG_FLEET_HANGAR, charge_type, f.owner)).await.unwrap().pop().unwrap();
    assert_eq!(restored.quantity, amount);
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn selected_unload_capacity_refusal_rolls_back_and_shared_hull_accepts() {
    use crate::services::drones::tests::fixture::Fixture;
    let mut f = Fixture::with_hold_attributes(&[infinity_core::consts::dogma::ATTR_FLEET_HANGAR_CAPACITY]).await;
    let (module_type, charge_type) = compatible_ammunition(&f.sde);
    let charge = f.sde.types.get(charge_type).unwrap();
    let capacity = (charge.volume.unwrap() * (f64::from(i32::MAX) + 4.0) * 2.0).max(1_000_000.0);
    assert!(capacity.is_finite());
    let ship = f.ship;
    let host_type = f.world.ball(f.system, ship).await.unwrap().type_id;
    let host = crate::dao::items::creation::positioned(&f.db, host_type, f.owner, f.system, [0.0; 3]).await.unwrap();
    let mut ball = f.world.ball(f.system, ship).await.unwrap();
    ball.id = host;
    ball.type_id = host_type;
    ball.char_id = None;
    f.world.add_ship(f.system, ball).await.unwrap();
    let flag = infinity_core::consts::FLAG_FITTING_FIRST;
    let module = crate::dao::items::creation::rookie_ship(&f.db, module_type, f.owner, ship, flag).await.unwrap();
    let second_flag = flag + 1;
    let second = crate::dao::items::creation::rookie_ship(&f.db, module_type, f.owner, ship, second_flag).await.unwrap();
    crate::dao::items::combat::apply_loads(&f.db, ship, &[(module, flag, charge_type, 2, 0), (second, second_flag, charge_type, 2, 0)]).await.unwrap();
    let source = crate::dao::items::transfers::insert(&f.db, charge_type, f.owner, ship, FLAG_CARGO, 5).await.unwrap();
    let pending_id = crate::dao::items::pending_reloads::insert(&f.db, f.owner, ship, module, flag, charge_type, 1, FLAG_CARGO, ship).await.unwrap();
    let pending_before = crate::dao::items::pending_reloads::for_ship(&f.db, ship).await.unwrap();
    assert!(pending_before.len() == 1 && pending_before[0].reload_id == pending_id);
    assert!(crate::dao::items::pending_reloads::intersecting(&f.db, &[ship]).await.unwrap() == pending_before);
    let source_before = load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    let (viewer, _) = f.add_pilot().await;
    let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(viewer, sender);
    f.set(host, infinity_core::consts::dogma::ATTR_FLEET_HANGAR_CAPACITY, 0.0).await;
    assert_eq!(crate::services::inventory::hold_storage::capacity(&f.db, &f.sde, f.owner, host, FLAG_FLEET_HANGAR).await.unwrap().unwrap().0, 0.0);
    let args = [Value::Int(ship), Value::list([Value::Int(module), Value::Int(second)]), Value::tuple([Value::Int(host), Value::Int(i64::MAX), Value::Int(i64::from(FLAG_FLEET_HANGAR))])];
    let kwargs = Value::None;
    let call = Call { service: "dogmaIM", method: "UnloadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    let before = f.client.outbox.len();
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    assert_eq!(loaded(&f.db, ship, flag).await.unwrap(), Some((charge_type, 2)));
    assert_eq!(loaded(&f.db, ship, second_flag).await.unwrap(), Some((charge_type, 2)));
    assert!(crate::dao::items::pending_reloads::for_ship(&f.db, ship).await.unwrap() == pending_before);
    assert!(received.try_recv().is_err());
    assert_eq!(f.client.outbox.len(), before);
    f.set(host, infinity_core::consts::dogma::ATTR_FLEET_HANGAR_CAPACITY, capacity).await;
    assert_eq!(crate::services::inventory::hold_storage::capacity(&f.db, &f.sde, f.owner, host, FLAG_FLEET_HANGAR).await.unwrap().unwrap().0, capacity);
    let full = crate::dao::items::transfers::insert(&f.db, charge_type, f.owner, host, FLAG_FLEET_HANGAR, i64::from(i32::MAX) - 2).await.unwrap();
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    assert_eq!(loaded(&f.db, ship, flag).await.unwrap(), None);
    assert_eq!(loaded(&f.db, ship, second_flag).await.unwrap(), None);
    let row = load_items(&f.db, &f.sde, ItemQuery::Id(full)).await.unwrap().pop().unwrap();
    assert_eq!(row.quantity, i64::from(i32::MAX) + 2);
    assert_eq!(load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap().quantity, source_before.quantity);
    assert!(crate::dao::items::pending_reloads::for_ship(&f.db, ship).await.unwrap() == pending_before);
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn selected_unload_locks_destination_hull_before_module() {
    use crate::services::drones::tests::fixture::Fixture;
    use std::time::Duration;
    let mut f = Fixture::with_hold_attributes(&[infinity_core::consts::dogma::ATTR_FLEET_HANGAR_CAPACITY]).await;
    let (module_type, charge_type) = compatible_ammunition(&f.sde);
    let ship = f.ship;
    let host_type = f.world.ball(f.system, ship).await.unwrap().type_id;
    let host = crate::dao::items::creation::positioned(&f.db, host_type, f.owner, f.system, [0.0; 3]).await.unwrap();
    let mut ball = f.world.ball(f.system, ship).await.unwrap();
    ball.id = host;
    ball.char_id = None;
    f.world.add_ship(f.system, ball).await.unwrap();
    f.set(host, infinity_core::consts::dogma::ATTR_FLEET_HANGAR_CAPACITY, 1_000_000.0).await;
    let flag = infinity_core::consts::FLAG_FITTING_FIRST;
    let module = crate::dao::items::creation::rookie_ship(&f.db, module_type, f.owner, ship, flag).await.unwrap();
    crate::dao::items::combat::apply_loads(&f.db, ship, &[(module, flag, charge_type, 2, 0)]).await.unwrap();

    let config = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut holder_client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let holder = holder_client.transaction().await.unwrap();
    holder.query_one("SELECT item_id FROM game.items WHERE item_id = $1 FOR UPDATE", &[&module]).await.unwrap();
    let pid = crate::dao::items::parent_locks::tests::backend_pid(&holder).await;
    let db = f.db.clone();
    let operation = tokio::spawn(async move {
        let args = [Value::Int(ship), Value::list([Value::Int(module)]), Value::tuple([
            Value::Int(host), Value::Int(i64::MAX), Value::Int(i64::from(FLAG_FLEET_HANGAR)),
        ])];
        let kwargs = Value::None;
        let call = Call { service: "dogmaIM", method: "UnloadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        let result = crate::services::dispatch(&mut f.client, &call).await;
        (f, result)
    });
    tokio::time::timeout(Duration::from_secs(30), crate::dao::items::parent_locks::tests::wait_blocked(&db, pid)).await.unwrap();
    let (probe_client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let probe = tokio::time::timeout(Duration::from_secs(30),
        probe_client.query_one("SELECT item_id FROM game.items WHERE item_id = $1 FOR SHARE NOWAIT", &[&host])).await.unwrap();
    holder.rollback().await.unwrap();
    let (f, result) = tokio::time::timeout(Duration::from_secs(30), operation).await.unwrap().unwrap();
    let remaining = loaded(&f.db, ship, flag).await.unwrap();
    let deposited = load_items(&f.db, &f.sde, ItemQuery::LocationFlagTypeOwnerStack(host, FLAG_FLEET_HANGAR, charge_type, f.owner)).await.unwrap().pop();
    f.cleanup().await;
    assert!(result.is_ok(), "selected unload failed: {result:?}");
    assert_eq!(remaining, None);
    assert_eq!(deposited.map(|stack| stack.quantity), Some(2));
    assert!(matches!(probe, Err(error) if error.code() == Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE)),
        "destination hull must already be UPDATE locked before the module");
}
