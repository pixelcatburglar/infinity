//! Boundary paths of one duration-bounded activation: manual stop, fitting
//! removal, committed offline and leaving space.
use super::*;
use crate::dao::items::loading::ItemQuery;
use crate::services::items::load_items;
use crate::services::CallError;
use infinity_core::consts::FLAG_CARGO;

/// A manual Deactivate during Running only sets the stop flag: the bonus and
/// the record stay until the frozen duration ends; the fitting removal refuses
/// while Running and while Cooling, then succeeds once the delay lapses.
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn manual_stop_and_removal_follow_the_timeline() {
    scenario(EFFECT_ASSAULT_DAMAGE_CONTROL, async |space, sde, db| {
        let (system, module, ship) = (space.system, space.module, space.fixture.ship);
        let name = effect_name(sde, EFFECT_ASSAULT_DAMAGE_CONTROL);
        let activate = activate_args(space, &name);
        let (attr_item, attr) = watched(sde, EFFECT_ASSAULT_DAMAGE_CONTROL, module, ship)[0];
        let base = fitted_attr(&mut space.client, attr_item, attr).await;
        let _ = drain(&mut space.rx);
        effects_fixture::dogma_call(&mut space.client, "Activate", &activate).await.ok().expect("activate");
        until(&space.client.world, system, 5_000, || matches!(record(module), Record::Running)).await;
        let Err(CallError::User { msg, .. }) = unfit(&mut space.client, module, ship).await else { panic!("running removal must refuse") };
        assert_eq!(msg, messages::MSG_CANNOT_REMOVE_ACTIVATED_MODULE);
        effects_fixture::dogma_call(&mut space.client, "Deactivate", &[Value::Int(module), Value::str(&name)]).await.ok().expect("deactivate");
        assert!(space.client.world.after(system, 200).await);
        assert!(matches!(record(module), Record::Running), "manual stop must not end the run early");
        assert!(fitted_attr(&mut space.client, ship, attr).await < base, "bonus holds until the nominal end");
        until(&space.client.world, system, 5_000, || matches!(record(module), Record::Cooling { .. })).await;
        let ev = drain(&mut space.rx);
        assert_eq!(godma(&ev, module, i64::from(EFFECT_ASSAULT_DAMAGE_CONTROL), 0), 1, "one stop notification");
        let Err(CallError::User { msg, dict }) = unfit(&mut space.client, module, ship).await else { panic!("cooling removal must refuse") };
        assert_eq!(msg, messages::MSG_CANNOT_REMOVE_MODULE_REACTIVATION_DELAYED);
        assert!(dict.iter().find(|(k, _)| k == "timePeriod").and_then(|(_, v)| v.as_float()).is_some_and(|d| d > 0.0));
        let Record::Cooling { deadline } = record(module) else { panic!("still cooling") };
        until(&space.client.world, system, 5_000, || now_ms() >= deadline).await;
        unfit(&mut space.client, module, ship).await.ok().expect("remove after the delay");
        let row = load_items(db, &space.client.sde, ItemQuery::Id(module)).await.unwrap().pop().expect("module row");
        assert_eq!(row.flag_id, FLAG_CARGO);
    }).await;
}

/// A committed offline and leaving space both end the run at once: exactly
/// one stop, the Cooling deadline survives the stale nominal-end callback
/// and keeps gating reactivation.
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn forced_terminals_end_once_and_keep_the_cooldown() {
    for offline in [true, false] {
        scenario(EFFECT_ASSAULT_DAMAGE_CONTROL, async |space, sde, _db| {
            let (system, module, ship) = (space.system, space.module, space.fixture.ship);
            let name = effect_name(sde, EFFECT_ASSAULT_DAMAGE_CONTROL);
            let activate = activate_args(space, &name);
            let (attr_item, attr) = watched(sde, EFFECT_ASSAULT_DAMAGE_CONTROL, module, ship)[0];
            let base = fitted_attr(&mut space.client, attr_item, attr).await;
            let _ = drain(&mut space.rx);
            effects_fixture::dogma_call(&mut space.client, "Activate", &activate).await.ok().expect("activate");
            until(&space.client.world, system, 5_000, || matches!(record(module), Record::Running)).await;
            let nominal_end = match combat::state().active_cycles.get(&module) {
                Some(ActiveCycle::Running { start, duration_ms, .. }) => *start + *duration_ms as i64,
                _ => panic!("running record"),
            };
            if offline {
                module_online(&space.client, ship, module, false).await.ok().expect("committed offline");
            } else {
                crate::services::combat::reset_pilot(&space.client.world, space.fixture.character, Some((system, ship)));
            }
            until(&space.client.world, system, 5_000, || matches!(record(module), Record::Cooling { .. })).await;
            let Record::Cooling { deadline } = record(module) else { panic!("cooling") };
            let ev = drain(&mut space.rx);
            assert_eq!(godma(&ev, module, i64::from(EFFECT_ASSAULT_DAMAGE_CONTROL), 0), 1, "exactly one stop");
            // A departed pilot's ShipGetInfo no longer reflects the fit; the
            // restored bonus is only asserted for the committed offline.
            if offline {
                assert_eq!(fitted_attr(&mut space.client, attr_item, attr).await, base, "bonus gone at once");
            }
            until(&space.client.world, system, 5_000, || now_ms() >= nominal_end + 300).await;
            assert!(space.client.world.after(system, 300).await);
            let ev = drain(&mut space.rx);
            assert_eq!(godma(&ev, module, i64::from(EFFECT_ASSAULT_DAMAGE_CONTROL), 0), 0, "stale callback publishes no second stop");
            assert_eq!(record(module), Record::Cooling { deadline }, "stale callback keeps the deadline");
        }).await;
    }
}
