//! Multi-type probe recovery against a real SDE-backed database.
//!
//! Exercises scanMgr.RecoverProbes with live in-memory probes and persisted cargo;
//! expiry and refusal cases are covered by the parent tests.

use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn multi_type_probe_recovery_deposits_each_type_once() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let character = owner.character;
    let scenario_db = db.clone();
    let ids = [character * 100 + 53, character * 100 + 54];
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(scenario_db.clone(), character).await;
        let probes: Vec<_> = client.sde.types.iter().filter(|ty| {
            ty.group_id == Some(infinity_core::consts::GROUP_SCANNER_PROBE)
                && ty.published == Some(true)
                && ty.volume.is_some_and(|v| v.is_finite() && v > 0.0)
        }).take(2).map(|ty| (ty.type_id, ty.volume.unwrap())).collect();
        assert_eq!(probes.len(), 2, "SDE needs two published scanner probe types");
        assert_ne!(probes[0].0, probes[1].0);
        let volume = probes[0].1 + probes[1].1;
        let hull = client.sde.types.iter().find(|ty| {
            ty.published == Some(true)
                && ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
                && ty.group_id != Some(infinity_core::consts::GROUP_CAPSULE)
                && ty.capacity.is_some_and(|v| v.is_finite() && v >= volume)
        }).expect("published ship with enough cargo").type_id;
        let system = crate::dao::characters::ship::current(&scenario_db, character).await.unwrap().and_then(|location| location.system).unwrap();
        let ship = recovery_ship(&scenario_db, &mut client.session, character, system, hull).await;
        let expiry = filetime_now() + FILETIME_SECOND * 60;
        for (id, (type_id, _)) in ids.into_iter().zip(probes.iter().copied()) {
            state().probes.insert(id, Probe { char_id: character, system, type_id, pos: [0.0; 3], expiry, strength: 1.0 });
        }
        let foreign_system = client.sde.map.systems().find(|s| s.solar_system_id != system).unwrap().solar_system_id;
        let foreign = character * 100 + 55;
        state().probes.insert(foreign, Probe { char_id: character, system: foreign_system, type_id: probes[0].0, pos: [0.0; 3], expiry, strength: 1.0 });
        let args = [Value::list(ids.into_iter().chain([foreign, ids[0]]).map(Value::Int))];
        let kwargs = Value::None;
        let call = Call { service: "scanMgr", method: "RecoverProbes", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        let foreign_args = [Value::list([Value::Int(foreign)])];
        let foreign_call = Call { service: "scanMgr", method: "RecoverProbes", args: &foreign_args, kwargs: &kwargs, bound: true, bind_params: None };
        let before = client.outbox.len();
        assert_eq!(recover_probes(&mut client, &foreign_call, false).await.unwrap(), Value::list([]));
        assert!(state().probes.contains_key(&foreign));
        assert_eq!(client.outbox.len(), before);
        assert!(crate::dao::items::loading::load_items(&scenario_db, crate::dao::items::loading::ItemQuery::Location(ship)).await.unwrap().is_empty());
        assert_eq!(recover_probes(&mut client, &call, false).await.unwrap(), Value::list(ids.into_iter().map(Value::Int)));
        for id in ids { assert!(!state().probes.contains_key(&id)); }
        assert!(state().probes.contains_key(&foreign));
        assert_eq!(recover_probes(&mut client, &call, false).await.unwrap(), Value::list([]));
        let items = crate::dao::items::loading::load_items(&scenario_db, crate::dao::items::loading::ItemQuery::Location(ship)).await.unwrap();
        assert_eq!(items.len(), 2);
        for (type_id, _) in probes {
            let item = items.iter().find(|item| item.type_id == type_id).expect("recovered type in cargo");
            assert_eq!(item.quantity, 1);
            assert_eq!(item.flag_id, infinity_core::consts::FLAG_CARGO);
        }
        assert_eq!(recover_probes(&mut client, &foreign_call, true).await.unwrap(), Value::list([Value::Int(foreign)]));
        assert!(!state().probes.contains_key(&foreign));
    }).await;
    for id in ids.into_iter().chain([character * 100 + 55]) { state().probes.remove(&id); }
    db.execute("DELETE FROM game.items WHERE location_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&character]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&character]).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}
