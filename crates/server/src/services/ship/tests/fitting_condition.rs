//! Real station transitions preserve a ship's fitted condition without fitting changes.
use super::*;
use infinity_core::{
    consts::{CATEGORY_SHIP, dogma::{ATTR_ARMOR_DAMAGE, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_HULL_DAMAGE, ATTR_SHIELD_CAPACITY, ATTR_SHIELD_CHARGE, ATTR_SHIELD_RECHARGE_RATE}},
};
use crate::{
    services::{combat, fit::{Fit, Key}},
};
use std::time::Duration;

const CONDITION_IDS: [i32; 3] = [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE];
const MAX_IDS: [i32; 3] = [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP];
const CLEARANCE: f64 = 100.0;

macro_rules! phase {
    ($name:literal, $future:expr) => {
        tokio::time::timeout(Duration::from_secs(30), $future).await.expect(concat!("station phase timed out: ", $name))
    };
}

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() <= 1e-7 * expected.abs().max(1.0), "{actual} != {expected}");
}

async fn inspect(client: &mut Client, ship: i64, system: i64, live: bool, maxima: [f64; 3], tau: f64, minimum_shield: f64, damages: [f64; 2], stage: &str) -> f64 {
    let fit = phase!("load fit", Fit::load(&client.db, client.sde.clone(), client.session.int("charid").unwrap(), ship)).unwrap();
    for (id, max) in MAX_IDS.into_iter().zip(maxima) { close(fit.value(Key::Ship, id), max); }
    close(fit.value(Key::Ship, ATTR_SHIELD_RECHARGE_RATE), tau);
    let rows = phase!("read saved condition", crate::dao::items::dogma::overrides(&client.db, ship)).unwrap();
    let saved = CONDITION_IDS.map(|id| rows.iter().find(|row| row.attribute_id == id).expect("saved condition").value);
    let saved_at = rows.iter().find(|row| row.attribute_id == ATTR_SHIELD_CHARGE).expect("saved shield").updated_at;
    assert!(saved[0] <= maxima[0] + 1e-7 * maxima[0].max(1.0),
        "{stage}: raw saved={saved:?}, updated_at={saved_at}, minimum={minimum_shield}, maxima={maxima:?}, live={live}");
    close(saved[1], damages[0]); close(saved[2], damages[1]);
    let current = if live {
        phase!("read live condition", client.world.with(system, move |s| {
            let condition = s.ship_condition(ship)?;
            let (max, _) = s.combat_health_scalars(ship)?;
            Some((condition, max))
        })).flatten().expect("live ship condition")
    } else {
        (phase!("prepare stored condition", combat::prepare_condition_with_fit(&client.db, ship, &fit)).unwrap().scalars(), maxima)
    };
    for (actual, expected) in current.1.into_iter().zip(maxima) { close(actual, expected); }
    assert!(current.0.0 + 1e-7 * maxima[0].max(1.0) >= saved[0].max(minimum_shield) && current.0.0 <= maxima[0] + 1e-7 * maxima[0].max(1.0),
        "{stage}: raw saved={saved:?}, updated_at={saved_at}, minimum={minimum_shield}, effective={:?}, maxima={maxima:?}, live={live}", current.0);
    close(current.0.1, damages[0]); close(current.0.2, damages[1]);
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "dogmaIM", method: "ShipGetInfo", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    let Value::Dict(info) = phase!("ship information", crate::services::dogma::dispatch(client, &call)).unwrap() else { panic!("ship information") };
    let (_, Value::Instance(_, fields)) = info.iter().find(|(key, _)| *key == Value::Int(ship)).expect("ship information row") else { panic!("ship information instance") };
    let Some(Value::Dict(attrs)) = fields.get("attributes") else { panic!("ship attributes") };
    let value = |id: i32| attrs.iter().find(|(key, _)| *key == Value::Int(i64::from(id))).expect("dogma attribute").1.as_float().expect("numeric dogma attribute");
    for (id, max) in MAX_IDS.into_iter().zip(maxima) { close(value(id), max); }
    close(value(ATTR_SHIELD_RECHARGE_RATE), tau);
    let shield = value(ATTR_SHIELD_CHARGE);
    assert!(shield + 1e-7 * maxima[0].max(1.0) >= current.0.0 && shield <= maxima[0] + 1e-7 * maxima[0].max(1.0),
        "{stage}: raw saved={saved:?}, updated_at={saved_at}, minimum={minimum_shield}, effective={:?}, ShipGetInfo shield={shield}, maxima={maxima:?}, live={live}", current.0);
    close(value(ATTR_ARMOR_DAMAGE), damages[0]); close(value(ATTR_HULL_DAMAGE), damages[1]);
    shield
}

