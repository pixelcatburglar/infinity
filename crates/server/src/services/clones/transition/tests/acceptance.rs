//! Database acceptance tests for augmentation custody during station jumps.
//! Exercises the clone transition and skill body-change adapter against game data;
//! ship clones and death transitions are outside this test module.
use super::*;
use crate::{consts::{dogma::{ATTR_BOOSTERNESS, ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES}, FILETIME_MINUTE, FLAG_BOOSTER}, db::outbox::{Effect, Notification}};
use marshal::Value;

fn notices(effects: &[Effect<CloneEffect>], event: &str) -> Vec<Vec<Value>> {
    effects.iter().filter_map(|effect| match effect {
        Effect::Notification(Notification::Pilot { event: name, args, .. }) if name == event => Some(args.clone()),
        _ => None,
    }).collect()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database with SDE"]
async fn jump_moves_only_nonfollowers_and_refreshes_actual_booster_loss() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (client, _peer) = client(db.clone(), f.character).await;
    let sde = client.sde.clone();
    let station = sde.map.stations().find(|station| {
        station.owner_id.is_some_and(|owner| sde.map.required_station_standing(owner, JUMP_CLONE_FACILITY).is_none())
            && station.solar_system_id.and_then(|id| sde.map.system(id)).is_some_and(|system| system.faction_id.is_none_or(|faction| sde.map.required_station_standing(faction, JUMP_CLONE_FACILITY).is_none()))
            && station.constellation_id.is_some() && station.region_id.is_some()
    }).expect("unrestricted NPC station").station_id;
    let implants: Vec<_> = sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER))
        .filter_map(|t| t.attribute(ATTR_IMPLANTNESS).filter(|v| *v > 0.0 && v.fract() == 0.0 && *v <= i32::MAX as f64).map(|slot| (t.type_id, slot as i32))).collect();
    let ordinary = *implants.first().expect("slotted implant");
    let follower = *implants.iter().find(|(_, slot)| *slot != ordinary.1).expect("implant in another slot");
    let boosters: Vec<_> = sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id == Some(GROUP_BOOSTER))
        .filter_map(|t| t.attribute(ATTR_BOOSTERNESS).filter(|v| *v > 0.0 && v.fract() == 0.0 && *v <= i32::MAX as f64).map(|slot| (t.type_id, slot as i32, t.effects().iter().map(|effect| effect.effect_id).collect::<Vec<_>>()))).collect();
    let removed = boosters.first().expect("slotted booster");
    let retained = boosters.iter().find(|item| item.1 != removed.1 && !item.2.is_empty()).expect("booster in another slot with effects");
    let owner = f.character;
    let now = filetime_now();
    let expiry = now + FILETIME_MINUTE;
    let capsule = creation::rookie_ship(&db, TYPE_CAPSULE, owner, station, FLAG_HANGAR).await.unwrap();
    let balance = policy::service_fee() * 3.0 + 1.0;
    db.execute("UPDATE game.characters SET ship_id=$2,station_id=$3,balance=$4::float8::numeric WHERE character_id=$1", &[&owner, &capsule, &station, &balance]).await.unwrap();
    let target = clones::create(&db, owner, station).await.unwrap();
    let mut ids = Vec::new();
    for (kind, slot, flag, follows) in [(ordinary.0, ordinary.1, FLAG_IMPLANT, 0.0), (follower.0, follower.1, FLAG_IMPLANT, 1.0), (removed.0, removed.1, FLAG_BOOSTER, 0.0), (retained.0, retained.1, FLAG_BOOSTER, 1.0)] {
        let item = transfers::insert(&db, kind, owner, owner, flag, 1).await.unwrap();
        db.execute("UPDATE game.items SET singleton=true,brain_slot=$2,brain_activated_at=$3,brain_expires_at=$4 WHERE item_id=$1", &[&item, &slot, &now, &expiry]).await.unwrap();
        assert!(dogma::set_override(&db, item, ATTR_FOLLOWS_JUMP_CLONES, follows).await.unwrap());
        ids.push(item);
    }
    db.execute("UPDATE game.items SET brain_effect_ids=$2 WHERE item_id=$1", &[&ids[3], &retained.2]).await.unwrap();
    for (item, credits) in [(ids[2], 2.0), (ids[3], 3.0)] {
        assert!(dogma::set_override(&db, item, ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES, credits).await.unwrap());
    }
    let target_item = transfers::insert(&db, ordinary.0, owner, owner, FLAG_IMPLANT, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true,brain_slot=$2,jump_clone_id=$3 WHERE item_id=$1", &[&target_item, &ordinary.1, &target.clone_id]).await.unwrap();
    // Overrides must be attached before the item becomes dormant.
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,$3)", &[&target_item, &ATTR_FOLLOWS_JUMP_CLONES, &0.0_f64]).await.unwrap();
    let _market = crate::services::market::lock().await;
    let (result, effects) = db.transaction(|tx, outbox: &mut Outbox<CloneEffect>| {
        let sde = sde.clone();
        Box::pin(async move {
            let id = Identity { char_id: owner, ship_id: Some(capsule), corp_id: 0, station_id: Some(station), system_id: None };
            jump_tx(tx, outbox, &sde, &id, station, target.clone_id, policy::service_fee(), false, now).await
        })
    }, |result, outbox| async move { Ok((result, outbox.into_entries())) }).await.unwrap();
    let rows = clones::clones(&db, owner).await.unwrap();
    assert_eq!(rows.len(), 1);
    let parked = custody::list(&db, owner, rows[0].clone_id).await.unwrap();
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].item_id, ids[0]);
    assert!(brain::get(&db, owner, ids[0]).await.unwrap().is_none());
    assert_eq!(brain::get(&db, owner, ids[1]).await.unwrap().unwrap().item_id, ids[1]);
    assert!(brain::get(&db, owner, ids[2]).await.unwrap().is_none());
    assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&ids[2]]).await.unwrap().is_none());
    let kept = brain::get(&db, owner, ids[3]).await.unwrap().unwrap();
    assert_eq!((kept.expires_at, kept.activated_at, kept.effect_ids), (Some(expiry), Some(now), Some(retained.2.clone())));
    assert_eq!(brain::get(&db, owner, target_item).await.unwrap().unwrap().item_id, target_item);
    assert_eq!(dogma::overrides(&db, target_item).await.unwrap()[0].value, 0.0);
    assert_eq!(notices(&effects, "OnNonDiminishingInjectionsRemoved"), vec![vec![]]);
    assert_eq!(notices(&effects, "OnNonDiminishingInjectionsAdded"), vec![vec![Value::Int(3)]]);
    assert_eq!(notices(&effects, "OnServerBoostersChanged").len(), 1);
    assert_eq!(notices(&effects, "OnServerImplantsChanged").len(), 1);
    let return_clone = rows[0].clone_id;
    let effects = db.transaction(|tx, outbox: &mut Outbox<CloneEffect>| {
        let sde = sde.clone();
        Box::pin(async move {
            let id = Identity { char_id: owner, ship_id: Some(result.capsule), corp_id: 0, station_id: Some(station), system_id: None };
            jump_tx(tx, outbox, &sde, &id, station, return_clone, policy::service_fee(), false, now).await
        })
    }, |_, outbox| async move { Ok(outbox.into_entries()) }).await.unwrap();
    for event in ["OnNonDiminishingInjectionsRemoved", "OnNonDiminishingInjectionsAdded", "OnServerBoostersChanged"] {
        assert!(notices(&effects, event).is_empty(), "unexpected {event}");
    }
    assert_eq!(brain::get(&db, owner, ids[3]).await.unwrap().unwrap().expires_at, Some(expiry));
    db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.jump_clones WHERE owner_id=$1", &[&owner]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database with SDE"]
