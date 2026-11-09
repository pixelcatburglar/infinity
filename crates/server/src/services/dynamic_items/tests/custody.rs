//! Custody and reservation regressions for persisted mutation inputs.
use super::*;
use infinity_core::consts::{FLAG_CORP_SAG1, INDUSTRY_JOB_STATUS_INSTALLED, ROLE_HANGAR_CAN_TAKE_1};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with loaded SDE; run serially"]
async fn deployed_and_active_job_inputs_are_refused() {
    let f = Arc::new(Fixture::new().await);
    let scenario = f.clone();
    let outcome = tokio::spawn(async move {
        let f = scenario;
        let dispatched = Arc::new(AtomicBool::new(false));
        let system = f.station + 1;
        f.db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&f.source, &system]).await.unwrap();
        let before = f.fingerprint().await;
        let context = Context { system: Some(system), ..f.context() };
        assert!(matches!(f.create(context, f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&f.source, &f.station]).await.unwrap();
        f.db.execute("INSERT INTO game.industry_jobs (installer_id,owner_id,activity_id,blueprint_id,blueprint_type_id,blueprint_copy,facility_id,solar_system_id,runs,cost,time_seconds,start_date,end_date,status) VALUES ($1,$1,1,$2,$3,false,$4,$5,1,0,1,0,1,$6)", &[&f.owner, &f.source, &f.source_type, &f.station, &system, &INDUSTRY_JOB_STATUS_INSTALLED]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(f.context(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        assert!(!dispatched.load(Ordering::SeqCst));
    }).await;
    f.db.execute("DELETE FROM game.industry_jobs WHERE installer_id=$1", &[&f.owner]).await.unwrap();
    f.cleanup().await;
    outcome.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with loaded SDE; run serially"]
async fn office_custody_requires_current_division_take_role() {
    let f = Arc::new(Fixture::new().await);
    let ceo: i64 = f.db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&f.account]).await.unwrap().get("character_id");
    let corp: i64 = f.db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES (nextval('game.corporations_corporation_id_seq')::text,nextval('game.corporations_corporation_id_seq')::text,$1,$1,$2,0) RETURNING corporation_id", &[&ceo, &f.station]).await.unwrap().get("corporation_id");
    let office = crate::dao::items::transfers::insert(&f.db, f.source_type, corp, f.station, FLAG_HANGAR, 1).await.unwrap();
    crate::dao::offices::insert(&f.db, office, corp, f.station, 1).await.unwrap();
    let scenario = f.clone();
    let outcome = tokio::spawn(async move {
        let f = scenario;
        f.db.execute("UPDATE game.characters SET corporation_id=$2,corp_roles=0,corp_roles_hq=0 WHERE character_id=$1", &[&f.owner, &corp]).await.unwrap();
        f.db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&f.source, &office, &FLAG_CORP_SAG1]).await.unwrap();
        let dispatched = Arc::new(AtomicBool::new(false));
        let before = f.fingerprint().await;
        assert!(matches!(f.create(f.context(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("UPDATE game.characters SET corp_roles_hq=$2 WHERE character_id=$1", &[&f.owner, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(f.context(), f.mutator, f.source, true, dispatched.clone()).await, Err(CallError::Internal(_))));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("UPDATE game.items SET flag_id=$2 WHERE item_id=$1", &[&f.source, &(FLAG_CORP_SAG1 + 1)]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(f.context(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        f.db.execute("UPDATE game.items SET flag_id=$2 WHERE item_id=$1", &[&f.source, &FLAG_CORP_SAG1]).await.unwrap();
        f.db.execute("UPDATE game.offices SET station_id=$2 WHERE office_id=$1", &[&office, &(f.station + 1)]).await.unwrap();
        let before = f.fingerprint().await;
        assert!(matches!(f.create(f.context(), f.mutator, f.source, false, dispatched.clone()).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint().await, before);
        assert!(!dispatched.load(Ordering::SeqCst));
    }).await;
    f.db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0,corp_roles_hq=0 WHERE character_id=$1", &[&f.owner]).await.unwrap();
    f.db.execute("DELETE FROM game.offices WHERE office_id=$1", &[&office]).await.unwrap();
    f.db.execute("DELETE FROM game.items WHERE item_id=$1", &[&office]).await.unwrap();
    f.db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    f.db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&ceo]).await.unwrap();
    f.cleanup().await;
    outcome.unwrap();
}