async fn dock_at_station(client: &mut Client, identity: &Identity, system: i64, station: i64, ship: i64) {
    let (sent, received) = tokio::sync::oneshot::channel();
    phase!("queue station approach", client.world.with(system, move |s| {
        let station = s.ball(station).expect("station ball");
        let position = station.position_at(s.now_ms());
        let radius = f64::from(station.radius);
        s.command(ship, move |ball, _, _| {
            ball.pos = [position[0] + radius + f64::from(ball.radius) + CLEARANCE, position[1], position[2]];
            ball.vel = [0.0; 3];
            ball.stop();
            let _ = sent.send(());
            Vec::new()
        });
    })).expect("system actor");
    phase!("station approach", received).expect("approach action");
    let change = phase!("dock", dock(identity, &client.db, &client.world, Some(system), station, None::<&mut Client>)).unwrap();
    client.session.apply(change.updates);
}

async fn move_extender(client: &Client, ship: i64, module: i64, flag: i32) {
    use crate::services::{self, inventory::{access::Container, movement}};
    let result = phase!("move extender", movement::move_item(
        &Identity::of_client(client), &client.db, &client.world, &client.sde,
        client.session.int("solarsystemid"), client.notify_tx.clone(), &Container::Item(ship),
        module, Some(ship), Some(flag), None,
        |n| services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))));
    assert_eq!(result.unwrap(), Some(module));
}

async fn online_extender(client: &Client, ship: i64, module: i64, online: bool) {
    let result = phase!("change extender online state", crate::services::effects::online::set_online(
        &Identity::of_client(client), &client.db, &client.world, &client.sde, client.notify_tx.clone(),
        client.login.user_id, ship, module, online,
        |n| crate::services::effects::online::dispatch_notification(n, &client.world, &client.notify_tx, client.session.int("charid"))));
    result.unwrap();
}

