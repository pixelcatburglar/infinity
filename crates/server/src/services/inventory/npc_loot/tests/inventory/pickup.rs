use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn pickup_slices_loaded_settlement_and_notify_commit() {
    let mut f = space().await;
    let owner = f.fixture.character;
    let (module_type, charge_type) = crate::services::combat::targeting_charges::tests::compatible_ammunition(&f.sde);
    let attr = |name: &str| f.sde.dogma.attributes_by_name(name).next().map(|a| a.attribute_id);
    let named = |names: &[String]| names.iter().map(|name| attr(name).expect("required dogma attribute")).collect::<Vec<_>>();
    let groups = named(&(1..=20).map(|i| format!("canFitShipGroup{i:02}")).collect::<Vec<_>>());
    let types = named(&(1..=12).map(|i| format!("canFitShipType{i}")).collect::<Vec<_>>());
    let (max_group, max_type) = (attr("maxGroupFitted").expect("required dogma attribute"), attr("maxTypeFitted").expect("required dogma attribute"));
    let unrestricted = |t: &crate::sde::types::TypeData| groups.iter().chain(types.iter()).all(|a| t.attribute(*a).unwrap_or(0.0) == 0.0)
        && t.attribute(max_group).unwrap_or(0.0) == 0.0 && t.attribute(max_type).unwrap_or(0.0) == 0.0;
    let autofit_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_MODULE)
        && matches!(crate::services::effects::fitting::rack(&f.sde.types, t.type_id), Some((effect, _, _))
            if effect == infinity_core::consts::dogma::EFFECT_LO_POWER || effect == infinity_core::consts::dogma::EFFECT_MED_POWER || effect == infinity_core::consts::dogma::EFFECT_HI_POWER)
        && !t.effects().iter().any(|e| e.effect_id == infinity_core::consts::dogma::EFFECT_TURRET_FITTED || e.effect_id == infinity_core::consts::dogma::EFFECT_LAUNCHER_FITTED)
        && unrestricted(t)).expect("published ordinary module").type_id;
    let rack_attr = crate::services::effects::fitting::rack(&f.sde.types, module_type).map(|(_, attr, _)| attr)
        .into_iter().chain(crate::services::effects::fitting::rack(&f.sde.types, autofit_type).map(|(_, attr, _)| attr));
    let mut required = std::collections::BTreeMap::new();
    for kind in [module_type, autofit_type] {
        for (&skill, level) in infinity_core::consts::dogma::ATTR_REQUIRED_SKILLS.iter().zip(infinity_core::consts::dogma::ATTR_REQUIRED_SKILL_LEVELS) {
            let Some(skill) = f.sde.types.get(kind).and_then(|t| t.attribute(skill)).filter(|v| *v > 0.0) else { continue };
            let level = f.sde.types.get(kind).and_then(|t| t.attribute(level)).unwrap_or(0.0) as i32;
            required.entry(skill as i32).and_modify(|old: &mut i32| *old = (*old).max(level)).or_insert(level);
        }
    }
    let (root, loot_ids) = spawn(&f, owner, [50.0, 0.0, 0.0], vec![(f.loot_type, 7), (f.loot_type, 3)]).await;
    let (rest, rest_ids) = spawn(&f, owner, [60.0, 0.0, 0.0], vec![(f.loot_type, 9), (f.loot_type, 4)]).await;
    let scenario = crate::tests::catch_scenario(async {
        // A second in-range pilot registers for source notifications.
        let other = fixture::create(&f.db).await.unwrap();
        let other_ship = crate::dao::items::creation::positioned(&f.db, f.ship_type, other.character, f.system, [100.0, 0.0, 0.0]).await.unwrap();
        f.world.add_ship(f.system, ball(other_ship, f.ship_type, other.character, Some(other.character), [100.0, 0.0, 0.0])).await.unwrap();
        let (notice_tx, mut notices) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(other.character, notice_tx);
        let module = crate::dao::items::transfers::insert(&f.db, module_type, owner, f.ship, infinity_core::consts::FLAG_FITTING_FIRST, 1).await.unwrap();
        f.db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&module]).await.unwrap();
        crate::dao::items::dogma::set_override(&f.db, module, infinity_core::consts::dogma::ATTR_CAPACITY, f.sde.types.get(charge_type).unwrap().volume.unwrap() * 5.0).await.unwrap();
        crate::dao::items::combat::apply_loads(&f.db, f.ship, &[(0, 27, charge_type, 5, 0)]).await.unwrap();
        crate::services::skills::gm::apply(&mut f.client, owner, crate::services::skills::gm::Change::Set(required.into_iter().collect())).await.unwrap();
        for attr in rack_attr { crate::dao::items::dogma::set_override(&f.db, f.ship, attr, f64::from(crate::services::effects::fitting::RACK_WIDTH)).await.unwrap(); }
        crate::dao::items::dogma::set_override(&f.db, f.ship, infinity_core::consts::dogma::ATTR_CPU_OUTPUT, 1e9).await.unwrap();
        crate::dao::items::dogma::set_override(&f.db, f.ship, infinity_core::consts::dogma::ATTR_POWER_OUTPUT, 1e9).await.unwrap();
        let fit = crate::services::fit::Fit::load(&f.db, f.sde.clone(), owner, f.ship).await.unwrap();
        let condition = crate::services::combat::prepare_condition_with_fit(&f.db, f.ship, &fit).await.unwrap();
        f.world.with(f.system, move |s| s.install_condition(f.ship, condition.into_condition(), fit)).await.unwrap();
        let (ship, module_id) = (f.ship, module);
        let membership = f.world.with(f.system, move |s| s.combat_membership_generation(ship)).await.unwrap().unwrap();
        f.world.with(f.system, move |s| {
            s.publish_loaded_absolute(ship, membership, vec![
                crate::space::combat::loaded::LoadedSeed { module: module_id, flag: 27, type_id: Some(charge_type), quantity: 5, pending: false }]);
            let permit = s.hold_loaded_use(ship, module_id).unwrap();
            assert_eq!(s.consume_loaded(permit), Some((charge_type, 4)));
        }).await.unwrap();
        // Whole take of the first stack through the real bound RPC.
        let none = Value::Dict(vec![]);
        let params = bound_params(&mut f.client, "GetInventoryFromId", &[Value::Int(f.ship)]).await;
        let moved = call(&mut f.client, "Add", &params, &[Value::Int(loot_ids[0]), Value::Int(root)], &none).await.unwrap().as_int().unwrap();
        assert_eq!(moved, loot_ids[0]);
        let row = f.db.query_one("SELECT owner_id, location_id, flag_id, quantity FROM game.items WHERE item_id=$1", &[&loot_ids[0]]).await.unwrap();
        assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"), row.get::<_, i64>("quantity")),
            (owner, f.ship, FLAG_CARGO, 7));
        assert_eq!(view(&f, root).await.unwrap(), vec![(loot_ids[1], 3, owner, 0)]);
        // The dirty loaded row settled with its real charge type and quantity.
        let loaded = f.db.query_one("SELECT type_id, quantity FROM game.module_charges WHERE ship_id=$1 AND flag_id=$2",
            &[&f.ship, &27]).await.unwrap();
        assert_eq!((loaded.get::<_, i32>("type_id"), loaded.get::<_, i64>("quantity")), (charge_type, 4));
        // Partial take: a fresh reserved slice id carries only the taken quantity.
        let split = call(&mut f.client, "Add", &params, &[Value::Int(loot_ids[1]), Value::Int(root)], &Value::dict([("qty", Value::Int(2))])).await.unwrap().as_int().unwrap();
        assert_ne!(split, loot_ids[1]);
        let row = f.db.query_one("SELECT location_id, flag_id, quantity FROM game.items WHERE item_id=$1", &[&split]).await.unwrap();
        assert_eq!((row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"), row.get::<_, i64>("quantity")), (f.ship, FLAG_CARGO, 2));
        assert_eq!(view(&f, root).await.unwrap(), vec![(loot_ids[1], 1, owner, 0)]);
        // MultiAdd moves the two stacks of the other root, one commit each.
        call(&mut f.client, "MultiAdd", &params, &[Value::list([Value::Int(rest_ids[0]), Value::Int(rest_ids[1])]), Value::Int(rest)], &none).await.unwrap();
        for id in rest_ids {
            let row = f.db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
            assert_eq!(row.get::<_, i64>("location_id"), f.ship);
        }
        // A full-cargo refusal changes nothing in the source.
        crate::dao::items::dogma::set_override(&f.db, f.ship, infinity_core::consts::dogma::ATTR_CAPACITY, 0.0).await.unwrap();
        assert!(call(&mut f.client, "Add", &params, &[Value::Int(loot_ids[1]), Value::Int(root)], &none).await.is_err());
        assert_eq!(view(&f, root).await.unwrap(), vec![(loot_ids[1], 1, owner, 0)]);
        // R1: a persistent item can never be deposited into a transient root.
        let kept = crate::dao::items::transfers::insert(&f.db, f.loot_type, owner, f.ship, FLAG_CARGO, 1).await.unwrap();
        let wparams = bound_params(&mut f.client, "GetInventoryFromId", &[Value::Int(root)]).await;
        assert!(call(&mut f.client, "Add", &wparams, &[Value::Int(kept), Value::Int(f.ship)], &none).await.is_err());
        let row = f.db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&kept]).await.unwrap();
        assert_eq!(row.get::<_, i64>("location_id"), f.ship);
        // Emptying the wreck flips its isEmpty slim through the ordered channel.
        crate::dao::items::dogma::set_override(&f.db, f.ship, infinity_core::consts::dogma::ATTR_CAPACITY, 1e9).await.unwrap();
        call(&mut f.client, "Add", &params, &[Value::Int(loot_ids[1]), Value::Int(root)], &none).await.unwrap();
        assert!(view(&f, root).await.unwrap().is_empty());
        // The slim update is queued for the next destiny boundary, so the ball
        // is observed through the actor until that ordered projection lands.
        tokio::time::timeout(WAIT, async {
            loop {
                let projected = f.world.ball(f.system, root).await
                    .expect("the wreck actor left before its empty projection")
                    .slim.iter().any(|(k, v)| *k == "isEmpty" && *v == Value::Bool(true));
                if projected { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("ordered isEmpty projection never appeared");
        assert!(f.world.ball(f.system, root).await.unwrap().slim.iter().any(|(k, v)| *k == "isEmpty" && *v == Value::Bool(true)));
        // flag=0 on a module is a real autofit: it takes exactly 1 into a free slot.
        let (last, last_ids) = spawn(&f, owner, [70.0, 0.0, 0.0], vec![(autofit_type, 2)]).await;
        let fitted = call(&mut f.client, "Add", &params, &[Value::Int(last_ids[0]), Value::Int(last)], &Value::dict([("flag", Value::Int(0))])).await.unwrap().as_int().unwrap();
        let row = f.db.query_one("SELECT location_id, flag_id, quantity FROM game.items WHERE item_id=$1", &[&fitted]).await.unwrap();
        assert_eq!(row.get::<_, i64>("location_id"), f.ship);
        assert!((FLAG_FITTING_FIRST..=infinity_core::consts::FLAG_FITTING_LAST).contains(&row.get::<_, i32>("flag_id")) && row.get::<_, i64>("quantity") == 1);
        assert_eq!(view(&f, last).await.unwrap(), vec![(last_ids[0], 1, owner, 0)]);
        // A notification failure after commit cannot undo the durable take:
        // only the taken item's own DirectClient change is allowed to fail.
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let last_id = last_ids[0];
        let counting = {
            let hits = hits.clone();
            move |notice: crate::db::outbox::Notification| -> Result<(), CallError> {
                let hit = matches!(&notice, crate::db::outbox::Notification::DirectClient { event, args }
                    if event == "OnItemChange" && field(&args[0], "itemID") == Some(Value::Int(last_id)));
                if hit { hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst); }
                if hit { Err(notify_error(messages::fitting::CHARGES_SETTLING)) } else { Ok(()) }
            }
        };
        let moved = crate::services::inventory::movement::move_item(
            &crate::services::identity::Identity::of_client(&f.client), &f.db, &f.world, &f.sde,
            Some(f.system), f.client.notify_tx.clone(), &Container::Item(f.ship), last_ids[0], Some(last), None, None, counting).await.unwrap();
        assert!(moved.is_some() && hits.load(std::sync::atomic::Ordering::SeqCst) == 1,
            "the committed take survived exactly one real notification failure");
        // The second pilot saw only source deltas, never private cargo rows.
        let seen: Vec<(i64, Option<i64>)> = std::iter::from_fn(|| notices.try_recv().ok())
            .filter(|(name, _)| name == "OnItemChange")
            .filter_map(|(_, args)| field(&args[0], "itemID").and_then(|v| v.as_int())
                .map(|id| (id, field(&args[0], "locationID").and_then(|v| v.as_int())))).collect();
        assert!(seen.iter().any(|&(id, loc)| id == loot_ids[0] && loc == Some(infinity_core::consts::LOCATION_JUNKYARD)), "whole removal source delta");
        assert!(seen.iter().any(|&(id, loc)| id == loot_ids[1] && loc == Some(root)), "partial remainder on the source id");
        assert!(seen.iter().all(|&(id, _)| id != split && id != kept && id != fitted), "never a private cargo row");
        f.world.remove_ball(f.system, rest).await;
        f.world.remove_ball(f.system, last).await;
        f.world.remove_ball(f.system, other_ship).await;
        fixture::cleanup(&f.db, other).await.unwrap();
    }).await;
    f.world.remove_ball(f.system, root).await;
    f.world.remove_ball(f.system, rest).await;
    fixture::cleanup(&f.db, f.fixture).await.unwrap();
    scenario.unwrap();
}