async fn follower_slot_collision_refuses_and_rolls_back_all_property() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (client, _peer) = client(db.clone(), f.character).await;
    let sde = client.sde.clone();
    let station = sde.map.stations().find(|station| {
        station.owner_id.is_some_and(|owner| sde.map.required_station_standing(owner, JUMP_CLONE_FACILITY).is_none())
            && station.solar_system_id.and_then(|id| sde.map.system(id)).is_some_and(|system| system.faction_id.is_none_or(|faction| sde.map.required_station_standing(faction, JUMP_CLONE_FACILITY).is_none()))
            && station.constellation_id.is_some() && station.region_id.is_some()
    }).expect("unrestricted NPC station").station_id;
    let implant = sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER))
        .find_map(|t| t.attribute(ATTR_IMPLANTNESS).filter(|v| *v > 0.0 && v.fract() == 0.0 && *v <= i32::MAX as f64).map(|slot| (t.type_id, slot as i32))).expect("slotted implant");
    let owner = f.character;
    let capsule = creation::rookie_ship(&db, TYPE_CAPSULE, owner, station, FLAG_HANGAR).await.unwrap();
    let balance = policy::service_fee() * 2.0 + 1.0;
    db.execute("UPDATE game.characters SET ship_id=$2,station_id=$3,balance=$4::float8::numeric WHERE character_id=$1", &[&owner, &capsule, &station, &balance]).await.unwrap();
    let target = clones::create(&db, owner, station).await.unwrap();
    let follower = transfers::insert(&db, implant.0, owner, owner, FLAG_IMPLANT, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true,brain_slot=$2 WHERE item_id=$1", &[&follower, &implant.1]).await.unwrap();
    assert!(dogma::set_override(&db, follower, ATTR_FOLLOWS_JUMP_CLONES, 1.0).await.unwrap());
    let dormant: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,brain_slot,jump_clone_id) VALUES ($1,$2,$2,$3,1,true,$4,$5) RETURNING item_id", &[&implant.0, &owner, &FLAG_IMPLANT, &implant.1, &target.clone_id]).await.unwrap().get("item_id");
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,$3)", &[&dormant, &ATTR_FOLLOWS_JUMP_CLONES, &0.0_f64]).await.unwrap();
    let _market = crate::services::market::lock().await;
    let result = db.transaction(|tx, outbox: &mut Outbox<CloneEffect>| {
        let sde = sde.clone();
        Box::pin(async move {
            let id = Identity { char_id: owner, ship_id: Some(capsule), corp_id: 0, station_id: Some(station), system_id: None };
            jump_tx(tx, outbox, &sde, &id, station, target.clone_id, policy::service_fee(), false, filetime_now()).await
        })
    }, |_, _| async move { Ok(()) }).await;
    assert!(result.is_err());
    let rows = clones::clones(&db, owner).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].clone_id, target.clone_id);
    assert_eq!(brain::get(&db, owner, follower).await.unwrap().unwrap().item_id, follower);
    let parked = custody::list(&db, owner, target.clone_id).await.unwrap();
    assert_eq!(parked.len(), 1);
    assert_eq!(parked[0].item_id, dormant);
    assert_eq!(dogma::overrides(&db, follower).await.unwrap()[0].value, 1.0);
    assert_eq!(custody::overrides(&db, owner, target.clone_id, &[dormant]).await.unwrap()[0].value, 0.0);
    let state = clones::lock_character(&db, owner).await.unwrap().unwrap();
    assert_eq!((state.ship_id, state.station_id), (Some(capsule), Some(station)));
    assert_eq!(db.query_one("SELECT balance::float8 AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get::<_, f64>("balance"), balance);
    assert_eq!(db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1 AND item_id=ANY($2)", &[&owner, &vec![capsule, follower, dormant]]).await.unwrap().get::<_, i64>("n"), 3);
    db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.jump_clones WHERE owner_id=$1", &[&owner]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}
