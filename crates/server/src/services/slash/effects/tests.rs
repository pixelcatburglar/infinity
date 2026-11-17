use super::*;

#[test]
fn unsupported_arguments_are_exact() {
    assert!(valid_args(&["unsupported".into()]));
    for args in [&[][..], &["unsupported", "extra"][..], &["other"][..], &["Unsupported"][..]] {
        assert!(!valid_args(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()));
    }
}

#[test]
fn unsupported_output_includes_each_pair_in_snapshot_order() {
    assert_eq!(messages::gm::gm_effects_unsupported(&[]), "尚未记录未支持的效果。");
    let effects = vec![("alpha".to_string(), 42), ("beta".to_string(), 7)];
    assert_eq!(messages::gm::gm_effects_unsupported(&effects), "alpha（类型 ID：42）\nbeta（类型 ID：7）");
}

use crate::services::{combat::tests::remote_repair_db as fixture, slash::tests::support};
use marshal::Value;
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn effect_command_flows_from_slash_to_the_owning_observer() {
    let db = { crate::tests::init_config(); crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap() };
    let sde = crate::tests::shared_sde(&db).await;
    let has_guid = |guid: &str, t: &crate::sde::types::TypeData| t.effects().iter().filter_map(|e| sde.dogma.effect(e.effect_id)).any(|e| e.guid.as_deref().is_some_and(|g| g.split(',').any(|tok| tok.trim() == guid)));
    let own_delay = |t: &crate::sde::types::TypeData| sde.dogma.attributes_by_name("damageDelayDuration").any(|a| t.attribute(a.attribute_id).is_some());
    let module = sde.types.iter().filter(|t| t.category_id == Some(infinity_core::consts::CATEGORY_MODULE) && sde.types.attributes(t.type_id).next().is_some()).min_by_key(|t| (own_delay(*t), t.type_id)).unwrap().type_id;
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let f = fixture::create(&db, ship_type, module, 42_000).await.unwrap();
    let result = crate::tests::catch_scenario(async {
        let (mut c, _peer) = crate::services::tests::client_with_sde(db.clone(), f.source, sde.clone()).await;
        c.login.role = infinity_core::consts::GM_ROLES;
        c.session.apply(vec![("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(42_000))]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut space = crate::space::System::new(42_000, std::sync::Arc::default(), 12_345);
        space.insert(fixture::ball(f.ship, ship_type, f.source));
        space.insert(fixture::ball(f.victim, ship_type, f.target));
        crate::space::system::tests::events::install_publication_actor(&c.world, space);
        c.world.observe(42_000, c.login.client_id, f.ship, tx).await.unwrap();
        while let Ok(delivery) = rx.try_recv() {
            match delivery {
                crate::space::SceneDelivery::Destiny(_) => {}
                crate::space::SceneDelivery::Notify { event, .. } => assert_eq!(event, "OnFleetStateChange"),
            }
        }
        assert!(matches!(support::rpc(&mut c, "/effect list").await.unwrap(), Value::Unicode(t) if t.contains("effects.TurboLaser") && t.contains(messages::gm::GM_VISUAL_EXCLUDED_CLOAK)));
        assert!(support::rpc(&mut c, "/effect cloak").await.is_err());
        let mut cases: Vec<(String, bool, Result<(Option<i64>, Option<i32>, bool, &'static str), String>)> = vec![
            ("/effect jump_in".into(), false, Ok((None, None, false, "effects.JumpIn"))), ("/effect remote_armour_repair".into(), false, Err(messages::gm::GM_VISUAL_TARGET_REQUIRED.into())),
            ("/effect remote_armour_repair".into(), true, Ok((Some(f.victim), None, false, "effects.RemoteArmourRepair")))];
        let mut covered = [false; 3];
        for entry in gm_effects::CATALOGUE.iter().filter(|e| e.disposition.is_ok_and(|v| v.needs_module_type())) {
            let mut ids: Vec<i32> = sde.types.iter().filter(|&t| has_guid(entry.guid, t)).map(|t| t.type_id).collect();
            ids.sort_unstable(); covered[ids.len().min(2)] = true;
            cases.push((format!("/effect {}", entry.name), true, match ids.as_slice() { [] => Err(messages::gm::GM_VISUAL_MODULE_REQUIRED.into()), [one] => Ok((Some(f.victim), Some(*one), false, entry.guid)),
                _ => Err(super::super::finish_list(ids.iter().take(super::super::LIST_ROW_LIMIT).fold(messages::gm::GM_VISUAL_MODULE_AMBIGUOUS.to_string(), |mut t, id| { t.push_str(&format!("\n{id}")); t }), ids.len().saturating_sub(super::super::LIST_ROW_LIMIT))) }));
            if let &[first, ..] = ids.as_slice() { let pick = *ids.iter().min_by_key(|id| (own_delay(sde.types.get(**id).unwrap()), **id)).unwrap_or(&first); let other = sde.types.iter().map(|t| t.type_id).find(|id| *id > 0 && !ids.contains(id)).unwrap();
                cases.push((format!("/effect {} moduleType={pick}", entry.name), true, Ok((Some(f.victim), Some(pick), false, entry.guid)))); cases.push((format!("/effect {} moduleType={other}", entry.name), true, Err(messages::gm::GM_VISUAL_MODULE_MISMATCH.into()))); }
            else { cases.push((format!("/effect {} moduleType={module}", entry.name), true, Ok((Some(f.victim), Some(module), true, entry.guid)))); cases.push((format!("/effect {} moduleType={ship_type}", entry.name), true, Err(messages::gm::GM_VISUAL_MODULE_INVALID.into()))); }
        }
        assert_eq!(covered, [true; 3], "loaded data must hold zero, one and several weapon-candidate situations");
        for (command, locked, expected) in cases {
            fixture::set_locked_target(f.source, f.victim, locked);
            let before = c.world.with(42_000, |s| s.current_time()).await.unwrap();
            match (support::rpc(&mut c, &command).await, expected) {
                (Ok(Value::Unicode(reply)), Ok((target, module_type, unchecked, guid))) => {
                    let entry = gm_effects::find(command.split_whitespace().nth(1).unwrap()).unwrap();
                    assert_eq!(reply, messages::gm::gm_visual_effect_sent(entry.name, target, module_type, false, entry.disposition.unwrap().one_shot(), unchecked, module_type.is_some_and(|id| !own_delay(sde.types.get(id).unwrap()))));
                    assert!(marshal::loads(&support::wire(Ok(Value::unicode(reply)))).is_ok());
                    let batch = crate::space::system::tests::events::destiny(rx.try_recv().unwrap()); let after = c.world.with(42_000, |s| s.current_time()).await.unwrap();
                    let fx: Vec<&[Value]> = batch.iter().filter_map(|e| e.items()?.get(1)?.items()).filter(|c| c.first() == Some(&Value::str("OnSpecialFX"))).filter_map(|c| c.get(1)?.items()).collect();
                    assert!(batch.iter().all(|e| e.items().and_then(|p| p.first()).and_then(Value::as_int).is_some_and(|t| (before..=after).contains(&t))));
                    assert!(fx.len() == 1 && fx[0].len() == 14 && fx[0][..8] == [Value::Int(f.ship), Value::None, module_type.map(|id| Value::Int(id.into())).unwrap_or(Value::None),
                        target.map(Value::Int).unwrap_or(Value::None), Value::None, Value::str(guid), Value::Bool(false), Value::Int(1)]);
                }
                (Err(CallError::User { dict, .. }), Err(text)) => assert!(dict.iter().any(|(k, v)| k == "reason" && v.as_text().as_deref() == Some(text.as_str())) && rx.try_recv().is_err(), "{command} expected {text:?} got {dict:?}"),
                (result, _) => panic!("unexpected slash result: {result:?}"),
            }
        }
        assert!(support::rpc(&mut c, "/effect stop-all").await.is_ok());
    }).await;
    fixture::set_locked_target(f.source, f.victim, false);
    fixture::cleanup(&db, f).await.unwrap();
    result.unwrap();
}
