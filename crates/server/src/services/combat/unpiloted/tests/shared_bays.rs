//! Mixed-owner free-hull loss destroys the complete contents tree.
use super::*;
use infinity_core::{consts::{GROUP_CARGO_CONTAINER, FLAG_CARGO, FLAG_FITTING_FIRST, FLAG_FLEET_HANGAR, FLAG_SHIP_HANGAR, dogma::{ATTR_FLEET_HANGAR_CAPACITY, ATTR_SHIP_MAINTENANCE_BAY_CAPACITY}}};
use crate::{dao::{insurance, items::{charges, combat, inventory, loading::{load_items, ItemQuery}, transfers}}, services::{drones::tests::fixture::Fixture, ship::balls}};
use crate::dao::{items::service_life, repairable_ship::ItemId};
use crate::services::{repairable_ship::deadlines, tests::fixture as recovery_fixture};

async fn item(db: &Db, id: i64) -> Option<(i32, i64, i64, i32, i64, bool)> {
    load_items(db, ItemQuery::Id(id)).await.unwrap().into_iter().next().map(|r| (r.type_id, r.owner_id, r.location_id, r.flag_id, r.quantity, r.singleton))
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn mixed_owner_unpiloted_host_loss_destroys_whole_tree() {
    let mut f = Fixture::new().await;
    let (guest, outside) = f.add_pilot().await;
    let (module_type, charge_type) = crate::services::combat::targeting_charges::tests::compatible_ammunition(&f.sde);
    let ty = f.sde.types.get(module_type).unwrap();
    assert!(f.sde.repairable_ship.enters_recovery(module_type, ty.group_id.unwrap(), ty.category_id.unwrap())
        && !f.sde.repairable_ship.unrepairable(ty.category_id.unwrap(), ty.group_id.unwrap(), module_type));
    let container = f.sde.types.iter().find(|t| t.published == Some(true) && t.group_id == Some(GROUP_CARGO_CONTAINER)
        && t.volume.is_some_and(|v| v.is_finite() && v > 0.0)).expect("published container with volume");
    let container_type = container.type_id;
    let fleet_volume = container.volume.unwrap() + 3.0 * f.sde.types.get(charge_type).unwrap().volume.unwrap();
    let host_kind = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.group_id.is_some_and(|group| f.sde.repairable_ship.enters_recovery(t.type_id, group, CATEGORY_SHIP)
            && !f.sde.repairable_ship.unrepairable(CATEGORY_SHIP, group, t.type_id))
        && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, ATTR_SHIP_MAINTENANCE_BAY_CAPACITY].iter().all(|id| t.attribute(*id).is_some_and(|v| v.is_finite() && v > 0.0))
        && t.attribute(ATTR_FLEET_HANGAR_CAPACITY).is_some_and(|v| v.is_finite() && v >= fleet_volume)).expect("published ship with health, resonances and both bays");
    let host_type = host_kind.type_id;
    let maintenance_capacity = host_kind.attribute(ATTR_SHIP_MAINTENANCE_BAY_CAPACITY).unwrap();
    let nested_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.volume.is_some_and(|v| v.is_finite() && v > 0.0 && v <= maintenance_capacity)
        && crate::services::inventory::holds::accepts(&f.sde, FLAG_SHIP_HANGAR, t.type_id)).expect("published ship fitting the maintenance bay").type_id;
    let db = f.db.clone();
    let world = f.world.clone();
    let owner = f.owner;
    let system = f.system;
    let host = creation::positioned(&db, host_type, owner, system, [100.0, 0.0, 0.0]).await.unwrap();
    let nested = creation::rookie_ship(&db, nested_type, guest, host, FLAG_SHIP_HANGAR).await.unwrap();
    let container = creation::rookie_ship(&db, container_type, guest, host, FLAG_FLEET_HANGAR).await.unwrap();
    let own = transfers::insert(&db, charge_type, owner, host, FLAG_FLEET_HANGAR, 3).await.unwrap();
    let cargo = transfers::insert(&db, charge_type, guest, container, FLAG_CARGO, 7).await.unwrap();
    let module = creation::rookie_ship(&db, module_type, guest, nested, FLAG_FITTING_FIRST).await.unwrap();
    let slave = creation::rookie_ship(&db, module_type, guest, nested, FLAG_FITTING_FIRST + 1).await.unwrap();
    let root_module = creation::rookie_ship(&db, module_type, owner, host, FLAG_FITTING_FIRST).await.unwrap();
    let root_slave = creation::rookie_ship(&db, module_type, owner, host, FLAG_FITTING_FIRST + 1).await.unwrap();
    let foreign_fit = creation::rookie_ship(&db, module_type, guest, host, FLAG_FITTING_FIRST + 2).await.unwrap();
    let cargo_container = creation::rookie_ship(&db, container_type, owner, host, FLAG_CARGO).await.unwrap();
    let cargo_module = creation::rookie_ship(&db, module_type, owner, cargo_container, FLAG_FITTING_FIRST).await.unwrap();
    let foreign_leaf = creation::rookie_ship(&db, module_type, owner, container, FLAG_FITTING_FIRST).await.unwrap();
    let drone_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_DRONE)
        && t.group_id.is_some_and(|group| f.sde.repairable_ship.enters_recovery(t.type_id, group, infinity_core::consts::CATEGORY_DRONE)
            && !f.sde.repairable_ship.unrepairable(infinity_core::consts::CATEGORY_DRONE, group, t.type_id))).unwrap().type_id;
    let bay_drone = creation::rookie_ship(&db, drone_type, owner, host, infinity_core::consts::FLAG_DRONE_BAY).await.unwrap();
    let bay_prior = f.sde.repairable_ship.max_life() / 4.0;
    assert!(service_life::set_if(&db, ItemId::new(bay_drone).unwrap(), None, bay_prior).await.unwrap());
    let outside_module = creation::rookie_ship(&db, module_type, guest, outside, FLAG_FITTING_FIRST).await.unwrap();
    let outside_stack = transfers::insert(&db, charge_type, guest, outside, FLAG_CARGO, 11).await.unwrap();
    for (ship, master, bank_slave) in [(host, root_module, root_slave), (nested, module, slave)] {
        combat::apply_loads(&db, ship, &[(master, FLAG_FITTING_FIRST, charge_type, 4, 0)]).await.unwrap();
        combat::move_bank(&db, ship, master, bank_slave).await.unwrap();
    }
    insurance::insure(&db, host, owner, host_type, 1.0, 1, i64::MAX).await.unwrap();
    insurance::insure(&db, nested, guest, nested_type, 0.5, 1, i64::MAX).await.unwrap();
    let root_reload = pending_reloads::insert(&db, owner, host, root_module, FLAG_FITTING_FIRST, charge_type, 2, FLAG_CARGO, container).await.unwrap();
    let nested_reload = pending_reloads::insert(&db, guest, nested, module, FLAG_FITTING_FIRST, charge_type, 3, FLAG_CARGO, container).await.unwrap();
    let outside_reload = pending_reloads::insert(&db, guest, outside, outside_module, FLAG_FITTING_FIRST, charge_type, 5, FLAG_CARGO, container).await.unwrap();
    let survivor_reload = pending_reloads::insert(&db, guest, outside, outside_module, FLAG_FITTING_FIRST, charge_type, 1, FLAG_CARGO, outside).await.unwrap();
    let before_outside = item(&db, outside).await;
    let before_stack = item(&db, outside_stack).await;
    let before_outside_module = item(&db, outside_module).await;
    let before_pending: Vec<_> = pending_reloads::for_ship(&db, outside).await.unwrap().into_iter().filter(|r| r.reload_id == survivor_reload).collect();
    let killmails = db.query_one("SELECT count(*) AS n FROM game.killmails", &[]).await.unwrap().get::<_, i64>("n");
    let row = crate::dao::items::space_ships::FreeShip { item_id: host, type_id: host_type, owner_id: owner, corporation_id: 1, alliance_id: -1, pos: [100.0, 0.0, 0.0] };
    let prepared = balls::unpiloted(&db, f.sde.clone(), &row).await.ok().unwrap();
    {
        let market = crate::services::market::lock().await;
        world.add_persistent_ship_locked(system, prepared, &market).await.unwrap();
    }
    let d = db.clone();
    let owner_balance = crate::dao::wallets::characters::balance(&db, owner).await.unwrap();
    let guest_balance = crate::dao::wallets::characters::balance(&db, guest).await.unwrap();
    let w = world.clone();
    let recovery_kinds = [deadlines::PAYMENT, deadlines::DELIVERY, deadlines::WITHDRAW];
    let before_jobs: std::collections::BTreeSet<String> = db.query("SELECT job_id FROM game.jobs WHERE kind=ANY($1)", &[&recovery_kinds.as_slice()])
        .await.unwrap().iter().map(|r| r.get("job_id")).collect();
    let result = tokio::spawn(async move {
        assert!(w.ball(system, host).await.unwrap().char_id.is_none());
        assert!(w.with(system, move |s| s.persistent_generation(host).is_some()).await.unwrap());
        assert!(w.with(system, move |s| s.combat_condition_snapshot(host).is_some()).await.unwrap());
        let generation = crate::space::targeting::next_generation();
        let weak = std::sync::Arc::downgrade(&w);
        let mut attempt = w.with(system, move |s| {
            let facts = s.prepare_combat_source(outside, None, None, None,
                crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1,
                move |s| s.pilot_ball(guest).is_some_and(|b| b.id == outside)).unwrap();
            crate::services::effects::space_source::PreparedCombatAttempt::new(facts, weak, system)
        }).await.unwrap();
        let facts = attempt.facts();
        let outcome = damage::apply_damage_guarded(&d, &w, system, host, [1e30, 0.0, 0.0, 0.0],
            damage::DamageSource { attacker_id: Some(guest), attacker_ball_id: Some(outside),
                source_ship_id: Some(outside), source_ship_type: Some(facts.entity.type_id),
                facts: Some(facts), ..Default::default() }, false, |_| true).await.unwrap().unwrap();
        w.with(system, move |s| {
            s.finish_combat_attempt(&facts);
            s.retire_combat_source(generation).unwrap();
        }).await.unwrap();
        attempt.disarm();
        assert!(outcome.dealt > 0.0 && outcome.lethal);
        assert!(w.ball(system, host).await.is_none());
        let group: i64 = d.query_one("SELECT group_id FROM game.repairable_recovery_groups WHERE beneficiary_id=$1 AND source_item_id=$2", &[&owner, &host]).await.unwrap().get("group_id");
        for (id, ty, flag) in [(host, host_type, 0), (root_module, module_type, FLAG_FITTING_FIRST), (root_slave, module_type, FLAG_FITTING_FIRST + 1), (bay_drone, drone_type, infinity_core::consts::FLAG_DRONE_BAY)] {
            assert_eq!(item(&d, id).await, Some((ty, infinity_core::consts::NPC_CORP_INTERBUS, group, flag, 1, true)));
            assert!(d.query_opt("SELECT 1 FROM game.repairable_recovery_items WHERE group_id=$1 AND item_id=$2", &[&group, &id]).await.unwrap().is_some());
        }
        let captured = d.transaction(|tx, _: &mut ()| Box::pin(async move {
            Ok::<_, crate::db::Error>(recovery_fixture::capture_recovery_jobs(tx, owner).await)
        }), |value, ()| async move { Ok(value) }).await.unwrap();
        let member_ids: Vec<i64> = d.query("SELECT member_id FROM game.repairable_recovery_items WHERE group_id=$1 AND original_owner_id=$2 AND item_id=ANY($3) ORDER BY member_id",
            &[&group, &owner, &[host, root_module, root_slave, bay_drone].as_slice()]).await.unwrap().iter().map(|r| r.get("member_id")).collect();
        assert_eq!(captured.0, member_ids);
        let after_jobs: std::collections::BTreeSet<String> = d.query("SELECT job_id FROM game.jobs WHERE kind=ANY($1)", &[&recovery_kinds.as_slice()])
            .await.unwrap().iter().map(|r| r.get("job_id")).collect();
        assert_eq!(after_jobs, before_jobs.into_iter().chain(captured.1).collect());
        assert_eq!(service_life::get_many(&d, &[ItemId::new(bay_drone).unwrap()]).await.unwrap()[&ItemId::new(bay_drone).unwrap()], bay_prior);
        for id in [nested, container, own, cargo, module, slave, foreign_fit, cargo_container, cargo_module, foreign_leaf] {
            assert!(item(&d, id).await.is_none(), "loss tree row {id} survives");
            assert!(d.query("SELECT 1 FROM game.repairable_recovery_items WHERE item_id=$1", &[&id]).await.unwrap().is_empty());
        }
        assert!(d.query("SELECT 1 FROM game.repairable_recovery_groups WHERE beneficiary_id=$1", &[&guest]).await.unwrap().is_empty());
        assert!(pending_reloads::for_ship(&d, host).await.unwrap().is_empty(), "destroyed weapon reservation {root_reload} must not be refunded");
        assert!(charges::all(&d, host).await.unwrap().is_empty());
        assert!(charges::banks(&d, host).await.unwrap().is_empty());
        assert!(insurance::contract(&d, host).await.unwrap().is_none());
        assert!(charges::all(&d, nested).await.unwrap().is_empty());
        assert!(charges::banks(&d, nested).await.unwrap().is_empty());
        assert!(insurance::contract(&d, nested).await.unwrap().is_none());
        assert!(pending_reloads::for_ship(&d, nested).await.unwrap().is_empty(), "destroyed weapon reservation {nested_reload} must not survive");
        let outside_after = pending_reloads::for_ship(&d, outside).await.unwrap();
        assert!(!outside_after.iter().any(|r| r.reload_id == outside_reload), "reservation sourced inside the loss tree must not survive");
        assert!(outside_after == before_pending && outside_after.iter().any(|r| r.reload_id == survivor_reload));
        assert_eq!(item(&d, outside).await, before_outside);
        assert_eq!(item(&d, outside_module).await, before_outside_module);
        assert_eq!(crate::dao::wallets::characters::balance(&d, owner).await.unwrap(), owner_balance);
        assert_eq!(crate::dao::wallets::characters::balance(&d, guest).await.unwrap(), guest_balance);
        assert_eq!(d.query_one("SELECT count(*) AS n FROM game.killmails", &[]).await.unwrap().get::<_, i64>("n"), killmails);
        assert_eq!(item(&d, outside_stack).await, before_stack);
        assert!(load_items(&d, ItemQuery::Location(host)).await.unwrap().is_empty());
        assert!(load_items(&d, ItemQuery::Location(container)).await.unwrap().is_empty());
    }).await;
    // These metadata rows are keyed to a deleted direct child and the normal
    // fixture cleanup cannot discover them by joining to existing item rows.
    combat::delete_charge(&db, nested, FLAG_FITTING_FIRST).await.unwrap();
    combat::clear_banks(&db, nested).await.unwrap();
    pending_reloads::take(&db, nested_reload).await.unwrap();
    pending_reloads::take(&db, outside_reload).await.unwrap();
    pending_reloads::take(&db, survivor_reload).await.unwrap();
    pending_reloads::take(&db, root_reload).await.unwrap();
    insurance::uninsure(&db, nested, guest).await.unwrap();
    insurance::uninsure(&db, host, owner).await.unwrap();
    // If the assertion task failed before destruction, remove the host metadata too.
    combat::delete_charge(&db, host, FLAG_FITTING_FIRST).await.unwrap();
    combat::clear_banks(&db, host).await.unwrap();
    crate::services::tests::fixture::cleanup_recovery(&db, owner).await.unwrap();
    for id in [cargo, module, slave, root_module, root_slave, outside_module, own, container, nested, host, foreign_fit, cargo_container, cargo_module, foreign_leaf, bay_drone] { inventory::delete(&db, id).await.unwrap(); }
    world.remove_ball(system, host).await;
    forget_ship(host);
    f.cleanup().await;
    result.unwrap();
}
