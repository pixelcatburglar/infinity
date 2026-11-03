//! Dedicated database fixture for simulated pilot lifecycle tests.
use std::sync::Arc;
use crate::{db::Db, sde::Sde, space::World, chat::Hub, lua::host::{Resources, pilots::Pilots}};

pub(super) async fn resources() -> Resources {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let hub = Hub::new("test", db.clone(), world.clone());
    let pilots = Pilots::new(&db, hub).await.unwrap();
    Resources { db, sde, world, pilots, store_value_limit_bytes: 65536 }
}

pub(super) async fn creation_inputs(r: &Resources) -> (i64, i64, i64, i64) {
    let races = crate::dao::sde::characters::races(&r.db).await.unwrap();
    let bloodline = crate::dao::sde::characters::bloodlines(&r.db).await.unwrap().first().unwrap().bloodline_id;
    let (race, school, station) = races.iter().find_map(|race| {
        let loaded = r.sde.characters.race(race.race_id)?;
        if loaded.skills.is_empty() || loaded.ship_type_id.is_none() { return None; }
        r.sde.characters.schools_for_race(race.race_id as i32).find_map(|school| {
            school.corporation_id?;
            let station = school.starting_stations.iter().filter_map(|id| r.sde.map.station(*id)).find(|station| station.solar_system_id.is_some() && station.constellation_id.is_some() && station.region_id.is_some())?;
            Some((race.race_id, school.school_id, station.station_id))
        })
    }).expect("SDE race with starting ship, skills and school");
    (race, bloodline, school, station)
}

pub(super) async fn cleanup(db: &Db, id: i64) {
    db.execute("DELETE FROM game.skills WHERE character_id=$1", &[&id]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&id]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&id]).await.unwrap();
}
