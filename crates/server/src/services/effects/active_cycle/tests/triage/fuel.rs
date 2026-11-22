//! Effective triage fuel: two paid debits, then the shortage refusal.
//! Shared mode stop, forced exit and stale terminal behavior stay outside this topic.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn triage_effective_fuel_debits_repeat_and_refuses_shortage() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let ordinary = published_links(&sde, EFFECT_TRIAGE).into_iter().find(|type_id| {
        crate::services::skills::requirements::requirements(&sde, *type_id).any(|(_, level)| level == 1)
    }).expect("ordinary triage");
    let fuel = ShipItemSpec { type_id: fuel_type(&sde, ordinary), flag_id: FLAG_CARGO, quantity: raw_units(&sde, ordinary) * 2, singleton: false, online: false, attributes: &[] };
    let mut space = in_space(&db, &sde, ordinary, EFFECT_TRIAGE, &[], &[fuel], None).await;
    let result = crate::tests::catch_scenario(async {
        let (system, mode, ship, stack) = (space.system, space.module, space.fixture.ship, space.fixture.items[1]);
        let units = effective_units(&space, mode).await;
        assert!(units > 0, "resident fit requires positive fuel");
        db.execute("UPDATE game.items SET quantity = $2 WHERE item_id = $1", &[&stack, &(units * 2)]).await.unwrap();
        let attribute = sde.dogma.effect(EFFECT_TRIAGE).expect("triage").modifiers().iter().find(|row| row.domain == "shipID").expect("ship row").modified;
        let base = ship_value(&space, attribute).await;
        let name = effect_name(&sde, EFFECT_TRIAGE);
        let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
        space.client.world.observe(system, space.fixture.character, ship, destiny).await.unwrap();
        activate(&mut space, &name, mode).await;
        until(&space.client.world, system, 5_000, || matches!(record(mode), Record::Running)).await;
        let (start, generation) = match combat::state().active_cycles.get(&mode) {
            Some(ActiveCycle::Running { start, generation, .. }) => (*start, generation.clone()),
            _ => panic!("running"),
        };
        assert_eq!(quantity(&db, stack).await, units, "the first cycle debits one effective requirement");
        assert_eq!(fx_count(&mut events, mode, 1), 1, "one start");
        assert_ne!(ship_value(&space, attribute).await, base, "the paid mode changes the fit");
        until(&space.client.world, system, 8_000, || now_ms() >= start + DURATION_MS as i64 + 400).await;
        assert_eq!(quantity(&db, stack).await, 0, "the repeat debits the second effective requirement");
        assert!(matches!(combat::state().active_cycles.get(&mode), Some(ActiveCycle::Running { generation: current, .. }) if std::sync::Arc::ptr_eq(current, &generation)), "the repeat keeps the accepted generation");
        assert_eq!(fx_count(&mut events, mode, 1), 0, "the paid repeat publishes no second start");
        let shortage = vec![Value::str(infinity_core::messages::MSG_CUSTOM_NOTIFY), Value::Dict(vec![(Value::str("notify"), Value::unicode(infinity_core::messages::effects::INSUFFICIENT_FUEL))])];
        let mut repeat_notices = Vec::new();
        until(&space.client.world, system, 8_000, || {
            repeat_notices.extend(drain(&mut space.rx));
            repeat_notices.iter().any(|(event, args)| event == "OnEffectMessage" && args == &shortage)
                && record(mode) == Record::None && !combat::state().active.contains_key(&mode)
        }).await;
        assert_eq!(godma(&repeat_notices, mode, i64::from(EFFECT_TRIAGE), 1), 1, "exactly the first start and no unpaid third start");
        assert_eq!(ship_value(&space, attribute).await, base, "retirement withdraws the bonus");
        assert_eq!(space.client.world.with(system, move |s| s.resident_fit(ship, |fit, _| fit.module_state(mode))).await.unwrap(), Some(Some((true, false))), "the fitted module stays online and inactive");
        assert_eq!(fx_count(&mut events, mode, 0), 1, "one stop");
        assert_eq!(quantity(&db, stack).await, 0, "the unpaid cycle debits nothing");
        let _ = drain(&mut space.rx);
        assert_eq!(effects_fixture::dogma_call(&mut space.client, "Activate", &[Value::Int(mode), Value::str(&name), Value::None, Value::Int(100)]).await.ok(), Some(Value::None), "the unfunded activation is queued");
        let mut notices = Vec::new();
        until(&space.client.world, system, 8_000, || {
            notices.extend(drain(&mut space.rx));
            notices.iter().any(|(event, args)| event == "OnEffectMessage" && args == &shortage)
                && record(mode) == Record::None && !combat::state().active.contains_key(&mode)
        }).await;
        assert!(notices.iter().all(|(event, args)| !(event == "OnGodmaShipEffect" && args.first() == Some(&Value::Int(mode)) && args.get(3) == Some(&Value::Int(1)))), "the refusal publishes no start");
        assert_eq!(fx_count(&mut events, mode, 1), 0, "the unfunded activation publishes no start");
        assert_eq!(quantity(&db, stack).await, 0, "the refusal debits nothing");
        assert_eq!(record(mode), Record::None, "the refusal leaves no running mode");
    }).await;
    teardown(&db, space).await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
