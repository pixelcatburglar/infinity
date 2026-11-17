use super::super::*;
use super::fixtures::*;
use super::fixtures::activate;
use crate::services::fit::{Fit, Key};

fn wire_contains(fit: &Fit, character: i64, ty: i32, modified: i32, op: i32) -> bool {
    let (character_effects, ship_effects, _) = crate::services::dogma::brain::compile(fit, character, 0);
    [character_effects, ship_effects].into_iter().any(|list| {
        let Value::List(effects) = list else { return false };
        effects.into_iter().any(|effect| {
            let Value::Reduce { data, .. } = effect else { return false };
            let Value::Tuple(parts) = *data else { return false };
            let Some(state) = parts.get(2) else { return false };
            state.get("toAttribID") == Some(&Value::Int(i64::from(modified)))
                && state.get("operation") == Some(&Value::Int(i64::from(op)))
                && matches!(state.get("skills"), Some(Value::List(skills)) if skills.contains(&Value::Int(i64::from(ty))))
        })
    })
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn real_activation_draws_select_penalty_in_fit_and_brain_traversal() {
    let _market = crate::services::market::lock().await;
    let (db, selected, c, _peer) = setup().await;
    let other = crate::services::tests::fixture::create(&db).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&other.character, &STATION]).await.unwrap();
    let now = filetime_now();
    let mut exercised = false;
    let mut recipient = None;
    let scenario = crate::tests::catch_scenario(async {
    'types: for data in c.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id == Some(GROUP_BOOSTER)
        && t.attribute(ATTR_BOOSTERNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0)
        && t.attribute(ATTR_BOOSTER_DURATION).is_some_and(|v| v > 0.0 && v.is_finite())
        && t.attribute(ATTR_BOOSTER_MAX_CHAR_AGE_HOURS).is_none() && t.attribute(ATTR_BOOSTER_LAST_INJECTION_DATETIME).is_none()) {
        for (effect_id, chance) in candidates(&c.sde, data.type_id) {
            if !data.attribute(chance).is_some_and(|v| v > 0.0 && v < 1.0) { continue; }
            let effect = c.sde.dogma.effect(effect_id).unwrap();
            if effect.effect_category.unwrap_or(0) != 0 { continue; }
            for modifier in effect.modifiers().iter().filter(|m| m.modified != m.modifying && m.op == 6 && m.domain == "shipID" && m.func == "ItemModifier") {
                let Some(ship_type) = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP) && t.attribute(modifier.modified).is_some_and(|v| v.is_finite() && v != 0.0)).map(|t| t.type_id) else { continue };
                let ship_a = item(&db, selected.character, ship_type, STATION, 1).await;
                let ship_b = item(&db, other.character, ship_type, STATION, 1).await;
                for character in [selected.character, other.character] {
                    for (skill, level) in requirements(&c.sde, data.type_id) {
                        db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0) ON CONFLICT (character_id,type_id) DO UPDATE SET level=GREATEST(game.skills.level,EXCLUDED.level)", &[&character, &skill, &level]).await.unwrap();
                    }
                }
                let a = item(&db, selected.character, data.type_id, STATION, 1).await;
                let b = item(&db, other.character, data.type_id, STATION, 1).await;
                for id in [a, b] { db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,10)", &[&id, &modifier.modifying]).await.unwrap(); }
                let on: Vec<_> = candidates(&c.sde, data.type_id).into_iter().map(|(id, _)| (id, if id == effect_id { 0.0 } else { 0.999999999999 })).collect();
                let off = candidates(&c.sde, data.type_id).into_iter().map(|(id, _)| (id, 0.999999999999)).collect();
                crate::dao::characters::selection::set_ship(&db, selected.character, ship_a).await.unwrap();
                let world = crate::space::World::new(db.clone(), c.sde.clone(), 1);
                recipient = Some(crate::services::tests::fixture::LiveRecipient::cold(&world, selected.character, None, &_market).await);
                let live = recipient.as_ref().unwrap();
                let fences = crate::services::effects::online::capture_online_fences(&world, &[selected.character]).await;
                let dispatch_world = &world;
                let left = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
                    let sde = c.sde.clone();
                    let draws = on.clone();
                    let fences = fences.clone();
                    Box::pin(async move {
                        activate_tx(tx, outbox, &sde, &fences, selected.character, None, a, Some(STATION), now, &draws, true).await
                    })
                }, |value, outbox| async move {
                    Ok((value, crate::services::tests::fixture::collected_notifications(outbox, dispatch_world)))
                }).await;
                let right = activate(&db, &c.sde, other.character, b, Some(STATION), now, true, off).await;
                if left.is_ok() && right.is_ok() {
                    let la = brain::get(&db, selected.character, a).await.unwrap().unwrap();
                    let lb = brain::get(&db, other.character, b).await.unwrap().unwrap();
                    let selected_id = la.effect_ids.as_ref().unwrap().contains(&effect_id);
                    let unselected_id = !lb.effect_ids.as_ref().unwrap().contains(&effect_id);
                    let yes = Fit::load(&db, c.sde.clone(), selected.character, ship_a).await.unwrap_or_else(|_| panic!("selected fit"));
                    let no = Fit::load(&db, c.sde.clone(), other.character, ship_b).await.unwrap_or_else(|_| panic!("unselected fit"));
                    let mut present = false; let mut absent = true;
                    yes.brain_modifiers(|key, m| { if key == Key::Booster(a) && std::ptr::eq(m, modifier) { present = true; } });
                    no.brain_modifiers(|key, m| { if key == Key::Booster(b) && std::ptr::eq(m, modifier) { absent = false; } });
                    let changed = [(Key::Char, Key::Char), (Key::Ship, Key::Ship), (Key::Booster(a), Key::Booster(b))].into_iter().any(|(x, y)| yes.value(x, modifier.modified) != no.value(y, modifier.modified));
                    let wire_yes = wire_contains(&yes, selected.character, data.type_id, modifier.modified, modifier.op);
                    let wire_no = wire_contains(&no, other.character, data.type_id, modifier.modified, modifier.op);
                    if selected_id && unselected_id && changed && wire_yes && !wire_no { assert!(present && absent); exercised = true; }
                    if exercised {
                        let (ty, modified, op) = (data.type_id, modifier.modified, modifier.op);
                        let expected = (yes.value(Key::Ship, modified), wire_yes);
                        assert_eq!(live.read(move |fit|
                            (fit.value(Key::Ship, modified), wire_contains(fit, selected.character, ty, modified, op))).await, expected);
                        let metadata = (la.activated_at, la.expires_at, la.effect_ids);
                        assert!(metadata.2.as_ref().is_some_and(|ids| !ids.is_empty() && ids.contains(&effect_id)));
                        for _ in 0..2 {
                            assert_eq!(live.read(move |fit| {
                                let inputs = fit.character_inputs();
                                let row = &inputs.brain.iter().find(|row| row.item.item_id == a).expect("selected booster").item;
                                (row.activated_at, row.expires_at, row.effect_ids.clone())
                            }).await, metadata);
                        }
                    }
                }
                recipient.take().unwrap().close().await;
                db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&selected.character]).await.unwrap();
                for id in [a, b, ship_a, ship_b] { db.execute("DELETE FROM game.items WHERE item_id=$1", &[&id]).await.unwrap(); }
                for character in [selected.character, other.character] { crate::scheduler::cancel(&db, &format!("{}:{character}", completion::KIND)).await.unwrap(); }
                if exercised { break 'types; }
            }
        }
    }
    }).await;
    if let Some(live) = recipient {
        live.close().await;
    }
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&selected.character]).await.unwrap();
    cleanup(&db, other).await;
    cleanup(&db, selected).await;
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
    assert!(exercised, "test SDE has no applicable chance modifier with distinct real activation outcomes");
}
