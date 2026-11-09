//! Pending kick cancellation across real governance transitions and startup recovery.
use super::*;
use infinity_core::{consts::{FILETIME_SECOND, ROLE_DIRECTOR}};
use crate::{services::corps::{exit, pending_kicks}, dao::corporations::pending_kicks as dao};
use std::sync::Arc;

async fn claim(db: &Db, target: i64) -> crate::dao::jobs::Claim {
    let id = exit::job_id(target);
    let now = crate::client::filetime_now();
    db.execute("UPDATE game.jobs SET due_at=$2 WHERE job_id=$1", &[&id, &now]).await.unwrap();
    let others: Vec<String> = db.query("SELECT job_id FROM game.jobs WHERE job_id<>$1", &[&id]).await.unwrap().into_iter().map(|r| r.get("job_id")).collect();
    let excluded: Vec<&str> = others.iter().map(String::as_str).collect();
    crate::dao::jobs::claim(db, now, now + FILETIME_SECOND, 1, &excluded).await.unwrap().into_iter().find(|c| c.job.id == id).unwrap()
}
async fn gone(db: &Db, corp: i64, target: i64) {
    assert!(dao::get(db, corp, target).await.unwrap().is_none());
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&exit::job_id(target)]).await.unwrap().is_none());
}
async fn stale(db: &Db, world: &Arc<crate::space::World>, target: i64, claim: crate::dao::jobs::Claim) {
    let marker = super::super::fixtures::notification_marker(db, &[target]).await;
    pending_kicks::handle(db.clone(), world.clone(), claim.job).await.unwrap();
    assert_eq!(crate::dao::jobs::ack(db, &exit::job_id(target), claim.token).await.unwrap(), 0);
    assert_eq!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=92 AND notification_id>$2", &[&target, &marker]).await.unwrap().len(), 0);
}
async fn cleanup(db: &Db, people: &[fixture::Fixture], corps: &[i64], marker: i64) {
    let ids: Vec<i64> = people.iter().map(|p| p.character).collect();
    for id in &ids { db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&exit::job_id(*id)]).await.unwrap(); }
    db.execute("DELETE FROM game.corp_pending_kicks WHERE character_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.corp_member_events WHERE character_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.corp_role_history WHERE character_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1) AND notification_id>$2", &[&ids, &marker]).await.unwrap();
    db.execute("DELETE FROM game.employment_history WHERE char_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.corp_applications WHERE character_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id=ANY($1)", &[&corps]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0,corp_roles_hq=0,corp_roles_base=0,corp_roles_other=0,corp_title_mask=0 WHERE character_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=ANY($1)", &[&corps]).await.unwrap();
    for &person in people { fixture::cleanup(db, person).await.unwrap(); }
}
async fn client(db: &Db, id: i64, corp: i64) -> crate::client::Client {
    let (mut client, _) = tests::client(db.clone(), id).await;
    client.session.apply(vec![("charid", Value::Int(id)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
    client
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn title_issuer_loss_and_successor_cancel_claimed_jobs() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let issuer = fixture::create(&db).await.unwrap();
    let target = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let people = [ceo, issuer, target];
    let marker = super::super::fixtures::notification_marker(&db, &people.map(|p| p.character)).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let ids = [ceo.character, issuer.character, target.character];
        db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001 WHERE character_id=ANY($2)", &[&corp, &ids.as_slice()]).await.unwrap();
        db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo.character, &ROLE_DIRECTOR]).await.unwrap();
        db.execute("INSERT INTO game.corp_titles(corporation_id,title_id,name,roles) VALUES ($1,1,'legacy director',$2)", &[&corp, &ROLE_DIRECTOR]).await.unwrap();
        db.execute("UPDATE game.characters SET corp_title_mask=1 WHERE character_id=$1", &[&issuer.character]).await.unwrap();
        let now = crate::client::filetime_now();
        db.execute("UPDATE game.characters SET corp_roles_removed_at=$2 WHERE character_id=$1", &[&target.character, &now]).await.unwrap();
        let mut director = client(&db, issuer.character, corp).await;
        rpc(&mut director, "KickOutMember", &[Value::Int(target.character)], corp).await.unwrap();
        let old = claim(&db, target.character).await;
        let mut chief = client(&db, ceo.character, corp).await;
        rpc(&mut chief, "DeleteTitle", &[Value::Int(1)], corp).await.unwrap();
        gone(&db, corp, target.character).await;
        stale(&db, &chief.world, target.character, old).await;
        db.execute("UPDATE game.characters SET corp_roles=$2,corp_title_mask=0 WHERE character_id=$1", &[&issuer.character, &ROLE_DIRECTOR]).await.unwrap();
        rpc(&mut director, "KickOutMember", &[Value::Int(target.character)], corp).await.unwrap();
        let old = claim(&db, target.character).await;
        let skill = crate::services::corps::SKILL_CORPORATION_MANAGEMENT;
        let level = i32::try_from(crate::config::game().corporations.min_management_skill_level).unwrap().max(1);
        db.execute("INSERT INTO game.skills(character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0)", &[&target.character, &skill, &level]).await.unwrap();
        rpc(&mut chief, "ResignFromCEO", &[Value::Int(target.character)], corp).await.unwrap();
        gone(&db, corp, target.character).await;
        stale(&db, &chief.world, target.character, old).await;
        assert_eq!(crate::dao::corporations::registry::ceo_of(&db, corp).await.unwrap(), Some(target.character));
        assert_eq!(db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&target.character]).await.unwrap().get::<_, i64>("corporation_id"), corp);
    }).await;
    cleanup(&db, &people, &[corp], marker).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn application_move_cancels_claimed_source_job() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let destination_ceo = fixture::create(&db).await.unwrap();
    let target = fixture::create(&db).await.unwrap();
    let source = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let destination = fixture::create_corporation(&db, destination_ceo.character, 60000001).await.unwrap();
    let people = [ceo, destination_ceo, target];
    let marker = super::super::fixtures::notification_marker(&db, &people.map(|p| p.character)).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001 WHERE character_id=ANY($2)", &[&source, &vec![ceo.character,target.character]]).await.unwrap();
        db.execute("UPDATE game.characters SET corporation_id=$2,corp_roles=$3,station_id=60000001 WHERE character_id=$1", &[&destination_ceo.character, &destination, &ROLE_DIRECTOR]).await.unwrap();
        db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo.character, &ROLE_DIRECTOR]).await.unwrap();
        db.execute("UPDATE game.corporations SET member_limit=10,welcome_mail='Welcome' WHERE corporation_id=$1", &[&destination]).await.unwrap();
        let now = crate::client::filetime_now();
        db.execute("UPDATE game.characters SET corp_roles_removed_at=$2 WHERE character_id=$1", &[&target.character, &now]).await.unwrap();
        let mut chief = client(&db, ceo.character, source).await;
        rpc(&mut chief, "KickOutMember", &[Value::Int(target.character)], source).await.unwrap();
        let old = claim(&db, target.character).await;
        let past = now - (crate::config::game().corporations.role_removal_cooldown_secs + 1) * FILETIME_SECOND;
        db.execute("UPDATE game.characters SET corp_roles_removed_at=$2 WHERE character_id=$1", &[&target.character, &past]).await.unwrap();
        let app: i64 = db.query_one("INSERT INTO game.corp_applications(corporation_id,character_id,application_text,status,created) VALUES ($1,$2,'application',6,123) RETURNING application_id", &[&destination, &target.character]).await.unwrap().get("application_id");
        let mut member = client(&db, target.character, source).await;
        let args = [Value::Int(app), Value::Int(target.character), Value::Int(destination), Value::unicode("application"), Value::Int(2), Value::unicode("response"), Value::None];
        rpc(&mut member, "UpdateApplicationOffer", &args, source).await.unwrap();
        gone(&db, source, target.character).await;
        stale(&db, &member.world, target.character, old).await;
        assert_eq!(db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&target.character]).await.unwrap().get::<_, i64>("corporation_id"), destination);
    }).await;
    cleanup(&db, &people, &[source, destination], marker).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn startup_reconciles_invalid_held_future_and_due_intents() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let issuer = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let held = fixture::create(&db).await.unwrap();
    let future = fixture::create(&db).await.unwrap();
    let due = fixture::create(&db).await.unwrap();
    let invalid = fixture::create(&db).await.unwrap();
    let moved = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let people = [ceo, issuer, outsider, held, future, due, invalid, moved];
    let marker = super::super::fixtures::notification_marker(&db, &people.map(|p| p.character)).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (manager, _) = tests::client(db.clone(), ceo.character).await;
        crate::services::corps::initialize(&db, &manager.sde).await.unwrap();
        let members = [ceo.character, issuer.character, held.character, future.character, due.character, invalid.character];
        db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001 WHERE character_id=ANY($2)", &[&corp, &members.as_slice()]).await.unwrap();
        db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=ANY($1)", &[&vec![ceo.character,issuer.character], &ROLE_DIRECTOR]).await.unwrap();
        db.execute("UPDATE game.characters SET corp_roles_hq=$2 WHERE character_id=$1", &[&held.character, &infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
        let now = crate::client::filetime_now();
        db.execute("UPDATE game.characters SET corp_roles_removed_at=$2 WHERE character_id=$1", &[&future.character, &now]).await.unwrap();
        for (target, actor) in [(held.character,issuer.character), (future.character,issuer.character), (due.character,issuer.character), (invalid.character,outsider.character), (moved.character,issuer.character), (ceo.character,issuer.character)] {
            let row = dao::PendingKick { corporation_id: corp, character_id: target, issuer_id: actor, requested_at: now, eligible_at: Some(now), reason: 1 };
            dao::upsert(&db, &row).await.unwrap();
            pending_kicks::schedule(&db, &row).await.unwrap();
        }
        crate::services::corps::initialize(&db, &manager.sde).await.unwrap();
        for target in [invalid.character, moved.character, ceo.character] { gone(&db, corp, target).await; }
        let held_row = dao::get(&db, corp, held.character).await.unwrap().unwrap();
        assert_eq!((held_row.issuer_id, held_row.requested_at, held_row.eligible_at), (issuer.character, now, None));
        assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&exit::job_id(held.character)]).await.unwrap().is_none());
        let future_row = dao::get(&db, corp, future.character).await.unwrap().unwrap();
        assert_eq!((future_row.issuer_id, future_row.requested_at, future_row.eligible_at), (issuer.character, now, Some(exit::deadline(now).unwrap())));
        let job = db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&exit::job_id(future.character)]).await.unwrap();
        assert_eq!(job.get::<_, i64>("due_at"), exit::deadline(now).unwrap());
        let due_row = dao::get(&db, corp, due.character).await.unwrap().unwrap();
        assert_eq!((due_row.issuer_id, due_row.requested_at), (issuer.character, now));
        let due_at = due_row.eligible_at.unwrap();
        assert!(due_at >= now && due_at <= crate::client::filetime_now());
        assert_eq!(db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&exit::job_id(due.character)]).await.unwrap().get::<_, i64>("due_at"), due_at);
    }).await;
    cleanup(&db, &people, &[corp], marker).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn claimed_job_discards_invalid_current_issuer_without_departure() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let issuer = fixture::create(&db).await.unwrap();
    let target = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let people = [ceo, issuer, target];
    let marker = super::super::fixtures::notification_marker(&db, &people.map(|p| p.character)).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let ids = [ceo.character, issuer.character, target.character];
        db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001 WHERE character_id=ANY($2)", &[&corp, &ids.as_slice()]).await.unwrap();
        db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=ANY($1)", &[&vec![ceo.character,issuer.character], &ROLE_DIRECTOR]).await.unwrap();
        let now = crate::client::filetime_now();
        db.execute("UPDATE game.characters SET corp_roles_removed_at=$2 WHERE character_id=$1", &[&target.character, &now]).await.unwrap();
        let mut director = client(&db, issuer.character, corp).await;
        rpc(&mut director, "KickOutMember", &[Value::Int(target.character)], corp).await.unwrap();
        let old = claim(&db, target.character).await;
        db.execute("UPDATE game.characters SET corp_roles=0 WHERE character_id=$1", &[&issuer.character]).await.unwrap();
        pending_kicks::handle(db.clone(), director.world.clone(), old.job).await.unwrap();
        assert_eq!(crate::dao::jobs::ack(&db, &exit::job_id(target.character), old.token).await.unwrap(), 0);
        gone(&db, corp, target.character).await;
        assert_eq!(db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&target.character]).await.unwrap().get::<_, i64>("corporation_id"), corp);
        assert!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=92 AND notification_id>$2", &[&target.character, &marker]).await.unwrap().is_empty());
    }).await;
    cleanup(&db, &people, &[corp], marker).await;
    result.unwrap();
}
