//! Recovery of committed selection changes after the owning system actor stops.
//! The client consumes the resulting state through beyonce's full-state update;
//! custody and condition are read from the game database and the SDE.

use super::*;
use infinity_core::consts::{FLAG_SHIP_HANGAR, TYPE_CAPSULE};
use crate::dao::items::{boarding::{self, Recovery, RecoveryPlan}, creation, loading::{load_items, ItemQuery}, transfers::{self, Transfer}};
use crate::services::drones::tests::fixture::Fixture;
use crate::services::ship::balls;
use marshal::Value;

#[derive(Clone, Copy)]
enum Operation { Board, Store, BoardStored }

async fn recover_after_stop(operation: Operation, disconnected: bool, replaced: bool, session_changed: bool, force_collision: bool, selective: bool, missing_receipt: bool, postcommit_failure: bool, capsule_drift: bool) {
    let mut f = Fixture::real_system().await;
    let owner = f.owner;
    let user = f.account;
    let system = f.system;
    let old = f.ship;
    let client = f.client.login.client_id;
    let system_data = f.sde.map.systems().find(|s| s.solar_system_id == system).unwrap();
    crate::dao::characters::ship::location(&f.db, owner, system, system_data.constellation_id.unwrap(), system_data.region_id.unwrap()).await.unwrap();
    let old_type = crate::dao::items::inventory::type_id(&f.db, old).await.unwrap().unwrap();
    let host = creation::positioned(&f.db, old_type, owner, system, [10.0, 0.0, 0.0]).await.unwrap();
    let target_type = if matches!(operation, Operation::Store) { TYPE_CAPSULE } else { old_type };
    let prior_owner = if matches!(operation, Operation::BoardStored) { f.add_pilot().await.0 } else { owner };
    let target = match operation {
        Operation::Board => creation::positioned(&f.db, target_type, owner, system, [2.0, 0.0, 0.0]).await.unwrap(),
        Operation::BoardStored => creation::rookie_ship(&f.db, target_type, prior_owner, host, FLAG_SHIP_HANGAR).await.unwrap(),
        Operation::Store => 0,
    };
    let cargo_type = f.sde.types.iter().find(|ty| ty.published == Some(true)
        && ty.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && f.sde.is_tradable(ty.type_id)).unwrap().type_id;
    let cargo_ship = if matches!(operation, Operation::BoardStored) { target } else { old };
    let cargo = transfers::insert(&f.db, cargo_type, prior_owner, cargo_ship, infinity_core::consts::FLAG_CARGO, 3).await.unwrap();
    if matches!(operation, Operation::Board) {
        let row = crate::dao::items::space_ships::unpiloted(&f.db, system).await.unwrap().into_iter().find(|row| row.item_id == target).unwrap();
        let prepared = balls::unpiloted(&f.db, f.sde.clone(), &row).await.unwrap();
        f.world.with(system, move |s| prepared.install(s, |s, ball| s.add_persistent_ship(ball))).await.unwrap();
    }
    if selective {
        let row = crate::dao::items::space_ships::unpiloted(&f.db, system).await.unwrap().into_iter().find(|row| row.item_id == host).unwrap();
        let prepared = balls::unpiloted(&f.db, f.sde.clone(), &row).await.unwrap();
        f.world.with(system, move |s| {
            prepared.install(s, |s, ball| s.add_persistent_ship(ball));
            assert!(s.set_condition_fraction_mask(host, 0b010, 0.5));
        }).await.unwrap();
    }
    f.client.session.apply(vec![("userid", Value::Int(user)), ("solarsystemid2", Value::Int(system))]);
    let session = infinity_core::session::OnlineSession::from_session(&f.client.session).unwrap();
    assert_eq!((session.user_id, session.session_id, session.character_id, session.ship_id, session.system_id),
        (Some(user), f.client.session.sid, owner, Some(old), Some(system)));
    f.world.set_online_session(owner, Some(session.clone()));
    let expected = session;
    let market = crate::services::market::lock().await;
    let gate = f.world.lock_condition_snapshot(system).await.unwrap();
    let ids = if matches!(operation, Operation::Board) { vec![old, target] } else { vec![old] };
    let transition = f.world.with(system, move |s| {
        assert!(s.set_condition_fraction_mask(old, 0b110, 0.5));
        let generation = s.combat_membership_generation(old).unwrap();
        let source = s.prepare_combat_source(old, None, None, None,
            crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
        let hit = s.apply_damage(old, [1.0, 0.0, 0.0, 0.0],
            crate::services::combat::damage::DamageSource { facts: Some(source), ..Default::default() }, false).unwrap().unwrap();
        assert!(hit.dealt > 0.0 && !hit.lethal);
        s.begin_condition_transitions(&ids).unwrap()
    }).await.unwrap();
    let entries = transition.entries.clone();
    let frame = transition.facts.clone().expect("original combat fact frame");
    assert!(frame.events().next().is_some());
    assert!(!frame.is_committed(&f.db).await.unwrap());
    let states: Vec<_> = entries.iter().map(|(_, state)| state.clone()).collect();
    let old_state = states.iter().find(|state| state.item_id == old).unwrap();
    let old_damage = (old_state.condition.stats.max[1] - old_state.condition.now[1],
        old_state.condition.stats.max[2] - old_state.condition.now[2]);
    let plan = match operation {
        Operation::Board => RecoveryPlan::Board { character: owner, user, system, old, old_type, target, target_type },
        Operation::Store => RecoveryPlan::Store { character: owner, user, system, old, old_type, host },
        Operation::BoardStored => RecoveryPlan::BoardStored { character: owner, user, system, old, old_type, host, target, target_type, prior_owner },
    };
    if postcommit_failure {
        let (corporation, alliance) = f.world.with(system, move |s| {
            let ball = s.pilot_ball(owner).unwrap();
            (ball.corp_id, ball.alliance_id)
        }).await.unwrap();
        let sde = f.sde.clone();
        let selected = settle_space(f.world.clone(), f.db.clone(), f.sde.clone(), system, owner, client,
            expected.clone(), transition, market, gate, plan,
            move |tx, outbox, states, facts| {
                let sde = sde.clone();
                Box::pin(async move {
                    let written = checkpoint(tx, outbox, system, states, facts).await?;
                    let target = if matches!(operation, Operation::Store) {
                        creation::positioned(tx, TYPE_CAPSULE, owner, system, [0.0; 3]).await.map_err(crate::services::db_err)?
                    } else { target };
                    match operation {
                        Operation::Board => assert!(boarding::board_in_space(tx, owner, old, target, system, old_type, target_type, [0.0; 3], [2.0, 0.0, 0.0]).await.map_err(crate::services::db_err)?),
                        Operation::Store | Operation::BoardStored => {
                            assert!(boarding::guard_current(tx, owner, user, old, system, old_type).await.map_err(crate::services::db_err)?);
                            let transfer = if matches!(operation, Operation::Store) {
                                Transfer { item_id: old, owner, location: host, flag: FLAG_SHIP_HANGAR }
                            } else { Transfer { item_id: target, owner, location: system, flag: 0 } };
                            let closure = crate::dao::items::inventory::descendants(tx, &[transfer.item_id]).await.map_err(crate::services::db_err)?;
                            let requests: Vec<_> = closure.iter().map(|&item_id| crate::dao::items::parent_locks::LockRequest {
                                item_id, strength: crate::dao::items::parent_locks::LockStrength::Update,
                            }).collect();
                            let locks = crate::services::inventory::destination::lock_items(tx, &sde, &requests, &[]).await?;
                            transfers::hand_over(tx, &[transfer], &closure, &locks, &[]).await.map_err(crate::services::db_err)?;
                            if matches!(operation, Operation::BoardStored) {
                                assert!(boarding::position_moved(tx, target, owner, system, [2.0, 0.0, 0.0]).await.map_err(crate::services::db_err)?);
                            }
                            assert!(boarding::select_moved(tx, owner, user, old, target, system).await.map_err(crate::services::db_err)?);
                        }
                    }
                    let row = crate::dao::items::space_ships::FreeShip {
                        item_id: target, type_id: target_type, owner_id: owner, corporation_id: corporation,
                        alliance_id: alliance, pos: [2.0, 0.0, 0.0],
                    };
                    let prepared = balls::piloted(tx, sde, &row, owner, 0.0).await?;
                    // A mismatched reservation makes the real after-COMMIT conversion fail.
                    Ok((PreparedTransition {
                        old: Some(PreparedOld::Stored { id: host }),
                        target: PreparedTarget { id: target, sampled_position: row.pos, prepared,
                            pilot: Some((client, owner)), existing: matches!(operation, Operation::Board) },
                    }, written))
                }) as crate::db::transaction::TxFuture<'_, (PreparedTransition, Vec<i64>), CallError>
            }).await.unwrap().unwrap();
        assert!(frame.is_committed(&f.db).await.unwrap());
        assert_eq!(crate::dao::characters::selection::ship_id(&f.db, owner).await.unwrap(), Some(selected));
        assert_eq!(f.world.with(system, move |s| s.pilot_ball(owner).map(|ball| ball.id)).await.unwrap(), Some(selected));
        f.cleanup().await;
        return;
    }
    let handle = f.world.systems.lock().unwrap().get(&system).unwrap().clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let settlement = BoardSettlement {
        world: Arc::downgrade(&f.world), db: f.db.clone(), sde: f.sde.clone(), system,
        character: owner, client, expected, plan, entries, facts: Some(frame.clone()), actor_gate: handle.gate.clone(),
        receipt: Mutex::new(Some((sender, None))),
    };
    let rolled_back: Result<(), CallError> = f.db.transaction(
        |tx, outbox: &mut crate::db::outbox::Outbox<()>| {
            let frame = frame.clone();
            let states = states.clone();
            Box::pin(async move {
                checkpoint(tx, outbox, system, &states, Some(&frame)).await?;
                Err(CallError::Internal(anyhow::anyhow!("force checkpoint rollback")))
            })
        }, |(), _| async move { Ok(()) },
    ).await;
    assert!(rolled_back.is_err());
    assert!(!frame.is_committed(&f.db).await.unwrap());
    assert!(f.world.with(system, move |s| {
        let state = s.combat_condition_snapshot(old).unwrap();
        state.revision != state.saved_revision
    }).await.unwrap());
    let db = f.db.clone();
    let sde = f.sde.clone();
    let target = db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox<()>| {
        let states = states.clone();
        let frame = frame.clone();
        let sde = sde.clone();
        Box::pin(async move {
            let written = checkpoint(tx, outbox, system, &states, if missing_receipt { None } else { Some(&frame) }).await?;
            assert!(written.contains(&old));
            let target = if matches!(operation, Operation::Store) {
                creation::positioned(tx, TYPE_CAPSULE, owner, system, [0.0; 3]).await.map_err(crate::services::db_err)?
            } else { target };
            match operation {
                Operation::Board => assert!(boarding::board_in_space(tx, owner, old, target, system, old_type, target_type, [0.0; 3], [2.0, 0.0, 0.0]).await.map_err(crate::services::db_err)?),
                Operation::Store | Operation::BoardStored => {
                    assert!(boarding::guard_current(tx, owner, user, old, system, old_type).await.map_err(crate::services::db_err)?);
                    let transfer = if matches!(operation, Operation::Store) {
                        Transfer { item_id: old, owner, location: host, flag: FLAG_SHIP_HANGAR }
                    } else {
                        Transfer { item_id: target, owner, location: system, flag: 0 }
                    };
                    let closure = crate::dao::items::inventory::descendants(tx, &[transfer.item_id]).await.map_err(crate::services::db_err)?;
                    let requests: Vec<_> = closure.iter().map(|&item_id| crate::dao::items::parent_locks::LockRequest {
                        item_id, strength: crate::dao::items::parent_locks::LockStrength::Update,
                    }).collect();
                    let locks = crate::services::inventory::destination::lock_items(tx, &sde, &requests, &[]).await?;
                    transfers::hand_over(tx, &[transfer], &closure, &locks, &[]).await.map_err(crate::services::db_err)?;
                    if matches!(operation, Operation::BoardStored) {
                        assert!(boarding::position_moved(tx, target, owner, system, [2.0, 0.0, 0.0]).await.map_err(crate::services::db_err)?);
                    }
                    assert!(boarding::select_moved(tx, owner, user, old, target, system).await.map_err(crate::services::db_err)?);
                }
            }
            Ok::<_, crate::services::CallError>(target)
        })
    }, |target, _| async move { Ok(target) }).await.unwrap();
    assert_eq!(frame.is_committed(&db).await.unwrap(), !missing_receipt);
    let Recovery::Committed { target: proof_target, old: proof_old } = boarding::recovery(&f.db, &plan).await.unwrap()
        else { panic!("expected committed selection proof") };
    assert_eq!((proof_target.item_id, proof_target.type_id, proof_target.owner_id), (target, target_type, owner));
    if matches!(operation, Operation::Store) { assert!(proof_old.is_none()); }
    else { assert_eq!(proof_old.unwrap().item_id, old); }
    let rows = load_items(&f.db, ItemQuery::Ids(&[old, target])).await.unwrap();
    let old_row = rows.iter().find(|row| row.item_id == old).unwrap();
    let target_row = rows.iter().find(|row| row.item_id == target).unwrap();
    assert_eq!((old_row.type_id, old_row.owner_id, old_row.location_id, old_row.flag_id, old_row.singleton, old_row.quantity),
        (old_type, owner, if matches!(operation, Operation::Store) { host } else { system }, if matches!(operation, Operation::Store) { FLAG_SHIP_HANGAR } else { 0 }, true, 1));
    assert_eq!((target_row.type_id, target_row.owner_id, target_row.location_id, target_row.flag_id, target_row.singleton, target_row.quantity), (target_type, owner, system, 0, true, 1));
    let cargo_row = load_items(&f.db, ItemQuery::Id(cargo)).await.unwrap().pop().unwrap();
    assert_eq!((cargo_row.type_id, cargo_row.owner_id, cargo_row.location_id, cargo_row.flag_id, cargo_row.quantity),
        (cargo_type, owner, cargo_ship, infinity_core::consts::FLAG_CARGO, 3));
    assert_eq!(crate::dao::characters::selection::ship_id(&f.db, owner).await.unwrap(), Some(target));
    let attrs = crate::dao::items::dogma::overrides(&f.db, old).await.unwrap();
    for (attribute, expected) in [(infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, old_damage.0),
        (infinity_core::consts::dogma::ATTR_HULL_DAMAGE, old_damage.1)] {
        assert!(attrs.iter().any(|row| row.attribute_id == attribute && (row.value - expected).abs() < 1e-8));
    }
    if capsule_drift {
        assert!(matches!(operation, Operation::Store));
        let config = crate::config::tests::test_database();
        let (mut raw, connection) = config.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        let connection = tokio::spawn(async move { let _ = connection.await; });
        let tx = raw.transaction().await.unwrap();
        let blocker: i32 = tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
        tx.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&target]).await.unwrap();
        let (notifications, mut published) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(owner, notifications);
        let actor_before = f.world.with(system, move |s| {
            let condition = s.combat_condition_snapshot(old).unwrap();
            (s.pilot_ball(owner).map(|ball| ball.id), s.persistent_generation(old),
                s.combat_phase(old), s.ship_condition(old).map(|v| (v.1, v.2)),
                s.observers.get(&client).map(|entry| entry.0), condition.revision, condition.saved_revision)
        }).await.unwrap();
        drop(gate);
        drop(market);
        let reconcile = settlement.reconcile_once(&f.world);
        let mutate = async {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let blocked: bool = db.query_one(
                        "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)) AND wait_event_type='Lock') AS blocked",
                        &[&blocker],
                    ).await.unwrap().get("blocked");
                    if blocked { break; }
                    tokio::task::yield_now().await;
                }
            }).await.expect("recovery did not reach the selected capsule lock");
            assert_eq!(tx.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&target, &host]).await.unwrap(), 1);
            tx.commit().await.unwrap();
        };
        let (reconciled, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(reconcile, mutate)
        }).await.expect("capsule drift recovery did not finish");
        assert!(!reconciled);
        assert!(matches!(boarding::recovery(&db, &plan).await.unwrap(), Recovery::Unknown));
        let rows = load_items(&db, ItemQuery::Ids(&[old, target, cargo])).await.unwrap();
        let old_row = rows.iter().find(|row| row.item_id == old).unwrap();
        let target_row = rows.iter().find(|row| row.item_id == target).unwrap();
        let cargo_row = rows.iter().find(|row| row.item_id == cargo).unwrap();
        assert_eq!((old_row.type_id, old_row.owner_id, old_row.location_id, old_row.flag_id, old_row.singleton, old_row.quantity),
            (old_type, owner, host, FLAG_SHIP_HANGAR, true, 1));
        assert_eq!((target_row.type_id, target_row.owner_id, target_row.location_id, target_row.flag_id, target_row.singleton, target_row.quantity),
            (target_type, host, system, 0, true, 1));
        assert_eq!((cargo_row.type_id, cargo_row.owner_id, cargo_row.location_id, cargo_row.flag_id, cargo_row.quantity),
            (cargo_type, owner, cargo_ship, infinity_core::consts::FLAG_CARGO, 3));
        assert_eq!(crate::dao::characters::selection::ship_id(&db, owner).await.unwrap(), Some(target));
        assert!(frame.is_committed(&db).await.unwrap());
        let attrs_after = crate::dao::items::dogma::overrides(&db, old).await.unwrap();
        for (attribute, expected) in [(infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, old_damage.0),
            (infinity_core::consts::dogma::ATTR_HULL_DAMAGE, old_damage.1)] {
            assert!(attrs_after.iter().any(|row| row.attribute_id == attribute && (row.value - expected).abs() < 1e-8));
        }
        let actor_after = f.world.with(system, move |s| {
            let condition = s.combat_condition_snapshot(old).unwrap();
            (s.pilot_ball(owner).map(|ball| ball.id), s.persistent_generation(old),
                s.combat_phase(old), s.ship_condition(old).map(|v| (v.1, v.2)),
                s.observers.get(&client).map(|entry| entry.0), condition.revision, condition.saved_revision)
        }).await.unwrap();
        assert_eq!(actor_after, actor_before);
        assert!(settlement.receipt.lock().unwrap().is_some());
        let mut receiver = receiver;
        assert!(matches!(receiver.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        assert!(matches!(published.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
        assert_eq!(db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&target, &owner]).await.unwrap(), 1);
        drop(raw);
        let _ = connection.await;
        f.cleanup().await;
        return;
    }
    if missing_receipt {
        drop(gate);
        drop(market);
        assert!(!settlement.reconcile_once(&f.world).await, "business state and HP cannot replace the original receipt");
        assert_eq!(f.world.with(system, move |s| s.pilot_ball(owner).map(|ball| ball.id)).await.unwrap(), Some(old));
        db.transaction(|_, outbox: &mut crate::db::outbox::Outbox<()>| {
            let frame = frame.clone();
            Box::pin(async move {
                frame.append(outbox);
                Ok::<_, CallError>(())
            })
        }, |(), _| async move { Ok(()) }).await.unwrap();
        assert!(frame.is_committed(&db).await.unwrap());
        assert!(settlement.reconcile_once(&f.world).await);
        assert_eq!(receiver.await.unwrap().unwrap(), Some(target));
        assert_eq!(f.world.with(system, move |s| s.pilot_ball(owner).map(|ball| ball.id)).await.unwrap(), Some(target));
        f.cleanup().await;
        return;
    }
    if selective {
        let collateral = frame.condition_rows().find(|row| row.0 == host).expect("collateral frame condition");
        assert!(crate::dao::items::dogma::set_override(&db, host, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, collateral.1[1] + 1.0).await.unwrap());
        drop(gate);
        drop(market);
        assert!(settlement.reconcile_once(&f.world).await);
        assert_eq!(receiver.await.unwrap().unwrap(), Some(target));
        assert!(f.world.with(system, move |s| {
            let state = s.combat_condition_snapshot(host).unwrap();
            state.revision != state.saved_revision
        }).await.unwrap(), "unverified collateral must remain dirty");
        f.cleanup().await;
        return;
    }
    assert!(f.world.with(system, |_| -> () { panic!("stop actor before committed publication") }).await.is_none());
    handle.tx.closed().await;
    drop(gate);
    drop(market);
    if force_collision {
        f.world.system(system).await.unwrap();
        f.world.with(system, move |s| {
            let ball = s.ball(old).unwrap();
            let generation = s.persistent_generation(old).unwrap();
            assert_eq!((ball.id, ball.type_id, ball.owner_id, ball.char_id, ball.interactive),
                (old, old_type, owner, None, false));
            assert!(ball.free && ball.massive);
            let host_ball = s.ball(host).unwrap();
            assert!(host_ball.free && host_ball.massive);
            let separation = (ball.radius as f64 + host_ball.radius as f64) / 2.0;
            assert!(separation > 0.0 && separation.is_finite());
            for (id, pos) in [(old, [0.0; 3]), (host, [separation, 0.0, 0.0])] {
                s.command(id, move |ball, _, _| {
                    ball.stop();
                    ball.pos = pos;
                    ball.vel = [0.0; 3];
                    Vec::new()
                });
            }
            assert!(crate::space::collision::enabled());
            s.tick(crate::space::next_aligned_ms(s.now_ms(), crate::space::MILLISECONDS_PER_SECOND));
            let ball = s.ball(old).unwrap();
            assert_ne!(ball.pos, [0.0; 3]);
            assert_eq!((ball.id, ball.type_id, ball.owner_id, ball.char_id, ball.interactive),
                (old, old_type, owner, None, false));
            assert_eq!(s.persistent_generation(old), Some(generation));
        }).await.unwrap();
    }
    if replaced {
        f.world.system(system).await.unwrap();
        let prepared = balls::piloted(&f.db, f.sde.clone(), &proof_target, owner, 0.0).await.unwrap();
        let before = f.world.with(system, move |s| {
            prepared.install(s, |s, ball| s.add_ship(ball));
            assert!(s.set_condition_fraction_mask(target, 0b110, 0.25));
            (s.persistent_generation(target).unwrap(), s.ship_condition(target).unwrap())
        }).await.unwrap();
        assert!(!settlement.reconcile_once(&f.world).await);
        f.world.with(system, move |s| {
            assert_eq!(s.pilot_ball(owner).unwrap().id, target);
            assert_eq!(s.pilots().filter(|ball| ball.char_id == Some(owner)).count(), 1);
            assert_eq!(s.persistent_generation(target), Some(before.0));
            let after = s.ship_condition(target).unwrap();
            assert_eq!((after.1, after.2), (before.1.1, before.1.2));
        }).await.unwrap();
        drop(receiver);
        f.cleanup().await;
        return;
    }
    let mut receiver = Some(receiver);
    let mut replacement_rx = if session_changed {
        let mut newer = settlement.expected.clone();
        newer.session_id = newer.session_id.checked_add(1).unwrap();
        assert_ne!(newer.session_id, settlement.expected.session_id);
        let (replacement_tx, replacement_rx) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(owner, replacement_tx);
        f.world.set_online_session(owner, Some(newer.clone()));
        assert_eq!(f.world.online_session(owner), Some(newer));
        Some(replacement_rx)
    } else {
        None
    };
    if disconnected { drop(receiver.take()); }
    assert!(settlement.reconcile_once(&f.world).await);
    if let Some(replacement_rx) = replacement_rx.as_mut() {
        assert!(matches!(replacement_rx.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
    }
    if !disconnected && !session_changed {
        assert_eq!(receiver.take().unwrap().await.unwrap().unwrap(), Some(target));
        assert!(settlement.receipt.lock().unwrap().is_none());
        let snapshot = move |s: &mut System| {
            (
                s.pilot_ball(owner).map(|ball| ball.id),
                s.pilots().filter(|ball| ball.char_id == Some(owner)).count(),
                s.observers.get(&client).map(|entry| entry.0),
                s.persistent_generation(target),
                s.persistent_generation(old),
                s.ship_condition(target).map(|condition| (condition.1, condition.2)),
                s.ship_condition(old).map(|condition| (condition.1, condition.2)),
            )
        };
        let before = f.world.with(system, snapshot).await.unwrap();
        assert!(settlement.reconcile_once(&f.world).await);
        assert!(settlement.receipt.lock().unwrap().is_none());
        let after = f.world.with(system, snapshot).await.unwrap();
        assert_eq!(after, before);
        f.client.change_session(vec![("shipid", Value::Int(target))]);
        crate::services::ship::send_state(&mut f.client).await.unwrap();
        let packet = f.client.outbox.iter().rev().find(|packet| matches!(&packet.destination,
            crate::macho::Address::Broadcast { broadcast_id, idtype, .. }
                if broadcast_id.as_text().as_deref() == Some("DoDestinyUpdate")
                    && idtype.as_text().as_deref() == Some("clientID"))).unwrap();
        let envelope = packet.body[0].items().unwrap();
        let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
        let args = payload.items().unwrap()[1].items().unwrap();
        let event = &args[0].items().unwrap()[0];
        let call = event.items().unwrap()[1].items().unwrap();
        assert_eq!(call[0].as_text().as_deref(), Some("SetState"));
        let state_args = call[1].items().unwrap();
        let Value::Instance(_, bag) = &state_args[0] else { panic!("expected SetState bag") };
        let Value::Dict(fields) = &**bag else { panic!("expected SetState fields") };
        assert_eq!(fields.iter().find(|(key, _)| key == &Value::str("ego")).unwrap().1.as_int(), Some(target));
        f.world.with(system, move |s| {
            assert_eq!(s.pilot_ball(owner).unwrap().id, target);
            assert_eq!(s.pilots().filter(|ball| ball.char_id == Some(owner)).count(), 1);
            assert_eq!(s.observers.get(&client).unwrap().0, target);
            assert!(s.ship_condition(target).is_some());
            assert!(s.combat_health_scalars(target).is_some());
            if matches!(operation, Operation::Store) { assert!(s.ball(old).is_none()); }
            else {
                assert_eq!(s.ball(old).unwrap().char_id, None);
                let restored = s.ship_condition(old).unwrap();
                assert!((restored.1 - old_damage.0).abs() < 1e-8);
                assert!((restored.2 - old_damage.1).abs() < 1e-8);
            }
        }).await.unwrap();
    } else {
        if session_changed { assert!(receiver.take().unwrap().await.unwrap().is_err()); }
        f.world.with(system, move |s| {
            assert!(s.pilot_ball(owner).is_none());
            assert!(s.ball(target).is_none());
            if matches!(operation, Operation::Store) { assert!(s.ball(old).is_none()); }
            else { assert_eq!(s.ball(old).unwrap().char_id, None); }
        }).await.unwrap();
        let selected = crate::dao::characters::selection::selected(&f.db, owner, user).await.unwrap().unwrap();
        let prepared = crate::services::presence::prepare(&f.db, &f.world, &f.sde, owner, &selected, |_| Ok(())).await.unwrap();
        assert_eq!(prepared.ship_id, target);
        f.world.with(system, move |s| {
            assert_eq!(s.pilot_ball(owner).unwrap().id, target);
            assert_eq!(s.pilots().filter(|ball| ball.char_id == Some(owner)).count(), 1);
        }).await.unwrap();
    }
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn committed_selection_recovers_after_actor_stop() {
    for operation in [Operation::Board, Operation::Store, Operation::BoardStored] {
        recover_after_stop(operation, false, false, false, false, false, false, false, false).await;
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn disconnected_selection_defers_to_login_restoration() {
    for operation in [Operation::Board, Operation::Store, Operation::BoardStored] {
        recover_after_stop(operation, true, false, false, false, false, false, false, false).await;
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn replacement_pilot_and_condition_are_not_overwritten() {
    for operation in [Operation::Board, Operation::Store, Operation::BoardStored] {
        recover_after_stop(operation, false, true, false, false, false, false, false, false).await;
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn changed_session_defers_committed_selection_to_login() {
    for operation in [Operation::Board, Operation::Store, Operation::BoardStored] {
        recover_after_stop(operation, false, false, true, false, false, false, false, false).await;
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn collision_moved_old_hull_keeps_committed_recovery_valid() {
    for session_changed in [true, false] {
        for operation in [Operation::Board, Operation::BoardStored] {
            recover_after_stop(operation, false, false, session_changed, true, false, false, false, false).await;
        }
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn committed_recovery_acknowledges_only_verified_collateral() {
    for operation in [Operation::Board, Operation::Store, Operation::BoardStored] {
        recover_after_stop(operation, false, false, false, false, true, false, false, false).await;
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn stored_recovery_waits_for_original_frame_receipts() {
    for operation in [Operation::Store, Operation::BoardStored] {
        recover_after_stop(operation, false, false, false, false, false, true, false, false).await;
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn shared_worker_recovers_original_frame_after_postcommit_publication_error() {
    for operation in [Operation::Board, Operation::Store, Operation::BoardStored] {
        recover_after_stop(operation, false, false, false, false, false, false, true, false).await;
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn stored_recovery_refuses_selected_capsule_custody_drift() {
    recover_after_stop(Operation::Store, false, false, false, false, false, false, false, true).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn session_fence_refusal_thaws_all_reservations_before_any_write() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use crate::services::drones::tests::fixture::table_fingerprint;
    use crate::space::combat::CombatPhase;

    let mut f = Fixture::real_system().await;
    let owner = f.owner;
    let user = f.account;
    let system = f.system;
    let old = f.ship;
    let kind = crate::dao::items::inventory::type_id(&f.db, old).await.unwrap().unwrap();
    let target = creation::positioned(&f.db, kind, owner, system, [2.0, 0.0, 0.0]).await.unwrap();
    let row = crate::dao::items::space_ships::unpiloted(&f.db, system).await.unwrap().into_iter().find(|row| row.item_id == target).unwrap();
    let prepared = balls::unpiloted(&f.db, f.sde.clone(), &row).await.unwrap();
    f.world.with(system, move |s| prepared.install(s, |s, ball| s.add_persistent_ship(ball))).await.unwrap();
    let (notice_tx, notice_rx) = tokio::sync::mpsc::unbounded_channel();
    f.client.notify_tx = notice_tx.clone();
    f.world.register_pilot(owner, notice_tx);
    f.world.observe(system, f.client.login.client_id, old, f.client.destiny_tx.clone()).await.unwrap();
    f.client.session.apply(vec![("userid", Value::Int(user)), ("solarsystemid2", Value::Int(system))]);
    let session = infinity_core::session::OnlineSession::from_session(&f.client.session).unwrap();
    assert_eq!((session.user_id, session.session_id, session.character_id, session.ship_id, session.system_id),
        (Some(user), f.client.session.sid, owner, Some(old), Some(system)));
    f.world.set_online_session(owner, Some(session.clone()));
    assert_eq!(f.world.online_session(owner), Some(session.clone()));
    let market = crate::services::market::lock().await;
    let gate = f.world.lock_condition_snapshot(system).await.unwrap();
    let transition = f.world.with(system, move |s| s.begin_condition_transitions(&[old, target]).unwrap()).await.unwrap();
    let expected = session.clone();
    let mut newer = session;
    newer.session_id = newer.session_id.checked_add(1).unwrap();
    assert_ne!(newer.session_id, expected.session_id);
    let (replacement_tx, mut replacement_rx) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(owner, replacement_tx);
    f.world.set_online_session(owner, Some(newer.clone()));
    assert_eq!(f.world.online_session(owner), Some(newer));
    let tables = ["items", "item_attributes", "pending_reloads", "characters", "progress_events"];
    let mut before = Vec::new();
    for table in tables { before.push(table_fingerprint(&f.db, table).await.unwrap()); }
    let notices = f.client.outbox.len();
    let native_notices = notice_rx.len();
    let ran = Arc::new(AtomicBool::new(false));
    let worker_ran = ran.clone();
    let result = settle_space(f.world.clone(), f.db.clone(), f.sde.clone(), system, owner, f.client.login.client_id,
        expected, transition, market, gate,
        RecoveryPlan::Board { character: owner, user, system, old, old_type: kind, target, target_type: kind },
        move |_, _, _, _| {
            let worker_ran = worker_ran.clone();
            Box::pin(async move {
                worker_ran.store(true, Ordering::SeqCst);
                Err(crate::services::CallError::Internal(anyhow::anyhow!("unexpected transaction")))
            }) as crate::db::transaction::TxFuture<'_, (PreparedTransition, Vec<i64>), CallError>
        }).await;
    assert!(result.is_err());
    assert!(!ran.load(Ordering::SeqCst));
    for (index, table) in tables.into_iter().enumerate() {
        assert_eq!(before[index], table_fingerprint(&f.db, table).await.unwrap());
    }
    assert_eq!(f.client.outbox.len(), notices);
    assert!(!notice_rx.is_closed());
    assert_eq!(notice_rx.len(), native_notices);
    assert!(matches!(replacement_rx.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
    f.world.with(system, move |s| {
        assert_eq!(s.combat_phase(old), Some(CombatPhase::Live));
        assert_eq!(s.combat_phase(target), Some(CombatPhase::Live));
        assert_eq!(s.pilot_ball(owner).unwrap().id, old);
    }).await.unwrap();
    f.cleanup().await;
}
// Claim the entire plan in one actor turn, returning partial claims to the native retry loop.
async fn claim_board_callbacks(
    world: &Arc<World>, system: i64, expected: Vec<(i64, u64, u64)>,
) -> Arc<dyn Fn() -> crate::space::NpcFuture + Send + Sync> {
    for _ in 0..100 {
        let expected = expected.clone();
        let callback = world.with(system, move |s| {
            let claims = s.claim_pending_combat();
            let matching: Vec<_> = claims.iter().filter_map(|(id, pending)| match pending {
                crate::space::combat::PendingCombat::Transition { token, generation, callback, .. }
                    if expected.contains(&(*id, *token, *generation)) => Some(callback.clone()),
                _ => None,
            }).collect();
            if matching.len() == expected.len() && claims.len() == expected.len() {
                return matching.into_iter().next();
            }
            for (id, pending) in claims {
                match pending {
                    crate::space::combat::PendingCombat::Transition { token, generation, .. }
                    | crate::space::combat::PendingCombat::Dying { token, generation, .. } => {
                        s.release_pending_combat(id, token, generation);
                    }
                }
            }
            None
        }).await.expect("system actor");
        if let Some(callback) = callback { return callback; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("pending board callbacks were not retry-ready");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn board_pending_releases_group_for_retry_and_rejects_partial_ownership() {
    use crate::space::combat::CombatPhase;
    let mut f = Fixture::real_system().await;
    let (owner, user, system, old) = (f.owner, f.account, f.system, f.ship);
    let kind = crate::dao::items::inventory::type_id(&f.db, old).await.unwrap().unwrap();
    let target = creation::positioned(&f.db, kind, owner, system, [2.0, 0.0, 0.0]).await.unwrap();
    let other = creation::positioned(&f.db, kind, owner, system, [4.0, 0.0, 0.0]).await.unwrap();
    let row = crate::dao::items::space_ships::unpiloted(&f.db, system).await.unwrap().into_iter()
        .find(|row| row.item_id == target).unwrap();
    let prepared = balls::unpiloted(&f.db, f.sde.clone(), &row).await.unwrap();
    f.world.with(system, move |s| prepared.install(s, |s, ball| s.add_persistent_ship(ball))).await.unwrap();
    f.client.session.apply(vec![("userid", Value::Int(user)), ("solarsystemid2", Value::Int(system))]);
    let expected = infinity_core::session::OnlineSession::from_session(&f.client.session).unwrap();
    f.world.set_online_session(owner, Some(expected.clone()));
    let plan = RecoveryPlan::Board { character: owner, user, system, old, old_type: kind, target, target_type: kind };
    let mut transition = f.world.with(system, move |s| {
        let generation = s.combat_membership_generation(old).unwrap();
        let source = s.prepare_combat_source(old, None, None, None,
            crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
        let hit = s.apply_damage(old, [1.0, 0.0, 0.0, 0.0],
            crate::services::combat::damage::DamageSource { facts: Some(source), ..Default::default() }, false).unwrap().unwrap();
        assert!(hit.dealt > 0.0 && !hit.lethal);
        s.begin_condition_transitions(&[old, target]).unwrap()
    }).await.unwrap();
    let entries = transition.entries.clone();
    let frame = transition.facts.clone().unwrap();
    transition.disarm();
    let handle = f.world.systems.lock().unwrap().get(&system).unwrap().clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let settlement = Arc::new(BoardSettlement {
        world: Arc::downgrade(&f.world), db: f.db.clone(), sde: f.sde.clone(), system, character: owner,
        client: f.client.login.client_id, expected: expected.clone(), plan, entries: entries.clone(), facts: Some(frame.clone()), actor_gate: handle.gate.clone(),
        receipt: Mutex::new(Some((sender, Some(CallError::Internal(anyhow::anyhow!("original board error")))))),
    });
    let rows: Vec<_> = frame.condition_rows().collect();
    let attrs = [infinity_core::consts::dogma::ATTR_SHIELD_CHARGE, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, infinity_core::consts::dogma::ATTR_HULL_DAMAGE];
    assert!(crate::dao::items::dogma::save_conditions(&f.db, system, &attrs, &rows).await.unwrap().contains(&old));
    crate::dao::characters::selection::set_ship(&f.db, owner, target).await.unwrap();
    assert!(!frame.is_committed(&f.db).await.unwrap());
    assert!(!settlement.reconcile_once(&f.world).await, "matching business state and HP cannot replace receipts");
    assert!(f.world.with(system, move |s| {
        let state = s.combat_condition_snapshot(old).unwrap();
        state.revision != state.saved_revision
    }).await.unwrap());
    crate::dao::characters::selection::set_ship(&f.db, owner, other).await.unwrap();
    assert!(settlement.mark_pending(&f.world).await);
    settlement.clone().reconcile().await;
    let callback = claim_board_callbacks(&f.world, system, entries.iter()
        .map(|(token, state)| (state.item_id, *token, state.generation)).collect()).await;
    crate::dao::characters::selection::set_ship(&f.db, owner, old).await.unwrap();
    callback().await;
    let error = receiver.await.unwrap().unwrap_err();
    assert!(format!("{error:?}").contains("original board error"));
    f.world.with(system, move |s| {
        assert_eq!(s.combat_phase(old), Some(CombatPhase::Live));
        assert_eq!(s.combat_phase(target), Some(CombatPhase::Live));
    }).await.unwrap();

    let mut transition = f.world.with(system, move |s| s.begin_condition_transitions(&[old, target]).unwrap()).await.unwrap();
    let entries = transition.entries.clone();
    transition.disarm();
    let target_token = entries[1].0;
    let target_generation = entries[1].1.generation;
    let replacement = f.world.with(system, move |s| {
        assert!(s.cancel_combat_transition(target, target_token, target_generation));
        s.begin_combat_transition(target).unwrap().0
    }).await.unwrap();
    let (sender, _receiver) = tokio::sync::oneshot::channel();
    let settlement = Arc::new(BoardSettlement {
        world: Arc::downgrade(&f.world), db: f.db.clone(), sde: f.sde.clone(), system, character: owner,
        client: f.client.login.client_id, expected, plan, entries: entries.clone(), facts: None, actor_gate: handle.gate.clone(),
        receipt: Mutex::new(Some((sender, None))),
    });
    crate::dao::characters::selection::set_ship(&f.db, owner, other).await.unwrap();
    assert!(!settlement.mark_pending(&f.world).await);
    let old_token = entries[0].0;
    let old_generation = entries[0].1.generation;
    f.world.with(system, move |s| {
        assert_eq!(s.combat_phase(old), Some(CombatPhase::Transition { token: old_token }));
        assert_eq!(s.combat_phase(target), Some(CombatPhase::Transition { token: replacement }));
        assert!(s.claim_pending_combat().is_empty());
        assert!(s.cancel_combat_transition(old, old_token, old_generation));
        assert!(s.cancel_combat_transition(target, replacement, target_generation));
    }).await.unwrap();
    crate::dao::characters::selection::set_ship(&f.db, owner, old).await.unwrap();
    f.cleanup().await;
}
