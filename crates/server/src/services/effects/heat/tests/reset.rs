//! Reset fences across the market lock and the owning system actor.
use std::{future::Future, sync::{Arc, atomic::AtomicBool}, task::{Context, Poll, Waker}};
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_MED_SLOT_FIRST, dogma::{ATTR_HULL_DAMAGE, ATTR_STRUCTURE_HP, ATTR_LOW_SLOTS, ATTR_MED_SLOTS, ATTR_HI_SLOTS, ATTR_RIG_SLOTS, EFFECT_MICROWARPDRIVE}};
use marshal::Value;
use crate::services::{combat::{self, ActiveModule, Ctx}, effects::tests::fixture, market, tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation}};
use super::super::{self as heat, Source, SourceSnapshot};

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn committed_heal_precedes_waiting_heat_damage_in_actor_fifo() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), 1).await;
    let world = client.world.clone();
    let sde = world.sde();
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(heat::CAPACITY[1]).is_some_and(|v| v > 0.0)).unwrap().type_id;
    let med_slot = sde.dogma.effects_by_name("medPower").next().unwrap().effect_id;
    let module = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v > 1.0)
        && t.attribute(heat::HEAT_DAMAGE).is_some_and(|v| v > 0.0)
        && t.effects().iter().any(|effect| effect.effect_id == med_slot)).unwrap().type_id;
    let f = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[ShipItemSpec { type_id: module, flag_id: FLAG_MED_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = f.module();
    for (attr, value) in [(ATTR_LOW_SLOTS, 0.0), (ATTR_MED_SLOTS, 1.0), (ATTR_HI_SLOTS, 0.0), (ATTR_RIG_SLOTS, 0.0)] {
        crate::dao::items::dogma::set_override(&db, f.ship, attr, value).await.unwrap();
    }
    crate::dao::items::dogma::set_override(&db, module_id, ATTR_HULL_DAMAGE, 0.0).await.unwrap();
    client.session.apply(vec![("charid", Value::Int(f.character)), ("shipid", Value::Int(f.ship)), ("solarsystemid2", Value::Int(1))]);
    let expected = infinity_core::session::OnlineSession::from_session(&client.session).unwrap();
    let _notices = crate::services::slash::tests::support::register_online(&world, expected.clone());
    fixture::admit(&db, &world, 1, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
    let capture = expected.clone();
    let (membership, gate) = world.with(1, move |s| s.capture_space_heal(&capture)).await.flatten().unwrap();
    let fit = crate::services::fit::Fit::load_in_space(&db, &world, 1, f.character, f.ship).await.unwrap();
    let heat_damage = fit.value(crate::services::fit::Key::Module(module_id), heat::HEAT_DAMAGE);
    assert!(heat_damage > 0.0);
    let stop = Arc::new(AtomicBool::new(false));
    combat::state().active.insert(module_id, ActiveModule { char_id: f.character, stop: stop.clone(), forced_inactive: false });
    let generation = stop.clone();
    let old = world.with(1, move |s| {
        let at = s.now_ms();
        super::seed_ship_heat(s, f.ship, f.character, &fit, [0.0; 3], at);
        let rack = &mut s.heat.ships.get_mut(&f.ship).unwrap().racks[1];
        assert!(rack.capacity > 0.0);
        rack.heat = rack.capacity;
        rack.anchor_heat = rack.heat;
        rack.sources.insert(module_id, Source { flag: FLAG_MED_SLOT_FIRST, rate: 0.0, damage: heat_damage, suspended: false, stopping: false, generation: generation.clone() });
        SourceSnapshot { module: module_id, master: module_id, flag: FLAG_MED_SLOT_FIRST, heat: rack.heat, capacity: rack.capacity, attenuation: rack.attenuation, heat_damage, generation, reset_epoch: rack.reset_epoch, sampled_at: at }
    }).await.unwrap();
    let cx = Ctx { db: db.clone(), world: world.clone(), notify: client.notify_tx.clone(), char_id: f.character, ship_id: f.ship, system_id: 1 };
    let market = market::lock().await;
    let mut apply = Box::pin(heat::damage::apply(&cx, Some(membership), vec![old]));
    let mut context = Context::from_waker(Waker::noop());
    heat::state().modules.insert(module_id, heat::Entry { owner: f.character, ship: f.ship, effect: EFFECT_MICROWARPDRIVE,
        effective: true, desired: true, cx: cx.clone(), source_generation: Some(stop.clone()) });
    assert!(matches!(apply.as_mut().poll(&mut context), Poll::Pending));
    let ack = crate::services::repair::tests::queue_space_heal(&world, expected, membership, gate, &[module_id]);
    drop(market);
    assert!(matches!(apply.as_mut().poll(&mut context), Poll::Pending));
    assert!(ack.await.unwrap());
    assert!(apply.await.is_ok());
    let rows = crate::dao::items::dogma::overrides_for(&db, &[module_id]).await.unwrap();
    assert_eq!(rows.iter().find(|v| v.attribute_id == ATTR_HULL_DAMAGE).unwrap().value, 0.0);
    let fresh_stop = stop.clone();
    {
        let entries = heat::state();
        let entry = &entries.modules[&module_id];
        assert!(entry.desired && entry.effective);
        assert!(Arc::ptr_eq(entry.source_generation.as_ref().unwrap(), &stop));
    }
    let fresh = world.with(1, move |s| {
        let at = s.now_ms();
        let rack = &mut s.heat.ships.get_mut(&f.ship).unwrap().racks[1];
        rack.heat = rack.capacity;
        rack.anchor_heat = rack.heat;
        rack.last = at;
        rack.anchor_at = at;
        SourceSnapshot { module: module_id, master: module_id, flag: FLAG_MED_SLOT_FIRST, heat: rack.heat, capacity: rack.capacity, attenuation: rack.attenuation, heat_damage: rack.sources[&module_id].damage, generation: fresh_stop, reset_epoch: rack.reset_epoch, sampled_at: at }
    }).await.unwrap();
    assert!(heat::damage::apply(&cx, Some(membership), vec![fresh]).await.is_ok());
    let rows = crate::dao::items::dogma::overrides_for(&db, &[module_id]).await.unwrap();
    assert!(rows.iter().find(|v| v.attribute_id == ATTR_HULL_DAMAGE).unwrap().value > 0.0);
    heat::state().modules.remove(&module_id);
    combat::state().active.remove(&module_id);
    world.remove_ball(1, f.ship).await;
    world.unregister_pilot_generation(f.character, None);
    combat::forget_ship(f.ship);
    world.flush_positions().await.unwrap();
    cleanup_ship(&db, None, f).await.unwrap();
}
