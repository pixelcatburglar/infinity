//! A piloted loss never lists the hidden mode item in the killmail, valuation
//! or recovery manifests, and deletes the mode item and its timing row.
use super::*;
use infinity_core::consts::FLAG_HIDDEN_MODIFIERS;
use crate::lua::events::{combat::{CombatEntity, ConflictKind}, resources::{FactContext, FactOrigin}};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn loss_excludes_mode_item_and_deletes_stance_state() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let stances = &crate::sde::ship_stances::HULL_STANCES[0];
    let home = sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).unwrap().station_id;
    db.execute("UPDATE game.characters SET solar_system_id=$2, station_id=NULL, home_station_id=$3 WHERE character_id=$1", &[&fixture.character, &system, &home]).await.unwrap();
    let ship = creation::positioned(&db, stances.hull_type, fixture.character, system, [0.0; 3]).await.unwrap();
    db.execute("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true)", &[&stances.modes[0].1, &fixture.character, &ship, &FLAG_HIDDEN_MODIFIERS]).await.unwrap();
    db.execute("INSERT INTO game.ship_stances (ship_id,switched_at,ready_at) VALUES ($1,0,0)", &[&ship]).await.unwrap();
    let mode_id: i64 = db.query_one("SELECT item_id FROM game.items WHERE location_id=$1 AND flag_id=$2", &[&ship, &FLAG_HIDDEN_MODIFIERS]).await.unwrap().get("item_id");
    selection::set_ship(&db, fixture.character, ship).await.unwrap();
    let input = LossInput {
        position: [10.0, 20.0, 30.0], lethal_at: crate::client::filetime_now(), contributions: Vec::new(),
        source: DamageSource { force_origin: Some(FactOrigin::Gm), ..Default::default() },
        victim: CombatEntity { item_id: ship, type_id: stances.hull_type, incarnation: [0; 16], character_id: Some(fixture.character),
            corporation_id: Some(1), alliance_id: None, faction_id: None },
        context: FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: Some(ship),
            ship_type_id: Some(stances.hull_type), system_id: None, station_id: None, origin: FactOrigin::Gm },
        conflict: ConflictKind::PlayerVersusPlayer, pending: Arc::new(facts::PlayerLossContext::new(None, [0.0; 3])),
    };
    let identity = Identity { char_id: fixture.character, corp_id: 1, ship_id: Some(ship), station_id: None, system_id: Some(system) };
    let outcome = db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let (sde, input) = (sde.clone(), input.clone());
            Box::pin(async move { destroy_ship_in_transaction(tx, outbox, &identity, &sde, &input, None, &[]).await })
        },
        |outcome, _outbox| async move { Ok(outcome) },
    ).await.unwrap().unwrap();
    assert!(db.query_opt("SELECT 1 FROM game.items WHERE item_id=$1", &[&mode_id]).await.unwrap().is_none()
        && crate::dao::items::ship_stance::timing(&db, ship).await.unwrap().is_none());
    let blob: String = db.query_one("SELECT blob FROM game.killmails WHERE victim_item_id=$1", &[&ship]).await.unwrap().get("blob");
    assert!(!blob.contains(&format!("f={FLAG_HIDDEN_MODIFIERS}")), "killmail must not list the hidden mode item: {blob}");
    assert!(db.query("SELECT 1 FROM game.repairable_recovery_items WHERE item_id=$1", &[&mode_id]).await.unwrap().is_empty());
    cleanup(&db, fixture, &[ship, outcome.replacement_ship]).await;
}
