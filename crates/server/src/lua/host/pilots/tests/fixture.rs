//! Dedicated database fixture for simulated pilot lifecycle tests.
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
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

/// Select a distinct starter system with a natural asteroid belt for each fixture.
pub(super) async fn creation_inputs(r: &Resources) -> (i64, i64, i64, i64) {
    static NEXT_SYSTEM: AtomicUsize = AtomicUsize::new(0);
    let races = crate::dao::sde::characters::races(&r.db).await.unwrap();
    let bloodline = crate::dao::sde::characters::bloodlines(&r.db).await.unwrap().first().unwrap().bloodline_id;
    let mut candidates = Vec::new();
    for race in races {
        let Some(loaded) = r.sde.characters.race(race.race_id) else { continue };
        if loaded.skills.is_empty() || loaded.ship_type_id.is_none() { continue; }
        for school in r.sde.characters.schools_for_race(race.race_id as i32) {
            if school.corporation_id.is_none() { continue; }
            for id in &school.starting_stations {
                let Some(station) = r.sde.map.station(*id) else { continue };
                let (Some(system), Some(constellation), Some(region)) =
                    (station.solar_system_id, station.constellation_id, station.region_id) else { continue };
                if r.sde.map.system(system).is_none() || r.sde.map.constellation(constellation).is_none()
                    || r.sde.map.region(region).is_none()
                    || !r.sde.map.celestials(system).any(|celestial| celestial.group_id == Some(i64::from(infinity_core::consts::GROUP_ASTEROID_BELT))) {
                    continue;
                }
                candidates.push((system, station.station_id, school.school_id, race.race_id));
            }
        }
    }
    candidates.sort_unstable();
    candidates.dedup_by_key(|candidate| candidate.0);
    let index = NEXT_SYSTEM.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |index| {
        (index < candidates.len()).then(|| index + 1)
    }).expect("exhausted distinct SDE starter systems with natural asteroid belts");
    let (_, station, school, race) = candidates[index];
    (race, bloodline, school, station)
}

pub(super) async fn creation_inputs_for_market(r: &Resources, ore: i32, price: f64, quantity: i64, region_range: i64) -> (i64, i64, i64, i64) {
    let price = crate::isk::IskAmount::from_isk(price).unwrap();
    loop {
        let inputs = creation_inputs(r).await;
        let station = r.sde.map.station(inputs.3).unwrap();
        let bids = crate::dao::market::orders::bids(&r.db, crate::client::filetime_now(), infinity_core::consts::FILETIME_DAY, ore, station.region_id.unwrap(), price).await.unwrap();
        if bids.first().is_some_and(|bid| bid.char_id.is_none()
            && (bid.at.id == inputs.3 || bid.range >= region_range)
            && bid.price >= price && bid.min_volume <= quantity) {
            return inputs;
        }
    }
}

pub(super) async fn cleanup(db: &Db, id: i64) {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        crate::services::tests::fixture::cleanup_progress(tx, &[id], &[id]).await?;
        tx.execute("DELETE FROM game.skills WHERE character_id=$1", &[&id]).await?;
        tx.execute("DELETE FROM game.items WHERE owner_id=$1", &[&id]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&id]).await?;
        Ok::<(), crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
}

pub(super) struct FieldSnapshot {
    system: i64,
    asteroids: std::collections::HashSet<i64>,
    fields: Vec<(i64, i64, i64, Option<i64>)>,
}

impl FieldSnapshot {
    pub(super) async fn capture(r: &Resources, station: i64) -> Self {
        let system = r.sde.map.station(station).unwrap().solar_system_id.unwrap();
        // The snapshot must include the initialized natural field baseline.
        r.world.system(system).await.unwrap();
        let asteroids = r.db.query("SELECT asteroid_id FROM game.asteroids WHERE system_id=$1", &[&system]).await.unwrap()
            .into_iter().map(|row| row.get("asteroid_id")).collect();
        let fields = r.db.query("SELECT field_id, system_id, grown_at, depleted_at FROM game.asteroid_fields WHERE system_id=$1", &[&system]).await.unwrap()
            .into_iter().map(|row| (row.get("field_id"), row.get("system_id"), row.get("grown_at"), row.get("depleted_at"))).collect();
        Self { system, asteroids, fields }
    }

    pub(super) async fn restore(self, r: &Resources) {
        let rows = r.db.query("SELECT asteroid_id, field_id FROM game.asteroids WHERE system_id=$1", &[&self.system]).await.unwrap();
        for row in rows {
            let id: i64 = row.get("asteroid_id");
            if !self.asteroids.contains(&id) {
                let field: i64 = row.get("field_id");
                let actor = r.world.asteroid_actor(self.system);
                assert_eq!(r.db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&id, &self.system, &field]).await.unwrap(), 1);
                assert!(r.db.query_opt("SELECT asteroid_id FROM game.asteroids WHERE asteroid_id=$1", &[&id]).await.unwrap().is_none());
                if let Some(actor) = actor {
                    assert!(crate::space::asteroids::publish_depletion(&r.world, self.system, &actor, id, field, None, None).await);
                    assert!(actor.rock_field(id).await.is_none());
                    assert!(r.world.ball(self.system, id).await.is_none());
                }
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
