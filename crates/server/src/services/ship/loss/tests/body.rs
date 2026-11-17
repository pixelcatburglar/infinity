//! Piloted-hull body loss through the real actor settlement: an ordinary hull
//! keeps the active brain while a capsule-group hull dies with only its
//! unprotected active augmentations and respawns at the home station.
use super::*;
use infinity_core::consts::{dogma::ATTR_NON_DESTRUCTIBLE, CATEGORY_IMPLANT, FLAG_IMPLANT, GROUP_BOOSTER, GROUP_CAPSULE};
use crate::dao::items::{brain, clones as custody, transfers};

/// Insert an active implant row; `protection` writes the instance
/// non-destruction override. Returns the item id.
async fn implant(db: &Db, owner: i64, kind: i32, slot: i32, protection: Option<f64>) -> i64 {
    let item = transfers::insert(db, kind, owner, owner, FLAG_IMPLANT, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true, brain_slot=$2 WHERE item_id=$1", &[&item, &slot]).await.unwrap();
    if let Some(value) = protection {
        db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,$3)", &[&item, &ATTR_NON_DESTRUCTIBLE, &value]).await.unwrap();
    }
    item
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn piloted_capsule_loss_destroys_only_unprotected_active_brain() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = fixture.character;
    let mut victims = Vec::new();
    let result = catch_scenario(async {
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let world = World::new(db.clone(), sde.clone(), 0);
    world.system(system).await.unwrap();
    let home = sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).unwrap();
    db.execute("UPDATE game.characters SET home_station_id=$2 WHERE character_id=$1", &[&character, &home.station_id]).await.unwrap();
    let mut kinds = sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER)).map(|t| t.type_id);
    let (lost_kind, kept_kind) = (kinds.next().unwrap(), kinds.next().unwrap());
    let clone: i64 = db.query_one("INSERT INTO game.jump_clones (owner_id,location_id) VALUES ($1,$2) RETURNING clone_id", &[&character, &home.station_id]).await.unwrap().get("clone_id");
    let dormant = transfers::insert(&db, kept_kind, character, character, FLAG_IMPLANT, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true, brain_slot=1, jump_clone_id=$2 WHERE item_id=$1", &[&dormant, &clone]).await.unwrap();

    // An ordinary hull dies without touching the active brain.
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.group_id != Some(GROUP_CAPSULE)
        && t.group_id.is_some_and(|group| sde.repairable_ship.enters_recovery(t.type_id, group, CATEGORY_SHIP)
            && !sde.repairable_ship.unrepairable(CATEGORY_SHIP, group, t.type_id))).unwrap().type_id;
    let ship = creation::positioned(&db, hull, character, system, [1.0; 3]).await.unwrap();
    victims.push(ship);
    db.execute("UPDATE game.characters SET station_id=NULL, solar_system_id=$2, ship_id=$3 WHERE character_id=$1", &[&character, &system, &ship]).await.unwrap();
    let carried = implant(&db, character, lost_kind, 2, None).await;
    let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &FreeShip { item_id: ship, type_id: hull, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [1.0; 3] }, character, 0.0).await.ok().unwrap();
    world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    assert!(apply_damage_guarded(&db, &world, system, ship, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true, |_| true).await.ok().unwrap().unwrap().lethal);
    assert!(brain::get(&db, character, carried).await.unwrap().is_some());
    assert!(db.query_opt("SELECT group_id FROM game.repairable_recovery_groups WHERE beneficiary_id=$1 AND source_item_id=$2", &[&character, &ship]).await.unwrap().is_some());

    // A piloted capsule dies with only the unprotected active rows; the
    // headless settlement commits with no client connection.
    let capsule = creation::positioned(&db, TYPE_CAPSULE, character, system, [2.0; 3]).await.unwrap();
    victims.push(capsule);
    db.execute("UPDATE game.characters SET station_id=NULL, solar_system_id=$2, ship_id=$3 WHERE character_id=$1", &[&character, &system, &capsule]).await.unwrap();
    let lost = implant(&db, character, lost_kind, 3, None).await;
    let kept = implant(&db, character, kept_kind, 4, Some(1.0)).await;
    let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &FreeShip { item_id: capsule, type_id: TYPE_CAPSULE, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [2.0; 3] }, character, 0.0).await.ok().unwrap();
    world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    assert!(apply_damage_guarded(&db, &world, system, capsule, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true, |_| true).await.ok().unwrap().unwrap().lethal);
    for item in [carried, lost] {
        assert!(brain::get(&db, character, item).await.unwrap().is_none());
    }
    assert!(brain::get(&db, character, kept).await.unwrap().is_some());
    assert_eq!(custody::list(&db, character, clone).await.unwrap().iter().map(|i| i.item_id).collect::<Vec<_>>(), [dormant]);
    let replacement = selection::ship_id(&db, character).await.unwrap().unwrap();
    assert_ne!(replacement, capsule);
    let row = db.query_one("SELECT type_id,location_id FROM game.items WHERE item_id=$1", &[&replacement]).await.unwrap();
    assert_eq!((row.get::<_, i32>("type_id"), row.get::<_, i64>("location_id")), (TYPE_CAPSULE, home.station_id));
    let location = crate::dao::characters::ship::current(&db, character).await.unwrap().unwrap();
    assert_eq!((location.station, location.ship), (Some(home.station_id), Some(replacement)));
    // The killmail records exactly the destroyed augmentations, and the settled
    // valuation classifies them as destroyed implants.
    let blob: String = db.query_one("SELECT blob FROM game.killmails WHERE victim_item_id=$1", &[&capsule]).await.unwrap().get("blob");
    assert!(blob.contains(&format!("t={lost_kind} f={FLAG_IMPLANT}")));
    assert!(!blob.contains(&format!("t={kept_kind}")));
    let settled = crate::db::outbox::tests::events(&db, "LossSettled", character).await;
    let valuation = settled.iter().find_map(|event| match event {
        GameEvent::LossSettled { victim, valuation, .. } if victim.item_id == capsule => Some(valuation),
        _ => None,
    }).unwrap();
    assert!(valuation.components.iter().any(|c| c.category == crate::lua::events::combat::ValuationCategory::DestroyedImplant && c.type_id == lost_kind));
    assert!(!valuation.components.iter().any(|c| c.type_id == kept_kind));

    // A different capsule-group type takes the same medical branch, never the
    // escape-bay survivor path.
    let alternate = sde.types.iter().find(|t| t.group_id == Some(GROUP_CAPSULE) && t.type_id != TYPE_CAPSULE).expect("alternate capsule-group type").type_id;
    let alt = creation::positioned(&db, alternate, character, system, [3.0; 3]).await.unwrap();
    victims.push(alt);
    db.execute("UPDATE game.characters SET station_id=NULL, solar_system_id=$2, ship_id=$3 WHERE character_id=$1", &[&character, &system, &alt]).await.unwrap();
    let prepared = crate::services::ship::balls::piloted(&db, sde.clone(), &FreeShip { item_id: alt, type_id: alternate, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [3.0; 3] }, character, 0.0).await.ok().unwrap();
    world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
    assert!(apply_damage_guarded(&db, &world, system, alt, [0.0; 4], DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true, |_| true).await.ok().unwrap().unwrap().lethal);
    let replacement = selection::ship_id(&db, character).await.unwrap().unwrap();
    let row = db.query_one("SELECT type_id,location_id FROM game.items WHERE item_id=$1", &[&replacement]).await.unwrap();
    assert_eq!((row.get::<_, i32>("type_id"), row.get::<_, i64>("location_id")), (TYPE_CAPSULE, home.station_id));
    assert!(brain::get(&db, character, kept).await.unwrap().is_some());
    assert!(db.query("SELECT 1 FROM game.repairable_recovery_groups WHERE source_item_id=ANY($1)", &[&[capsule, alt].as_slice()]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.repairable_recovery_items WHERE item_id=ANY($1)", &[&[carried, lost, kept, dormant].as_slice()]).await.unwrap().is_empty());
    }).await;
    db.execute("DELETE FROM game.items WHERE owner_id=$1 AND jump_clone_id IS NOT NULL", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.jump_clones WHERE owner_id=$1", &[&character]).await.unwrap();
    cleanup(&db, fixture, &victims).await;
    resume_scenario(result);
}
