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

pub(super) struct FieldSnapshot {
    system: i64,
    asteroids: std::collections::HashSet<i64>,
    fields: Vec<(i64, i64, i64, Option<i64>)>,
}

impl FieldSnapshot {
    pub(super) async fn capture(r: &Resources, station: i64) -> Self {
        let system = r.sde.map.station(station).unwrap().solar_system_id.unwrap();
        let asteroids = r.db.query("SELECT asteroid_id FROM game.asteroids WHERE system_id=$1", &[&system]).await.unwrap()
            .into_iter().map(|row| row.get("asteroid_id")).collect();
        let fields = r.db.query("SELECT field_id, system_id, grown_at, depleted_at FROM game.asteroid_fields WHERE system_id=$1", &[&system]).await.unwrap()
            .into_iter().map(|row| (row.get("field_id"), row.get("system_id"), row.get("grown_at"), row.get("depleted_at"))).collect();
        Self { system, asteroids, fields }
    }

    pub(super) async fn restore(self, r: &Resources) {
        let rows = r.db.query("SELECT asteroid_id FROM game.asteroids WHERE system_id=$1", &[&self.system]).await.unwrap();
        for row in rows {
            let id: i64 = row.get("asteroid_id");
            if !self.asteroids.contains(&id) {
                r.world.remove_ball(self.system, id).await;
                r.db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2", &[&id, &self.system]).await.unwrap();
                assert!(r.db.query_opt("SELECT asteroid_id FROM game.asteroids WHERE asteroid_id=$1", &[&id]).await.unwrap().is_none());
                assert!(r.world.ball(self.system, id).await.is_none());
            }
        }
        let rows = r.db.query("SELECT field_id FROM game.asteroid_fields WHERE system_id=$1", &[&self.system]).await.unwrap();
        for row in rows {
            let id: i64 = row.get("field_id");
            if !self.fields.iter().any(|field| field.0 == id) {
                r.db.execute("DELETE FROM game.asteroid_fields WHERE field_id=$1 AND system_id=$2", &[&id, &self.system]).await.unwrap();
                assert!(r.db.query_opt("SELECT field_id FROM game.asteroid_fields WHERE field_id=$1", &[&id]).await.unwrap().is_none());
            }
        }
        for (id, system, grown, depleted) in self.fields {
            r.db.execute("UPDATE game.asteroid_fields SET system_id=$2, grown_at=$3, depleted_at=$4 WHERE field_id=$1", &[&id, &system, &grown, &depleted]).await.unwrap();
            let row = r.db.query_one("SELECT field_id, system_id, grown_at, depleted_at FROM game.asteroid_fields WHERE field_id=$1", &[&id]).await.unwrap();
            assert_eq!((row.get::<_, i64>("field_id"), row.get::<_, i64>("system_id"), row.get::<_, i64>("grown_at"), row.get::<_, Option<i64>>("depleted_at")), (id, system, grown, depleted));
        }
    }
}
