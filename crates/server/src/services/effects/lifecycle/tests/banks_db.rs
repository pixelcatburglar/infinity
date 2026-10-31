//! Real actor regression: activating a bank by a slave and offlining a member.
use super::*;
use crate::services::effects::online::set_online;
use crate::{
    consts::{ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE, ATTR_SCAN_RESOLUTION, CATEGORY_CHARGE, CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HI_SLOT_FIRST, GROUPABLE_GROUP_IDS},
    dao::items::{charges, combat as item_combat, dogma},
    services::combat,
    space::{Ball, WarpState, World, MODE_STOP},
};

/// Client dogma.const.attributeMaxGroupActive.
const MAX_GROUP_ACTIVE: i32 = 763;

fn ball(id: i64, type_id: i32, owner: i64, pilot: Option<i64>) -> Ball {
    Ball { id, type_id, owner_id: owner, corp_id: 1, alliance_id: -1, char_id: pilot,
        signature_radius: None, mode: MODE_STOP, radius: 100.0, mass: 1e6, max_vel: 0.0,
        agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3],
        old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3], follow_id: 0,
        follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
        warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

async fn call(client: &mut Client, method: &str, args: &[Value]) -> CallResult {
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "dogmaIM", method, args, kwargs: &kwargs, bound: false, bind_params: None };
    combat::dispatch(client, &call).await.expect("combat method")
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE"]
async fn slave_activation_fires_bank_and_offline_stops_actor_cycle() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let size = sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
    let (gun, ammo, effect_name, effect_id, duration_attr) = sde.types.iter()
        .filter(|m| m.published == Some(true) && m.category_id == Some(CATEGORY_MODULE)
            && m.group_id.is_some_and(|g| GROUPABLE_GROUP_IDS.contains(&i64::from(g))))
        .find_map(|m| {
            let effect = m.effects().iter().find_map(|e| {
                let effect = registry::resolve(&sde, e.effect_id)?;
                let row = sde.dogma.effect(e.effect_id)?;
                let name = row.name.as_ref()?;
                (effect.kind == EffectKind::Turret && effect.guid.as_deref().is_some_and(|guid| !guid.is_empty())
                    && sde.dogma.effects_by_name(name).count() == 1)
                    .then_some((effect, name.clone()))
            })?;
            let groups: Vec<_> = ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"].iter()
                .flat_map(|name| sde.dogma.attributes_by_name(name)).filter_map(|a| m.attribute(a.attribute_id)).collect();
            let module_size = size.and_then(|id| m.attribute(id));
            let capacity = m.capacity.or_else(|| m.attribute(crate::consts::ATTR_CAPACITY))?;
            let c = sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE)
                && c.group_id.is_some_and(|g| groups.contains(&f64::from(g)))
                && (module_size.is_none_or(|v| v <= 0.0) || size.and_then(|id| c.attribute(id)) == module_size)
                && c.volume.is_some_and(|v| v.is_finite() && v > 0.0 && capacity.is_finite() && capacity >= 3.0 * v))?;
            Some((m.type_id, c.type_id, effect.1, effect.0.id, effect.0.attribute(EffectAttribute::Duration)?))
        }).expect("published groupable turret with FX and compatible ammunition");
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_CAPACITOR_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)
        && t.attribute(ATTR_RECHARGE_RATE).is_some_and(|v| v.is_finite() && v > 0.0)
        && t.attribute(ATTR_SCAN_RESOLUTION).is_some_and(|v| v.is_finite() && v > 0.0)).unwrap().type_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('bank-smoke-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'bank-smoke-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,1,0,1,true,0,0,0) RETURNING item_id", &[&ship_type,&character]).await.unwrap().get("item_id");
    let target: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,1,0,1,true,0,0,0) RETURNING item_id", &[&ship_type,&character]).await.unwrap().get("item_id");
    let mut guns = Vec::new();
    for i in 0..4 {
        let flag = FLAG_HI_SLOT_FIRST + i;
        let owner = if i == 3 { ship } else { character };
        let id: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&gun,&owner,&ship,&flag]).await.unwrap().get("item_id");
        guns.push(id);
        if i < 3 { db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,$3,3)", &[&ship,&flag,&ammo]).await.unwrap(); }
    }
    // A long real cycle leaves enough time to observe the first volley and stop it.
    dogma::set_override(&db, guns[0], duration_attr, 30_000.0).await.unwrap();
    dogma::set_override(&db, ship, ATTR_CAPACITOR_CAPACITY, 1_000_000.0).await.unwrap();
    item_combat::move_bank(&db, ship, guns[0], guns[1]).await.unwrap();
    item_combat::move_bank(&db, ship, guns[0], guns[2]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let w = world.clone();
    let d = db.clone();
    let ids = guns.clone();
    let result = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let mut client = Client::new(crate::conn::Conn::new(stream, address), Arc::new(crate::config::Config::default()), d.clone(), sde,
            crate::chat::Hub::new("localhost"), w.clone(), crate::handshake::Login { user_id: account, user_name: "bank-smoke".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
        client.session.apply(vec![("charid", Value::Int(character)), ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(1))]);
        let (notify, mut notifications) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = notify.clone();
        w.register_pilot(character, notify);
        w.add_ship(1, ball(ship, ship_type, character, Some(character))).await.unwrap();
        w.add_ship(1, ball(target, ship_type, character, None)).await.unwrap();
        let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
        w.observe(1, character, ship, destiny).await.unwrap();
        call(&mut client, "AddTarget", &[Value::Int(target)]).await.ok().expect("target request");
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            while !combat::targeting_charges::targets_of(character).contains(&target) { assert!(w.after(1, 200).await); }
        }).await.expect("target lock");
        // An unauthorized original slave may not inherit the owned master's authority.
        item_combat::move_bank(&d, ship, ids[0], ids[3]).await.unwrap();
        let args = |item| vec![Value::Int(item), Value::str(&effect_name), Value::Int(target), Value::Int(20)];
        assert!(call(&mut client, "Activate", &args(ids[3])).await.is_err());
        { let st = state(); assert!(!st.active.contains_key(&ids[0]) && !st.active.contains_key(&ids[3])); }
        d.execute("DELETE FROM game.weapon_banks WHERE ship_id=$1 AND slave_id=$2", &[&ship,&ids[3]]).await.unwrap();
        for &id in &ids[..3] { dogma::set_override(&d, id, MAX_GROUP_ACTIVE, 2.0).await.unwrap(); }
        match call(&mut client, "Activate", &args(ids[1])).await {
            Err(crate::services::CallError::User { msg, dict }) => {
                assert_eq!(msg, crate::messages::MSG_EFFECT_CROWDED_OUT);
                assert!(dict.iter().any(|(key, value)| key == "module" && *value == Value::Int(i64::from(gun))));
                assert!(dict.iter().any(|(key, value)| key == "count" && *value == Value::Int(2)));
            }
            _ => panic!("expected EffectCrowdedOut"),
        }
        {
            let st = state();
            for id in &ids[..3] {
                assert!(!st.active.contains_key(id));
                assert!(!st.activation_generation.contains_key(id));
            }
        }
        assert!(w.after(1, 200).await);
        while let Ok((name, payload)) = notifications.try_recv() {
            assert!(name != "OnGodmaShipEffect" || payload.get(1).and_then(Value::as_int) != Some(effect_id)
                || payload.get(3).and_then(Value::as_int) != Some(1)
                || !payload.first().and_then(Value::as_int).is_some_and(|id| ids[..3].contains(&id)));
        }
        while let Ok(batch) = events.try_recv() {
            for event in batch {
                let Value::Tuple(envelope) = event else { continue };
                let Some(Value::Tuple(call)) = envelope.get(1) else { continue };
                if call.first() != Some(&Value::str("OnSpecialFX")) { continue; }
                let Some(Value::Tuple(args)) = call.get(1) else { continue };
                assert!(args.get(7).and_then(Value::as_int) != Some(1)
                    || !args.get(1).and_then(Value::as_int).is_some_and(|id| ids[..3].contains(&id)));
            }
        }
        for &id in &ids[..3] { dogma::clear_override(&d, id, MAX_GROUP_ACTIVE).await.unwrap(); }
        call(&mut client, "Activate", &args(ids[1])).await.ok().expect("activate slave");
        let stop = { let st = state();
            assert!(!st.active.contains_key(&ids[1]) && !st.active.contains_key(&ids[2]));
            st.active.get(&ids[0]).expect("master active").stop.clone()
        };
        let mut start_notifications = 0;
        while let Ok((name, payload)) = notifications.try_recv() {
            if name == "OnGodmaShipEffect" && payload.get(1).and_then(Value::as_int) == Some(effect_id) && payload.get(3).and_then(Value::as_int) == Some(1) {
                assert_eq!(payload[0], Value::Int(ids[0])); start_notifications += 1;
            }
        }
        assert_eq!(start_notifications, 1);
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let mut ready = true;
                for i in 0..3 { ready &= loaded(&d, ship, FLAG_HI_SLOT_FIRST + i).await.ok().unwrap() == Some((ammo, 2)); }
                if ready { break; }
                assert!(w.after(1, 100).await);
            }
        }).await.expect("first bank volley");
        let mut seen = std::collections::HashSet::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while seen.len() < 3 {
                for event in events.recv().await.expect("destiny observer") {
                    let Value::Tuple(envelope) = event else { continue };
                    let Some(Value::Tuple(call)) = envelope.get(1) else { continue };
                    if call.first() != Some(&Value::str("OnSpecialFX")) { continue; }
                    let Some(Value::Tuple(args)) = call.get(1) else { continue };
                    if args.get(7).and_then(Value::as_int) == Some(1) {
                        if let Some(id) = args.get(1).and_then(Value::as_int) { if ids[..3].contains(&id) { seen.insert(id); } }
                    }
                }
            }
        }).await.expect("per-gun firing FX");
        set_online(&mut client, ship, ids[1], false).await.ok().expect("offline slave");
        assert!(charges::banks(&d, ship).await.unwrap().is_empty());
        assert!(stop.load(Ordering::Relaxed));
        tokio::time::timeout(std::time::Duration::from_secs(40), async {
            while state().active.contains_key(&ids[0]) { assert!(w.after(1, 100).await); }
        }).await.expect("master cycle retirement");
        for i in 0..3 { assert_eq!(loaded(&d, ship, FLAG_HI_SLOT_FIRST + i).await.ok().unwrap(), Some((ammo, 2))); }
        // A competing forced offline commits while activation is waiting for the shared lock.
        while notifications.try_recv().is_ok() {}
        while events.try_recv().is_ok() {}
        let policy = super::super::super::online::OnlinePolicy::from_client(&client).ok().expect("derive online policy");
        let lock = crate::services::market::lock().await;
        let activation_args = args(ids[0]);
        let (started, waiting) = tokio::sync::oneshot::channel();
        let activation = tokio::spawn(async move {
            let _ = started.send(());
            let result = call(&mut client, "Activate", &activation_args).await;
            (client, result)
        });
        waiting.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let completed_before_offline = activation.is_finished();
        let master = ids[0];
        let sde = w.sde().clone();
        let dispatch_world = w.clone();
        let offline = d.transaction(
            move |tx, outbox: &mut crate::db::outbox::Outbox<super::super::super::online::OnlineChanged>| {
                let policy = policy.clone();
                let sde = sde.clone();
                Box::pin(async move {
                    super::super::super::online::set_online_core(tx, &sde, &policy, outbox, ship, master, false).await
                })
            },
            move |changed, outbox| async move {
                for entry in outbox.into_entries() {
                    match entry {
                        crate::db::outbox::Effect::Operation(change) => super::super::super::online::apply_online_changed(change),
                        crate::db::outbox::Effect::Notification(notification) => notification.dispatch(&dispatch_world, None)?,
                        crate::db::outbox::Effect::Event(event) => crate::lua::on_event(event),
                    }
                }
                Ok(changed)
            },
        ).await;
        drop(lock);
        let (client, activation_result) = activation.await.unwrap();
        assert!(offline.ok().expect("forced offline transaction"));
        assert!(!completed_before_offline, "activation bypassed the fitting lock");
        assert!(activation_result.is_err(), "offline module activated");
        {
            let st = state();
            for id in &ids[..3] {
                assert!(!st.active.contains_key(id));
                assert!(!st.activation_generation.contains_key(id));
            }
        }
        assert!(w.after(1, 200).await);
        while let Ok((name, payload)) = notifications.try_recv() {
            assert!(name != "OnGodmaShipEffect" || payload.get(1).and_then(Value::as_int) != Some(effect_id)
                || payload.get(3).and_then(Value::as_int) != Some(1)
                || !payload.first().and_then(Value::as_int).is_some_and(|id| ids[..3].contains(&id)));
        }
        while let Ok(batch) = events.try_recv() {
            for event in batch {
                let Value::Tuple(envelope) = event else { continue };
                let Some(Value::Tuple(call)) = envelope.get(1) else { continue };
                if call.first() != Some(&Value::str("OnSpecialFX")) { continue; }
                let Some(Value::Tuple(args)) = call.get(1) else { continue };
                assert!(args.get(7).and_then(Value::as_int) != Some(1)
                    || !args.get(1).and_then(Value::as_int).is_some_and(|id| ids[..3].contains(&id)));
            }
        }
        combat::reset(&client);
        w.unobserve(character);
    }).await;
    {
        let mut st = state();
        for id in &guns {
            if let Some(active) = st.active.remove(id) { active.stop.store(true, Ordering::Relaxed); }
            st.activation_generation.remove(id);
        }
    }
    combat::forget_gm_spawn(&world, target);
    world.unobserve(character);
    world.remove_ball(1, ship).await;
    world.remove_ball(1, target).await;
    world.unregister_pilot(character);
    world.flush_positions().await.unwrap();
    combat::forget_ship(ship);
    combat::forget_ship(target);
    db.execute("DELETE FROM game.weapon_banks WHERE ship_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.module_charges WHERE ship_id=$1", &[&ship]).await.unwrap();
    let mut items = guns; items.extend([ship, target]);
    db.execute("DELETE FROM game.item_attributes WHERE item_id = ANY($1)", &[&items]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&items]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}
