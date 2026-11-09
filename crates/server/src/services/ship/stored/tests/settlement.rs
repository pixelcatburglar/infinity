//! Mixed-custody stored ship transitions using the real bound ship service.
//! The shared fixture supplies real SDE, database and World state; client-only
//! rendering and live-client behavior are outside these database regressions.
use super::*;
use infinity_core::consts::{FLAG_CARGO, FLAG_HANGAR};
use crate::dao::items::{dogma, transfers};

async fn mixed_take(method: &str, root_is_actor_owned: bool) {
    let mut b = BayFixture::new().await;
    let (third, _) = b.f.add_pilot().await;
    let root = b.stored(if root_is_actor_owned { b.f.owner } else { b.host_owner }).await;
    let container_type = b.f.sde.types.iter().find(|t| t.published == Some(true)
        && t.group_id.is_some_and(|g| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&g))
        && t.capacity.is_some_and(|v| v > 0.0) && b.f.sde.is_tradable(t.type_id)).expect("SDE cargo container").type_id;
    let charge_type = b.f.sde.types.iter().find(|t| t.published == Some(true)
        && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)
        && t.volume.is_some_and(|v| v > 0.0) && b.f.sde.is_tradable(t.type_id)).expect("SDE charge").type_id;
    let container = creation::rookie_ship(&b.f.db, container_type, b.host_owner, root, FLAG_CARGO).await.unwrap();
    let leaf = transfers::insert(&b.f.db, charge_type, third, container, FLAG_HANGAR, 2).await.unwrap();
    b.f.set(container, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, 1.0).await;
    b.f.set(root, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, 1.0).await;
    let root_condition = dogma::overrides(&b.f.db, root).await.unwrap();
    let condition_before = dogma::overrides(&b.f.db, container).await.unwrap();
    let before_container = row(&b, container).await;
    let before_leaf = row(&b, leaf).await;
    let old = b.f.ship;
    let active_condition = active_armor_hull(&b, old).await;
    let host = b.host;
    let result = b.call(method, &[Value::Int(host), Value::Int(root)]).await.unwrap();
    if method == "BoardStoredShip" { state(result); } else { assert_eq!(result, Value::None); }
    let root_after = row(&b, root).await;
    assert_eq!((root_after.owner_id, root_after.location_id, root_after.flag_id, root_after.type_id, root_after.quantity, root_after.singleton),
        (b.f.owner, b.f.system, 0, b.kind, 1, true));
    let after_container = row(&b, container).await;
    assert_eq!((after_container.owner_id, after_container.location_id, after_container.flag_id, after_container.type_id, after_container.quantity, after_container.singleton, after_container.custom_info),
        (b.f.owner, before_container.location_id, before_container.flag_id, before_container.type_id, before_container.quantity, before_container.singleton, before_container.custom_info));
    let after_leaf = row(&b, leaf).await;
    assert_eq!((after_leaf.owner_id, after_leaf.location_id, after_leaf.flag_id, after_leaf.type_id, after_leaf.quantity, after_leaf.singleton, after_leaf.custom_info),
        (b.f.owner, before_leaf.location_id, before_leaf.flag_id, before_leaf.type_id, before_leaf.quantity, before_leaf.singleton, before_leaf.custom_info));
    let condition_after = dogma::overrides(&b.f.db, container).await.unwrap();
    assert_eq!(condition_before.iter().map(|a| (a.item_id, a.attribute_id, a.value, a.updated_at)).collect::<Vec<_>>(),
        condition_after.iter().map(|a| (a.item_id, a.attribute_id, a.value, a.updated_at)).collect::<Vec<_>>());
    let root_after_condition = dogma::overrides(&b.f.db, root).await.unwrap();
    assert_eq!(root_condition.iter().map(|a| (a.item_id, a.attribute_id, a.value, a.updated_at)).collect::<Vec<_>>(),
        root_after_condition.iter().map(|a| (a.item_id, a.attribute_id, a.value, a.updated_at)).collect::<Vec<_>>());
    if method == "BoardStoredShip" {
        let previous = row(&b, old).await;
        assert_eq!((previous.owner_id, previous.location_id), (b.f.owner, b.f.system));
        assert_eq!(b.f.world.ball(b.f.system, old).await.unwrap().char_id, None);
        selected(&b, root).await;
    } else {
        let ball = b.f.world.ball(b.f.system, root).await.unwrap();
        assert_eq!((ball.owner_id, ball.char_id, ball.interactive), (b.f.owner, None, false));
        assert_eq!(active_armor_hull(&b, old).await, active_condition);
        selected(&b, old).await;
    }
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn board_takes_guest_tree_even_when_root_is_already_owned() {
    mixed_take("BoardStoredShip", false).await;
    mixed_take("BoardStoredShip", true).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn launch_takes_guest_tree_even_when_root_is_already_owned() {
    mixed_take("LaunchFromContainer", false).await;
    mixed_take("LaunchFromContainer", true).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn store_preserves_guest_cargo_and_later_board_takes_it() {
    let mut b = BayFixture::new().await;
    let old = b.f.ship;
    let host = b.host;
    let (module_type, charge_type) = crate::services::combat::targeting_charges::tests::compatible_ammunition(&b.f.sde);
    let kind = b.f.sde.types.iter().find(|t| t.published == Some(true)
        && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.type_id != charge_type
        && t.volume.is_some_and(|v| v > 0.0) && b.f.sde.is_tradable(t.type_id)).expect("SDE charge").type_id;
    let module = creation::rookie_ship(&b.f.db, module_type, b.f.owner, old, infinity_core::consts::FLAG_FITTING_FIRST).await.unwrap();
    b.f.set(old, infinity_core::consts::dogma::ATTR_CAPACITY, 1_000_000.0).await;
    b.f.set(old, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, 1.0).await;
    let fit = crate::services::fit::Fit::load_in_space(&b.f.db, &b.f.world, b.f.system, b.f.owner, old).await.unwrap();
    let condition = crate::services::combat::prepare_condition_with_fit(&b.f.db, old, &fit).await.unwrap().into_condition();
    let expected_damage = b.f.world.with(b.f.system, move |s| {
        s.install_condition(old, condition, fit);
        assert!(s.set_condition_fraction_mask(old, 0b010, 0.5));
        let snapshot = s.combat_condition_snapshot(old).unwrap();
        snapshot.condition.stats.max[1] - snapshot.condition.now[1]
    }).await.unwrap();
    assert!(expected_damage > 1.0);
    crate::dao::items::pending_reloads::insert(&b.f.db, b.f.owner, old, module, infinity_core::consts::FLAG_FITTING_FIRST, charge_type, 1, FLAG_CARGO, old).await.unwrap();
    let pending = crate::dao::items::pending_reloads::for_ship(&b.f.db, old).await.unwrap();
    let leaf = transfers::insert(&b.f.db, kind, b.host_owner, old, FLAG_CARGO, 2).await.unwrap();
    let private_row = crate::services::items::load_items(&b.f.db, &b.f.sde, ItemQuery::Id(leaf)).await.unwrap().pop().unwrap().row();
    let (tx, mut notices) = tokio::sync::mpsc::unbounded_channel();
    b.f.world.register_pilot(b.host_owner, tx);
    state(b.call("StoreVessel", &[Value::Int(host)]).await.unwrap());
    let capsule = b.f.client.session.int("shipid").unwrap();
    let stored = row(&b, old).await;
    assert_eq!((stored.owner_id, stored.location_id, stored.flag_id), (b.f.owner, host, FLAG_SHIP_HANGAR));
    let saved_attributes = dogma::overrides(&b.f.db, old).await.unwrap();
    assert!(saved_attributes.iter().any(|a| a.attribute_id == infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE && (a.value - expected_damage).abs() < 1e-9));
    let kept = row(&b, leaf).await;
    assert_eq!((kept.owner_id, kept.location_id, kept.flag_id, kept.quantity, kept.type_id),
        (b.host_owner, old, FLAG_CARGO, 2, kind));
    assert!(crate::dao::items::pending_reloads::for_ship(&b.f.db, old).await.unwrap() == pending);
    assert!(!load_items(&b.f.db, ItemQuery::Location(old)).await.unwrap().iter().any(|item| item.flag_id == FLAG_CARGO && item.type_id == charge_type && item.owner_id == b.f.owner && !item.singleton));
    while let Ok((event, args)) = notices.try_recv() {
        assert!(event != "OnItemChange" || args.first() != Some(&private_row), "host saw private cargo");
    }
    assert!(b.f.world.ball(b.f.system, old).await.is_none());
    selected(&b, capsule).await;
    state(b.call("BoardStoredShip", &[Value::Int(host), Value::Int(old)]).await.unwrap());
    assert!(crate::dao::items::pending_reloads::for_ship(&b.f.db, old).await.unwrap().is_empty());
    let refund = load_items(&b.f.db, ItemQuery::Location(old)).await.unwrap().into_iter().find(|item| item.flag_id == FLAG_CARGO && item.type_id == charge_type && item.owner_id == b.f.owner && !item.singleton).unwrap();
    assert_eq!(refund.quantity, 1);
    let acquired = row(&b, leaf).await;
    assert_eq!((acquired.owner_id, acquired.location_id, acquired.flag_id, acquired.quantity, acquired.type_id),
        (b.f.owner, old, FLAG_CARGO, 2, kind));
    assert_eq!(b.f.world.ball(b.f.system, capsule).await.unwrap().char_id, None);
    selected(&b, old).await;
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn source_viewers_receive_only_junkyard_row_on_launch() {
    let mut b = BayFixture::new().await;
    let (viewer, _) = b.f.add_pilot().await;
    let root = b.stored(b.host_owner).await;
    let old = crate::services::items::load_items(&b.f.db, &b.f.sde, ItemQuery::Id(root)).await.unwrap().pop().unwrap();
    let gone = crate::dao::items::loading::Item { location_id: infinity_core::consts::LOCATION_JUNKYARD, ..old.clone() }.row();
    let (host_tx, mut host_rx) = tokio::sync::mpsc::unbounded_channel();
    let (viewer_tx, mut viewer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (actor_tx, mut actor_rx) = tokio::sync::mpsc::unbounded_channel();
    b.f.world.register_pilot(b.host_owner, host_tx);
    b.f.world.register_pilot(viewer, viewer_tx);
    b.f.client.notify_tx = actor_tx;
    let host = b.host;
    assert_eq!(b.call("LaunchFromContainer", &[Value::Int(host), Value::Int(root)]).await.unwrap(), Value::None);
    let new = crate::services::items::load_items(&b.f.db, &b.f.sde, ItemQuery::Id(root)).await.unwrap().pop().unwrap().row();
    for rx in [&mut host_rx, &mut viewer_rx] {
        let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).filter(|(event, _)| event == "OnItemChange").collect();
        assert_eq!(notices.iter().filter(|(_, args)| args.first() == Some(&gone)).count(), 1);
        assert!(!notices.iter().any(|(_, args)| args.first() == Some(&new)), "source-only viewer saw destination row");
    }
    let actor_notices: Vec<_> = std::iter::from_fn(|| actor_rx.try_recv().ok()).filter(|(event, _)| event == "OnItemChange").collect();
    assert!(actor_notices.iter().any(|(_, args)| args.first() == Some(&new)), "native actor change missing");
    selected(&b, b.f.ship).await;
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn revoked_corporation_viewer_receives_no_shared_bay_row() {
    let mut b = BayFixture::new().await;
    let (viewer, _) = b.f.add_pilot().await;
    let host_owner = b.host_owner;
    let host = b.host;
    let host_ship = crate::dao::characters::selection::ship_id(&b.f.db, host_owner).await.unwrap().unwrap();
    crate::services::drones::tests::resources::fleet::form_fleet(&mut b.f, host_owner, host_ship).await.unwrap();
    b.f.db.transaction(|tx, _: &mut ()| Box::pin(async move {
        assert!(crate::dao::items::ship::lock_reload_ship(tx, host).await?);
        assert!(ship_configuration::patch(tx, host, host_owner, &Patch {
            maintenance_corp: Some(false), maintenance_fleet: Some(true), ..Patch::default()
        }).await?);
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    let root = b.stored(host_owner).await;
    let (tx, mut viewer_rx) = tokio::sync::mpsc::unbounded_channel();
    b.f.world.register_pilot(viewer, tx);
    assert_eq!(b.call("LaunchFromContainer", &[Value::Int(host), Value::Int(root)]).await.unwrap(), Value::None);
    assert!(!std::iter::from_fn(|| viewer_rx.try_recv().ok()).any(|(event, _)| event == "OnItemChange"));
    selected(&b, b.f.ship).await;
    b.cleanup().await;
}

async fn pending_take(method: &str, outside_weapon: bool, refuse: bool, late_skill: bool) {
    use infinity_core::consts::{FLAG_FITTING_FIRST, dogma::ATTR_CAPACITY};
    use crate::dao::items::pending_reloads;
    let mut b = BayFixture::new().await;
    let (module_type, charge_type) = crate::services::combat::targeting_charges::tests::compatible_ammunition(&b.f.sde);
    let (third, _) = b.f.add_pilot().await;
    let root = b.stored(b.host_owner).await;
    let outside = crate::dao::characters::selection::ship_id(&b.f.db, b.host_owner).await.unwrap().unwrap();
    let weapon_ship = if outside_weapon { outside } else { root };
    let source = if outside_weapon { root } else { outside };
    let module = creation::rookie_ship(&b.f.db, module_type, b.host_owner, weapon_ship, FLAG_FITTING_FIRST).await.unwrap();
    let descendant = transfers::insert(&b.f.db, charge_type, third, root, FLAG_CARGO, 2).await.unwrap();
    b.f.set(source, ATTR_CAPACITY, if refuse { 0.0 } else { 1_000_000.0 }).await;
    let stack = transfers::insert(&b.f.db, charge_type, b.host_owner, source, FLAG_CARGO, 2).await.unwrap();
    pending_reloads::insert(&b.f.db, b.host_owner, weapon_ship, module, FLAG_FITTING_FIRST, charge_type, 1, FLAG_CARGO, source).await.unwrap();
    if late_skill { remove_required_skill(&b).await; }
    let before_pending = pending_reloads::for_ship(&b.f.db, weapon_ship).await.unwrap();
    let before_root = row(&b, root).await;
    let before_module = row(&b, module).await;
    let before_stack = row(&b, stack).await;
    let before_descendant = row(&b, descendant).await;
    let before_outside = row(&b, outside).await;
    let before_active = row(&b, b.f.ship).await;
    let before_all_attributes = dogma::overrides_for(&b.f.db, &[root, outside, module, b.f.ship]).await.unwrap();
    let before_attributes = dogma::overrides(&b.f.db, source).await.unwrap();
    let (guest_tx, mut guest_rx) = tokio::sync::mpsc::unbounded_channel();
    let (actor_tx, mut actor_rx) = tokio::sync::mpsc::unbounded_channel();
    let (third_tx, mut third_rx) = tokio::sync::mpsc::unbounded_channel();
    b.f.world.register_pilot(third, third_tx);
    b.f.world.register_pilot(b.host_owner, guest_tx);
    b.f.client.notify_tx = actor_tx;
    let old = b.f.ship;
    let host = b.host;
    let packet_count = b.f.client.outbox.len();
    let event_count = crate::db::outbox::tests::events(&b.f.db, "ItemsMoved", b.f.owner).await.len();
    let result = b.call(method, &[Value::Int(host), Value::Int(root)]).await;
    if refuse || late_skill {
        if late_skill {
            assert!(matches!(result, Err(crate::services::CallError::User { ref msg, .. })
                if msg == infinity_core::messages::MSG_FITTING_HAS_SKILL_PREREQUISITES));
        } else {
            assert!(result.is_err());
        }
        let now_root = row(&b, root).await;
        let now_module = row(&b, module).await;
        let now_stack = row(&b, stack).await;
        let now_descendant = row(&b, descendant).await;
        let now_outside = row(&b, outside).await;
        let now_active = row(&b, old).await;
        for (before, after) in [(&before_root, &now_root), (&before_module, &now_module), (&before_stack, &now_stack), (&before_descendant, &now_descendant), (&before_outside, &now_outside), (&before_active, &now_active)] {
            assert_eq!((before.item_id, before.type_id, before.owner_id, before.location_id, before.flag_id, before.quantity, before.singleton, &before.custom_info),
                (after.item_id, after.type_id, after.owner_id, after.location_id, after.flag_id, after.quantity, after.singleton, &after.custom_info));
        }
        assert!(pending_reloads::for_ship(&b.f.db, weapon_ship).await.unwrap() == before_pending);
        let after_attributes = dogma::overrides(&b.f.db, source).await.unwrap();
        assert_eq!(before_attributes.iter().map(|a| (a.item_id, a.attribute_id, a.value, a.updated_at)).collect::<Vec<_>>(),
            after_attributes.iter().map(|a| (a.item_id, a.attribute_id, a.value, a.updated_at)).collect::<Vec<_>>());
        let after_all_attributes = dogma::overrides_for(&b.f.db, &[root, outside, module, old]).await.unwrap();
        assert_eq!(before_all_attributes.iter().map(|a| (a.item_id, a.attribute_id, a.value, a.updated_at)).collect::<Vec<_>>(),
            after_all_attributes.iter().map(|a| (a.item_id, a.attribute_id, a.value, a.updated_at)).collect::<Vec<_>>());
        assert_eq!(b.f.client.outbox.len(), packet_count);
        assert!(actor_rx.try_recv().is_err());
        assert!(guest_rx.try_recv().is_err());
        assert!(third_rx.try_recv().is_err());
        assert!(b.f.world.ball(b.f.system, root).await.is_none());
        assert_eq!(crate::db::outbox::tests::events(&b.f.db, "ItemsMoved", b.f.owner).await.len(), event_count);
        selected(&b, old).await;
    } else {
        let value = result.unwrap();
        if method == "BoardStoredShip" { state(value); } else { assert_eq!(value, Value::None); }
        let events = crate::db::outbox::tests::events(&b.f.db, "ItemsMoved", b.f.owner).await;
        assert!(events[event_count..].iter().any(|event| matches!(event,
            crate::lua::events::GameEvent::ItemsMoved { item_id, from_location_id, to_location_id, quantity, .. }
                if *item_id == root && *from_location_id == host && *to_location_id == b.f.system && *quantity == 1)));
        assert!(pending_reloads::for_ship(&b.f.db, weapon_ship).await.unwrap().is_empty());
        let refunded = row(&b, stack).await;
        assert_eq!((refunded.owner_id, refunded.location_id, refunded.flag_id, refunded.quantity, refunded.type_id),
            (if outside_weapon { b.f.owner } else { b.host_owner }, source, FLAG_CARGO, 3, charge_type));
        let refund_item = crate::services::items::load_items(&b.f.db, &b.f.sde, ItemQuery::Id(stack)).await.unwrap().pop().unwrap();
        let private = refund_item.row();
        let old_owner_row = crate::dao::items::loading::Item { owner_id: b.host_owner, ..refund_item }.row();
        let guest_notices: Vec<_> = std::iter::from_fn(|| guest_rx.try_recv().ok()).collect();
        if !outside_weapon {
            let changes: Vec<_> = guest_notices.iter().filter(|(event, args)| event == "OnItemChange" && args.first() == Some(&private)).collect();
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].1[1], Value::Dict(vec![(Value::Int(infinity_core::consts::IX_STACK_SIZE), Value::Int(2))]));
            assert!(!std::iter::from_fn(|| actor_rx.try_recv().ok()).any(|(event, args)| event == "OnItemChange" && args.first() == Some(&private)));
            for packet in &b.f.client.outbox[packet_count..] {
                if matches!(&packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. }
                    if broadcast_id.as_text().as_deref() == Some("OnItemChange")) {
                    let envelope = packet.body[0].items().unwrap();
                    let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
                    let args = payload.items().unwrap()[1].items().unwrap();
                    assert_ne!(args.first(), Some(&private), "private refund reached actor packet");
                }
            }
        } else {
            assert_eq!(guest_notices.iter().filter(|(event, args)| event == "OnItemChange" && args.first() == Some(&old_owner_row)).count(), 1);
        }
        selected(&b, if method == "BoardStoredShip" { root } else { old }).await;
    }
    b.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn private_outside_source_refunds_original_owner_before_board_and_launch() {
    pending_take("BoardStoredShip", false, false, false).await;
    pending_take("LaunchFromContainer", false, false, false).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn surviving_outside_weapon_source_inside_is_settled_before_handover() {
    pending_take("LaunchFromContainer", true, false, false).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn refund_capacity_refusal_rolls_back_board_and_launch() {
    pending_take("BoardStoredShip", false, true, false).await;
    pending_take("LaunchFromContainer", true, true, false).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn late_board_skill_refusal_rolls_back_refund_and_mixed_handover() {
    pending_take("BoardStoredShip", false, false, true).await;
}