async fn fitting_pair(client: &Client, events: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>, ship: i64, old: f64, new: f64) -> f64 {
    let owner = client.session.int("charid").unwrap();
    let mut pair = None;
    while let Ok((name, args)) = events.try_recv() {
        if name != "OnModuleAttributeChanges" { continue; }
        let [Value::List(rows)] = args.as_slice() else { continue };
        for window in rows.windows(2) {
            let (Value::Tuple(max), Value::Tuple(current)) = (&window[0], &window[1]) else { continue };
            if max.get(1) == Some(&Value::Int(owner)) && max.get(2) == Some(&Value::Int(ship))
                && max.get(3) == Some(&Value::Int(i64::from(ATTR_SHIELD_CAPACITY))) {
                assert_eq!(current.get(1), Some(&Value::Int(owner)));
                assert_eq!(current.get(2), Some(&Value::Int(ship)));
                assert_eq!(current.get(3), Some(&Value::Int(i64::from(ATTR_SHIELD_CHARGE))));
                close(max[5].as_float().unwrap(), new);
                close(max[6].as_float().unwrap(), old);
                assert!(pair.replace(current[5].as_float().unwrap()).is_none());
            }
        }
    }
    let notified = pair.expect("ordered shield capacity and current pair");
    let saved = phase!("read fitting checkpoint", crate::dao::items::dogma::overrides(&client.db, ship)).unwrap();
    close(saved.iter().find(|row| row.attribute_id == ATTR_SHIELD_CHARGE).unwrap().value, notified);
    notified
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn station_roundtrip_preserves_full_and_damaged_condition() {
    station_condition_roundtrip(false).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn station_roundtrip_with_shield_extender_preserves_condition() {
    station_condition_roundtrip(true).await;
}

async fn station_condition_roundtrip(extender: bool) {
    use infinity_core::consts::dogma::{ATTR_CAPACITY, ATTR_CPU, ATTR_CPU_OUTPUT, ATTR_MED_SLOTS, ATTR_POWER, ATTR_POWER_OUTPUT};
    use infinity_core::consts::FLAG_CARGO;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), 0).await;
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some() && client.sde.map.celestial(s.station_id).is_some_and(|c| c.x.is_some() && c.y.is_some() && c.z.is_some())).expect("station celestial");
    let (station, system) = (station.station_id, station.solar_system_id.unwrap());
    let selected = extender.then(|| crate::services::effects::fitting::tests::shield_extender(&client.sde));
    let module_type = selected.map(|v| v.0);
    let module_data = module_type.map(|id| client.sde.types.get(id).unwrap());
    let ship_type = client.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && MAX_IDS.into_iter().all(|id| t.fit_attribute(id).is_some_and(|v| v > 0.0))
        && t.fit_attribute(ATTR_SHIELD_RECHARGE_RATE).is_some_and(|v| v > 0.0)
        && module_data.is_none_or(|m| t.attribute(ATTR_MED_SLOTS).unwrap_or(0.0) >= 1.0
            && t.fit_attribute(ATTR_CAPACITY).unwrap_or(0.0) >= m.volume.unwrap_or(0.0)
            && t.fit_attribute(ATTR_CPU_OUTPUT).unwrap_or(0.0) >= m.attribute(ATTR_CPU).unwrap_or(0.0)
            && t.fit_attribute(ATTR_POWER_OUTPUT).unwrap_or(0.0) >= m.attribute(ATTR_POWER).unwrap_or(0.0)))
        .expect("published ship with positive condition capacities and extender resources").type_id;
    let (account, character, ship, module) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
        let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,$2,$3) RETURNING character_id", &[&account, &station, &system]).await?.get("character_id");
        let ship = crate::dao::items::creation::rookie_ship(tx, ship_type, character, station, FLAG_HANGAR).await?;
        tx.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&character, &ship]).await?;
        let module = if let Some(ty) = module_type {
            Some(tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,false) RETURNING item_id", &[&ty, &character, &ship, &FLAG_CARGO]).await?.get::<_, i64>("item_id"))
        } else { None };
        Ok::<_, crate::db::Error>((account, character, ship, module))
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    client.session.apply(vec![("charid", Value::Int(character)), ("corpid", Value::Int(1)), ("shipid", Value::Int(ship)), ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system))]);
    client.login.user_id = account;
    client.login.role = infinity_core::consts::GM_ROLES;
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = sender;
    let world = client.world.clone();
    let result = tokio::spawn(async move {
        if let Some((module_type, _, _)) = selected {
            let mut skills: Vec<_> = crate::services::skills::requirements::requirements(&client.sde, module_type).map(|(skill, _)| (skill, 5)).collect();
            for attr in infinity_core::consts::dogma::ATTR_REQUIRED_SKILLS {
                if let Some(skill) = client.sde.types.get(module_type).and_then(|ty| ty.attribute(attr)).filter(|v| *v > 0.0) {
                    let skill = skill as i32;
                    if !skills.iter().any(|&(id, _)| id == skill) { skills.push((skill, 5)); }
                }
            }
            phase!("seed extender skills", crate::services::skills::gm::apply(&mut client, character, crate::services::skills::gm::Change::Set(skills))).unwrap();
        }
        let fit = phase!("initial fit", Fit::load(&client.db, client.sde.clone(), character, ship)).unwrap();
        let maxima = MAX_IDS.map(|id| fit.value(Key::Ship, id));
        let tau = fit.value(Key::Ship, ATTR_SHIELD_RECHARGE_RATE);
        assert!(maxima.iter().all(|v| *v > 0.0) && tau > 0.0);
        let docked = Identity { char_id: character, ship_id: Some(ship), corp_id: 1, station_id: Some(station), system_id: Some(system) };
        let in_space = Identity { station_id: None, ..docked };
        for damaged in [false, true] {
            let shield = if damaged { maxima[0] * 0.5 } else { maxima[0] };
            let damages = if damaged { [maxima[1] * 0.25, maxima[2] * 0.25] } else { [0.0, 0.0] };
            let saved = [shield, damages[0], damages[1]];
            let written = phase!("save condition", crate::dao::items::dogma::save_conditions(&client.db, station, &CONDITION_IDS, &[(ship, saved, crate::client::filetime_now())])).unwrap();
            assert_eq!(written, vec![ship]);
            let mut last = inspect(&mut client, ship, system, false, maxima, tau, shield, damages, &format!("initial damaged={damaged}")).await;
            for cycle in 0..2 {
                let undocked = phase!("undock", ship::undock(&docked, &client.db, &client.world, &client.sde, None, None)).unwrap();
                client.session.apply(undocked.change.updates);
                drop(undocked.economy);
                last = inspect(&mut client, ship, system, true, maxima, tau, last, damages, &format!("undocked damaged={damaged} cycle={cycle}")).await;
                dock_at_station(&mut client, &in_space, system, station, ship).await;
                last = inspect(&mut client, ship, system, false, maxima, tau, last, damages, &format!("docked damaged={damaged} cycle={cycle}")).await;
            }
            if !damaged { close(last, maxima[0]); }
        }
        if let (Some(module), Some((_, slot, requires_online))) = (module, selected) {
            for damaged in [false, true] {
                while events.try_recv().is_ok() {}
                move_extender(&client, ship, module, slot).await;
                if requires_online { online_extender(&client, ship, module, true).await; }
                let fit = phase!("load extended fit", Fit::load(&client.db, client.sde.clone(), character, ship)).unwrap();
                let extended = MAX_IDS.map(|id| fit.value(Key::Ship, id));
                let extended_tau = fit.value(Key::Ship, ATTR_SHIELD_RECHARGE_RATE);
                assert!(extended[0] > maxima[0]);
                let installed = fitting_pair(&client, &mut events, ship, maxima[0], extended[0]).await;
                assert!(installed < extended[0], "install must not fill the larger shield");
                let damages = if damaged { [maxima[1] * 0.25, maxima[2] * 0.25] } else { [0.0, 0.0] };
                let initial = if damaged { (maxima[0] + extended[0]) / 2.0 } else { extended[0] };
                assert_eq!(phase!("save extended condition", crate::dao::items::dogma::save_conditions(
                    &client.db, station, &CONDITION_IDS, &[(ship, [initial, damages[0], damages[1]], crate::client::filetime_now())])).unwrap(), vec![ship]);
                let mut last = inspect(&mut client, ship, system, false, extended, extended_tau, initial, damages, &format!("extended initial damaged={damaged}")).await;
                for cycle in 0..2 {
                    let undocked = phase!("undock extended ship", ship::undock(&docked, &client.db, &client.world, &client.sde, None, None)).unwrap();
                    client.session.apply(undocked.change.updates);
                    drop(undocked.economy);
                    last = inspect(&mut client, ship, system, true, extended, extended_tau, last, damages, &format!("extended undocked damaged={damaged} cycle={cycle}")).await;
                    dock_at_station(&mut client, &in_space, system, station, ship).await;
                    last = inspect(&mut client, ship, system, false, extended, extended_tau, last, damages, &format!("extended docked damaged={damaged} cycle={cycle}")).await;
                }
                if requires_online {
                    while events.try_recv().is_ok() {}
                    online_extender(&client, ship, module, false).await;
                    close(fitting_pair(&client, &mut events, ship, extended[0], maxima[0]).await, maxima[0]);
                    online_extender(&client, ship, module, true).await;
                    assert!(fitting_pair(&client, &mut events, ship, maxima[0], extended[0]).await < extended[0]);
                    assert_eq!(phase!("restore extended condition", crate::dao::items::dogma::save_conditions(
                        &client.db, station, &CONDITION_IDS, &[(ship, [initial, damages[0], damages[1]], crate::client::filetime_now())])).unwrap(), vec![ship]);
                }
                while events.try_recv().is_ok() {}
                move_extender(&client, ship, module, FLAG_CARGO).await;
                close(fitting_pair(&client, &mut events, ship, extended[0], maxima[0]).await, maxima[0]);
                let mut last = inspect(&mut client, ship, system, false, maxima, tau, maxima[0], damages, &format!("unfit initial damaged={damaged}")).await;
                let undocked = phase!("undock unfit ship", ship::undock(&docked, &client.db, &client.world, &client.sde, None, None)).unwrap();
                client.session.apply(undocked.change.updates);
                drop(undocked.economy);
                last = inspect(&mut client, ship, system, true, maxima, tau, last, damages, &format!("unfit undocked damaged={damaged}")).await;
                dock_at_station(&mut client, &in_space, system, station, ship).await;
                inspect(&mut client, ship, system, false, maxima, tau, last, damages, &format!("unfit docked damaged={damaged}")).await;
                while events.try_recv().is_ok() {}
                move_extender(&client, ship, module, slot).await;
                if requires_online { online_extender(&client, ship, module, true).await; }
                let restored = fitting_pair(&client, &mut events, ship, maxima[0], extended[0]).await;
                assert!(restored < extended[0], "refitting must not fill the shield");
                let last = inspect(&mut client, ship, system, false, extended, extended_tau, restored, damages, &format!("refitted initial damaged={damaged}")).await;
                let undocked = phase!("undock refitted ship", ship::undock(&docked, &client.db, &client.world, &client.sde, None, None)).unwrap();
                client.session.apply(undocked.change.updates);
                drop(undocked.economy);
                let last = inspect(&mut client, ship, system, true, extended, extended_tau, last, damages, &format!("refitted undocked damaged={damaged}")).await;
                dock_at_station(&mut client, &in_space, system, station, ship).await;
                inspect(&mut client, ship, system, false, extended, extended_tau, last, damages, &format!("refitted docked damaged={damaged}")).await;
                move_extender(&client, ship, module, FLAG_CARGO).await;
            }
        }
    }).await;
    world.remove_ball(system, ship).await;
    combat::forget_ship(ship);
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let ids: Vec<i64> = tx.query("SELECT item_id FROM game.items WHERE owner_id=$1", &[&character]).await?.iter().map(|row| row.get("item_id")).collect();
        tx.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE kind='ItemsMoved' AND (payload->'ItemsMoved'->>'item_id')::bigint=ANY($1))", &[&ids]).await?;
        tx.execute("DELETE FROM game.progress_events WHERE kind='ItemsMoved' AND (payload->'ItemsMoved'->>'item_id')::bigint=ANY($1)", &[&ids]).await?;
        crate::services::tests::fixture::cleanup_progress(tx, &[account, character], &[character]).await?;
        tx.execute("DELETE FROM game.skill_queue WHERE character_id=$1", &[&character]).await?;
        tx.execute("DELETE FROM game.skills WHERE character_id=$1", &[&character]).await?;
        tx.execute("DELETE FROM game.item_attributes WHERE item_id=ANY($1)", &[&ids]).await?;
        tx.execute("DELETE FROM game.items WHERE item_id=ANY($1)", &[&ids]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await?;
        Ok::<_, crate::db::Error>(())
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    result.unwrap();
}
