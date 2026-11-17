use super::*;
use crate::space::combat_facts::Refusal;
use crate::services::{crimewatch::control, effects::tests::fixture::{admit, ball}};

#[test]
fn capacity_refusal_is_user_visible_but_replay_is_benign() {
    crate::tests::init_config();
    assert!(matches!(fact_refusal(1, Refusal::Full), Some(CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY));
    assert!(fact_refusal(1, Refusal::Replay).is_none());
    assert!(matches!(fact_refusal(1, Refusal::Overflow), Some(CallError::Internal(_))));
}

async fn hit(world: &Arc<World>, db: &Db, system: i64, target: i64, source: i64, raw: [f64; 4], forced: bool) -> Option<DamageOutcome> {
    let facts = world.with(system, move |s| s.prepare_combat_source(source, None, None, Some(1), crate::lua::events::resources::FactOrigin::Natural, crate::space::targeting::next_generation(), 1, 1, |_| true)).await.unwrap().unwrap();
    apply_damage_guarded(db, world, system, target, raw, DamageSource { facts: Some(facts), force_origin: forced.then_some(crate::lua::events::resources::FactOrigin::Natural), ..Default::default() }, false, |_| true).await.unwrap()
}
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn npc_contacts_fire_only_on_accepted_npc_damage() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let (system, victim, npc) = (sde.map.systems().next().unwrap().solar_system_id, fixture.character, fixture.character + 1);
    let result = crate::tests::catch_scenario(async {
        let world = crate::space::World::new(db.clone(), sde.clone(), 1);
        let ship_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
        world.add_ship(system, ball(npc, ship_type, 1, None, 0.0)).await.unwrap();
        assert!(world.with(system, move |s| s.add_npc(npc, crate::space::NpcBrain::new(None, crate::space::NpcConfig { fly_range: 1.0, aggro_range: 0.0, rate_of_fire_ms: 1e9, scan_resolution: 1.0, repairs: (0.0, 0.0, 1000.0) }, std::sync::Arc::new(|_, _| Some(Box::pin(async {})))))).await.unwrap());
        let ship = crate::dao::items::creation::positioned(&db, ship_type, victim, system, [0.0; 3]).await.unwrap();
        admit(&db, &world, system, ball(ship, ship_type, victim, Some(victim), 0.0)).await;
        apply_damage_guarded(&db, &world, system, ship, [0.0; 4], DamageSource { attacker_id: Some(npc), ..Default::default() }, false, |_| true).await.unwrap();
        hit(&world, &db, system, ship, npc, [1.0, 0.0, 0.0, 0.0], true).await;
        assert!(control::snapshot(&db, victim).await.unwrap().npc.is_none());
        assert!(hit(&world, &db, system, ship, npc, [1.0, 0.0, 0.0, 0.0], false).await.unwrap().dealt > 0.0 && control::snapshot(&db, victim).await.unwrap().npc.is_some());
        control::set_timer(&db, &world, victim, control::TimerChange { kind: control::TimerKind::Npc, duration_ms: 0 }).await.unwrap();
        assert!(hit(&world, &db, system, npc, ship, [1e30; 4], false).await.unwrap().lethal && control::snapshot(&db, victim).await.unwrap().npc.is_some());
    }).await;
    db.execute("DELETE FROM game.map_events WHERE source=$1 AND solar_system_id=$2", &[&(crate::dao::map_telemetry::EventSource::NpcOccurrence as i16), &system]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

use crate::dao::{items::{creation, inventory, repairable_ship as physical}, repairable_ship as recovery};
use crate::services::{drones::tests::fixture::Fixture, ship::loss::facts::PlayerLossContext};
use infinity_core::consts::{FLAG_FITTING_FIRST, NPC_CORP_INTERBUS};
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn recovery_proof_checks_all_old_members_and_keeps_drift_dying() {
    let mut f = Fixture::real_system().await;
    let (guest, new_id) = f.add_pilot().await;
    let result = crate::tests::catch_scenario(async {
        let (db, world, root, owner, system) = (&f.db, &f.world, f.ship, f.owner, f.system);
        let shape = physical::capture_many(db, &[recovery::ItemId::new(root).unwrap()]).await.unwrap().remove(0);
        let child = creation::rookie_ship(db, shape.type_id, owner, root, FLAG_FITTING_FIRST).await.unwrap();
        let snapshot = world.with(system, move |s| s.apply_damage(root, [0.0; 4],
            DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true)
            .unwrap().unwrap().first_lethal.unwrap()).await.unwrap();
        let pending = Arc::new(PlayerLossContext::new(None, [0.0; 3]));
        let (token, generation) = (snapshot.token, snapshot.generation);
        assert!(world.with(system, { let pending = pending.clone(); move |s| s.install_player_loss_context(root, token, generation, pending) }).await.unwrap());
        assert!(reconcile_locked(db, world, system, &snapshot).await.is_err());
        assert_eq!(world.with(system, move |s| s.combat_phase(root)).await.flatten(), Some(crate::space::combat::CombatPhase::Dying { token }));
        let source = recovery::LossSource { kind: recovery::SourceKind::PilotedShip, beneficiary: owner, shape: shape.clone(),
            system, lethal_at: snapshot.lethal_at, sampled_position: [1.0; 3] };
        let seed = pending.recovery_seed;
        pending.remember_source(source.clone()).unwrap();
        let group = recovery::insert_group(db, &recovery::NewGroup { loss: source.clone(), loss_id: Some(root),
            destination_station: f.sde.map.stations().next().unwrap().station_id, payment_expires_at: source.lethal_at + infinity_core::consts::FILETIME_DAY }).await.unwrap();
        let shapes = physical::capture_many(db, &[shape.item, recovery::ItemId::new(child).unwrap()]).await.unwrap();
        let items: Vec<_> = shapes.iter().map(|shape| recovery::NewItem { group, item: shape.item, original_owner: owner, type_id: shape.type_id,
            flag: shape.flag, quantity: shape.quantity, singleton: shape.singleton, last_service_life: 1.0, repair_completed_at: source.lethal_at }).collect();
        recovery::insert_items(db, &items).await.unwrap();
        let members = recovery::members_of(db, group).await.unwrap();
        let member = members.iter().find(|m| m.item.get() == child).unwrap().id;
        db.execute("UPDATE game.items SET owner_id=$2,location_id=$3 WHERE item_id=ANY($1)",
            &[&[root, child].as_slice(), &NPC_CORP_INTERBUS, &group.get()]).await.unwrap();
        assert_eq!(recovery_proof(db, &source).await.unwrap(), (true, false));
        let edits: [fn(&mut recovery::LossSource); 4] = [
            |s| s.kind = recovery::SourceKind::Drone,
            |s| s.shape.owner += 1,
            |s| s.lethal_at += 1,
            |s| s.shape.x = Some(s.sampled_position[0]),
        ];
        for edit in edits {
            let mut changed = source.clone();
            edit(&mut changed);
            assert_eq!(recovery_proof(db, &changed).await.unwrap(), (false, false));
            assert!(pending.remember_source(changed).is_err());
        }
        assert_eq!(pending.recovery_seed, seed);
        assert_eq!(pending.recovery_source.lock().unwrap().as_ref(), Some(&source));
        db.execute("UPDATE game.repairable_recovery_items SET original_owner_id=$2 WHERE member_id=$1", &[&member.get(), &guest]).await.unwrap();
        assert_eq!(recovery_proof(db, &source).await.unwrap(), (false, false));
        db.execute("UPDATE game.repairable_recovery_items SET original_owner_id=$2 WHERE member_id=$1", &[&member.get(), &owner]).await.unwrap();
        for root_missing in [false, true] {
            if root_missing {
                db.execute("DELETE FROM game.repairable_recovery_items WHERE group_id=$1 AND item_id=$2", &[&group.get(), &root]).await.unwrap();
                inventory::delete(db, root).await.unwrap();
            }
            assert_eq!(recovery_proof(db, &source).await.unwrap(), (true, false));
            for (column, value) in [("owner_id", owner), ("location_id", group.get() + 1)] {
                db.execute(&format!("UPDATE game.items SET {column}=$2 WHERE item_id=$1"), &[&child, &value]).await.unwrap();
                assert_eq!(recovery_proof(db, &source).await.unwrap(), (false, false));
                assert!(reconcile_locked(db, world, system, &snapshot).await.is_err());
                assert_eq!(world.with(system, move |s| s.combat_phase(root)).await.flatten(), Some(crate::space::combat::CombatPhase::Dying { token }));
                db.execute("UPDATE game.items SET owner_id=$2,location_id=$3 WHERE item_id=$1", &[&child, &NPC_CORP_INTERBUS, &group.get()]).await.unwrap();
            }
            inventory::delete(db, child).await.unwrap();
            assert_eq!(recovery_proof(db, &source).await.unwrap(), (false, false));
            db.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,1,true)",
                &[&child, &shape.type_id, &NPC_CORP_INTERBUS, &group.get(), &FLAG_FITTING_FIRST]).await.unwrap();
        }
        assert!(recovery::terminalize(db, &[(member, Some(recovery::ItemId::new(new_id).unwrap()))], &[recovery::Status::AwaitingPayment], recovery::Status::AwaitingPayment, source.lethal_at).await.is_err());
        recovery::start_delivery(db, source.lethal_at, 1, 1, &[recovery::Payment { member, amount: crate::isk::IskAmount::ZERO }]).await.unwrap();
        recovery::terminalize(db, &[(member, Some(recovery::ItemId::new(new_id).unwrap()))], &[recovery::Status::Delivering], recovery::Status::Withdrawn, source.lethal_at + 3).await.unwrap();
        assert_eq!(recovery_proof(db, &source).await.unwrap(), (false, false));
        inventory::delete(db, child).await.unwrap();
        assert_eq!(recovery_proof(db, &source).await.unwrap(), (true, false));
        db.execute("UPDATE game.repairable_recovery_items SET withdrawn_item_id=$2 WHERE member_id=$1", &[&member.get(), &child]).await.unwrap();
        assert_eq!(recovery_proof(db, &source).await.unwrap(), (false, false));
        db.execute("UPDATE game.repairable_recovery_items SET withdrawn_item_id=$2 WHERE member_id=$1", &[&member.get(), &new_id]).await.unwrap();
        let new_shape = physical::capture_many(db, &[recovery::ItemId::new(new_id).unwrap()]).await.unwrap().remove(0);
        assert_ne!(new_shape.owner, owner);
        assert_ne!(new_shape.location, group.get());
        assert_eq!(recovery_proof(db, &source).await.unwrap(), (true, false));
        inventory::delete(db, new_id).await.unwrap();
        assert_eq!(recovery_proof(db, &source).await.unwrap(), (true, false));
        for status in [recovery::Status::Abandoned, recovery::Status::Expired] {
            db.execute("UPDATE game.repairable_recovery_items SET status=$2,paid_at=NULL,paid_amount=NULL,delivery_completed_at=NULL,withdraw_expires_at=NULL,withdrawn_item_id=NULL,finished_at=$3 WHERE member_id=$1",
                &[&member.get(), &(status as i16), &source.lethal_at]).await.unwrap();
            assert_eq!(recovery_proof(db, &source).await.unwrap(), (true, false));
            assert!(recovery::terminalize(db, &[(member, Some(recovery::ItemId::new(new_id).unwrap()))], &[status], status, source.lethal_at).await.is_err());
        }
        db.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z,brain_slot,jump_clone_id) SELECT source_item_id,source_type_id,source_owner_id,source_location_id,source_flag_id,source_quantity,source_singleton,source_x,source_y,source_z,source_brain_slot,source_jump_clone_id FROM game.repairable_recovery_groups WHERE group_id=$1", &[&group.get()]).await.unwrap();
        db.execute("DELETE FROM game.repairable_recovery_groups WHERE group_id=$1", &[&group.get()]).await.unwrap();
        assert_eq!(recovery_proof(db, &source).await.unwrap(), (false, true));
        db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&root, &group.get()]).await.unwrap();
        assert_eq!(recovery_proof(db, &source).await.unwrap(), (false, false));
        inventory::delete(db, root).await.unwrap();
        assert_eq!(recovery_proof(db, &source).await.unwrap(), (true, false));
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
