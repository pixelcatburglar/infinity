//! One committed foreign refund observed by the private boarding worker.
use super::*;
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn committed_foreign_refund_stays_unknown_until_its_row_is_restored() {
    use crate::space::combat::CombatPhase;
    let mut f = Fixture::real_system().await;
    let result = async {
    let (owner, user, system, old) = (f.owner, f.account, f.system, f.ship);
    let kind = crate::dao::items::inventory::type_id(&f.db, old).await.unwrap().unwrap();
    let foreign_owner = f.add_pilot().await.0;
    let target = creation::positioned(&f.db, kind, foreign_owner, system, [2.0, 0.0, 0.0]).await.unwrap();
    let charge = f.sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id != Some(infinity_core::consts::CATEGORY_SHIP) && f.sde.is_tradable(ty.type_id)).unwrap().type_id;
    let external = creation::positioned(&f.db, kind, foreign_owner, system, [6.0, 0.0, 0.0]).await.unwrap();
    let stack = transfers::insert(&f.db, charge, foreign_owner, external, infinity_core::consts::FLAG_CARGO, infinity_core::consts::MAX_STACK_QUANTITY).await.unwrap();
    crate::dao::items::pending_reloads::insert(&f.db, foreign_owner, target, target, infinity_core::consts::FLAG_CARGO, charge, 40, infinity_core::consts::FLAG_CARGO, external).await.unwrap();
    let row = crate::dao::items::space_ships::unpiloted(&f.db, system).await.unwrap().into_iter().find(|row| row.item_id == target).unwrap();
    let prepared = balls::unpiloted(&f.db, f.sde.clone(), &row).await.unwrap();
    f.world.with(system, move |s| prepared.install(s, |s, ball| s.add_persistent_ship(ball))).await.unwrap();
    f.client.session.apply(vec![("userid", Value::Int(user)), ("solarsystemid2", Value::Int(system))]);
    let expected = infinity_core::session::OnlineSession::from_session(&f.client.session).unwrap();
    f.world.set_online_session(owner, Some(expected.clone()));
    let identity = crate::services::identity::Identity { char_id: owner, ship_id: Some(old), corp_id: 1, station_id: None, system_id: Some(system) };
    let mut transition = f.world.with(system, move |s| {
        let generation = s.combat_membership_generation(old).unwrap();
        let source = s.prepare_combat_source(old, None, None, None, crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
        let hit = s.apply_damage(old, [1.0, 0.0, 0.0, 0.0], crate::services::combat::damage::DamageSource { facts: Some(source), ..Default::default() }, false).unwrap().unwrap();
        assert!(hit.dealt > 0.0 && !hit.lethal);
        s.begin_condition_transitions(&[old, target]).unwrap()
    }).await.unwrap();
    let entries = transition.entries.clone();
    let tokens: Vec<_> = entries.iter().map(|(token, state)| (state.item_id, *token, state.generation)).collect();
    let frame = transition.facts.clone().expect("natural damage frame");
    assert!(frame.events().next().is_some(), "the captured frame has a combat fact");
    transition.disarm();
    let (sender, mut receiver) = tokio::sync::oneshot::channel();
    let handle = f.world.systems.lock().unwrap().get(&system).unwrap().clone();
    let states: Vec<_> = entries.iter().map(|(_, state)| state.clone()).collect();
    let db = f.db.clone();
    let (plan, created) = db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox<()>| {
        let identity = identity;
        let sde = f.sde.clone();
        let world = f.world.clone();
        let db = db.clone();
        let states = states.clone();
        let frame = frame.clone();
        Box::pin(async move { RecoveryPlan::foreign_proof_for_test(&db, &world, tx, &sde, outbox, &identity, user, system, old, kind, target, kind, [0.0, 0.0, 0.0], [2.0, 0.0, 0.0], &states, Some(&frame)).await })
    }, |value, _| async move { Ok(value) }).await.unwrap();
    let created = *created.first().expect("one committed refund row");
    let committed = f.db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&created]).await.unwrap().get::<_, i64>("quantity");
    assert!(frame.is_committed(&f.db).await.unwrap(), "the producer committed its original fact");
    let events = f.db.query("SELECT event_id::text AS id FROM game.progress_events WHERE subject_character_id=$1", &[&owner]).await.unwrap().len();
    f.db.execute("UPDATE game.items SET quantity=$2 WHERE item_id=$1", &[&created, &(committed - 1)]).await.unwrap();
    let settlement = Arc::new(BoardSettlement {
        world: Arc::downgrade(&f.world), db: f.db.clone(), sde: f.sde.clone(), system, character: owner, client: f.client.login.client_id, expected: expected.clone(), ready_at: 0, retained: Mutex::new(None), plan: plan.clone(), entries: entries.clone(), facts: Some(frame.clone()), actor_gate: handle.gate.clone(), receipt: Mutex::new(Some((sender, None))), admission: f.world.admission(system).unwrap(),
    });
    assert!(matches!(crate::dao::items::boarding::recovery(&f.db, &plan).await.unwrap(), Recovery::Unknown), "the changed refund row is unknown");
    assert!(settlement.mark_pending(&f.world).await);
    assert!(!settlement.reconcile_once(&f.world).await, "unknown recovery does not finish the worker");
    settlement.release(&f.world).await;
    assert!(matches!(receiver.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
    let pending = f.world.with(system, {
        let tokens = tokens.clone();
        move |s| {
            let mut phases = Vec::new();
            for (id, token, generation) in tokens {
                phases.push((s.combat_phase(id), s.combat_membership_generation(id), token, generation));
            }
            (phases, s.pilot_ball(owner).map(|ball| ball.id), s.ball(old).map(|ball| ball.char_id))
        }
    }).await.unwrap();
    for (phase, generation, token, expected_generation) in pending.0 {
        assert_eq!(phase, Some(CombatPhase::Transition { token }));
        assert_eq!(generation, Some(expected_generation));
    }
    assert_eq!(pending.1, Some(old), "the unknown worker leaves the original pilot");
    assert_eq!(pending.2, Some(Some(owner)), "the unknown worker leaves the old hull occupied");
    assert!(settlement.receipt.lock().unwrap().is_some(), "the pending receipt stays with the worker");
    assert_eq!(f.db.query("SELECT event_id::text AS id FROM game.progress_events WHERE subject_character_id=$1", &[&owner]).await.unwrap().len(), events, "the unknown worker adds no publication");
    let callback = claim_board_callbacks(&f.world, system, tokens).await;
    f.db.execute("UPDATE game.items SET quantity=$2 WHERE item_id=$1", &[&created, &committed]).await.unwrap();
    callback().await;
    assert_eq!(receiver.try_recv().unwrap().unwrap(), Some(target), "the restored refund lets the original worker finish once");
    assert!(matches!(receiver.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Closed)));
    assert!(settlement.receipt.lock().unwrap().is_none(), "the finished worker keeps no second receipt");
    assert_eq!(f.db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&created]).await.unwrap().get::<_, i64>("quantity"), committed);
    assert_eq!(f.db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&stack]).await.unwrap().get::<_, i64>("quantity"), infinity_core::consts::MAX_STACK_QUANTITY);
    assert_eq!(f.db.query("SELECT event_id::text AS id FROM game.progress_events WHERE subject_character_id=$1", &[&owner]).await.unwrap().len(), events, "convergence adds no second fact");
    let done = f.world.with(system, move |s| (
        s.combat_phase(old), s.combat_phase(target), s.pilot_ball(owner).map(|ball| ball.id), s.ball(old).map(|ball| ball.char_id),
    )).await.unwrap();
    assert_eq!(done.0, Some(CombatPhase::Live));
    assert_eq!(done.1, Some(CombatPhase::Live));
    assert_eq!(done.2, Some(target));
    assert_eq!(done.3, Some(None));
    };
    let outcome = crate::tests::catch_scenario(result).await;
    f.cleanup().await;
    outcome.unwrap();
}
