use super::super::*;
use crate::{db::{Db, outbox::{Effect, Notification}}, services::tests::{client, fixture}};

pub(super) const STATION: i64 = 60000001;

pub(super) async fn setup() -> (Db, fixture::Fixture, Client, tokio::net::TcpStream) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &STATION]).await.unwrap();
    let (c, peer) = client(db.clone(), f.character).await;
    (db, f, c, peer)
}

pub(super) fn implant(sde: &Sde) -> i32 {
    sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT)
        && t.group_id != Some(GROUP_BOOSTER) && t.attribute(ATTR_IMPLANTNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0)
        && requirements(sde, t.type_id).next().is_none()).expect("published implant without prerequisites required").type_id
}

pub(super) fn booster(sde: &Sde) -> i32 {
    sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT)
        && t.group_id == Some(GROUP_BOOSTER) && t.attribute(ATTR_BOOSTERNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0)
        && t.attribute(ATTR_BOOSTER_DURATION).is_some_and(|v| v > 0.0 && v.is_finite())
        && t.attribute(ATTR_BOOSTER_MAX_CHAR_AGE_HOURS).is_none() && t.attribute(ATTR_BOOSTER_LAST_INJECTION_DATETIME).is_none()
        && requirements(sde, t.type_id).next().is_none()).expect("unrestricted published booster required").type_id
}

pub(super) async fn item(db: &Db, owner: i64, ty: i32, location: i64, quantity: i64) -> i64 {
    crate::dao::items::transfers::insert(db, ty, owner, location, infinity_core::consts::FLAG_HANGAR, quantity).await.unwrap()
}

pub(super) async fn activate(db: &Db, sde: &Arc<Sde>, character: i64, item: i64, location: Option<i64>, now: i64, booster: bool, draws: Vec<(i32, f64)>) -> Result<(Value, Vec<Effect<OnlineChanged>>), CallError> {
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let fences = crate::services::effects::online::capture_online_fences(&world, &[character]).await;
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone(); let draws = draws.clone(); let fences = fences.clone();
        Box::pin(async move { activate_tx(tx, outbox, &sde, &fences, character, None, item, location, now, &draws, booster).await })
    }, |value, outbox| async move { Ok((value, outbox.into_entries())) }).await
}

pub(super) async fn destroy(db: &Db, sde: &Arc<Sde>, character: i64, item: i64, now: i64) -> Result<(Value, Vec<Effect<OnlineChanged>>), CallError> {
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let fences = crate::services::effects::online::capture_online_fences(&world, &[character]).await;
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone(); let fences = fences.clone();
        Box::pin(async move { destroy_tx(tx, outbox, &sde, &fences, character, item, now).await })
    }, |value, outbox| async move { Ok((value, outbox.into_entries())) }).await
}

pub(super) fn notices(effects: &[Effect<OnlineChanged>], event: &str) -> usize {
    effects.iter().filter(|e| matches!(e, Effect::Notification(Notification::Pilot { event: name, .. }) if name == event)).count()
}

pub(super) async fn fingerprint(db: &Db, character: i64) -> String {
    let job = format!("{}:{character}", completion::KIND);
    db.query_one("SELECT concat_ws('|', (SELECT row_to_json(c)::text FROM game.characters c WHERE character_id=$1), (SELECT coalesce(string_agg(row_to_json(i)::text, '' ORDER BY item_id),'') FROM game.items i WHERE owner_id=$1), (SELECT coalesce(string_agg(row_to_json(a)::text, '' ORDER BY a.item_id,a.attribute_id),'') FROM game.item_attributes a JOIN game.items i ON i.item_id=a.item_id WHERE i.owner_id=$1), (SELECT coalesce(string_agg(row_to_json(s)::text, '' ORDER BY type_id),'') FROM game.skills s WHERE character_id=$1), (SELECT coalesce(string_agg(row_to_json(q)::text, '' ORDER BY position),'') FROM game.skill_queue q WHERE character_id=$1), (SELECT coalesce(string_agg(row_to_json(h)::text, '' ORDER BY row_to_json(h)::text),'') FROM game.skill_history h WHERE character_id=$1), (SELECT coalesce(string_agg(row_to_json(j)::text, '' ORDER BY job_id),'') FROM game.jobs j WHERE job_id=$2)) AS state", &[&character, &job]).await.unwrap().get("state")
}

pub(super) async fn cleanup(db: &Db, f: fixture::Fixture) {
    crate::scheduler::cancel(db, &format!("{}:{}", completion::KIND, f.character)).await.unwrap();
    db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(db, f).await.unwrap();
}
