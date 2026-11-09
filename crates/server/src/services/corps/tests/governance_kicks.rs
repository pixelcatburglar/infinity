//! Real issuer revocation and replacement-intent regression.
use crate::{db::Db, services::{tests::{self, fixture}, Call, CallResult}};
use marshal::Value;
mod cancellation;

async fn rpc(client: &mut crate::client::Client, method: &str, args: &[Value], corp: i64) -> CallResult {
    let kwargs = Value::Dict(Vec::new());
    let binding = Value::Int(corp);
    let call = Call { service: "corpRegistry", method, args, kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
    super::super::dispatch(client, &call).await.expect("corporation method routed")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn issuer_revocation_and_stale_work_preserve_replacement_intent() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let director_a = fixture::create(&db).await.unwrap();
    let director_b = fixture::create(&db).await.unwrap();
    let target = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let ids = [ceo.character, director_a.character, director_b.character, target.character];
    let marker = super::fixtures::notification_marker(&db, &ids).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001 WHERE character_id=ANY($2)", &[&corp, &ids.as_slice()]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo.character, &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=ANY($1)", &[&vec![director_a.character, director_b.character], &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
    let old = crate::client::filetime_now();
    db.execute("UPDATE game.characters SET corp_roles=0,corp_roles_removed_at=$2,station_id=60000001 WHERE character_id=$1", &[&target.character, &old]).await.unwrap();
    let (mut a_client, _) = tests::client(db.clone(), director_a.character).await;
    a_client.session.apply(vec![("charid", Value::Int(director_a.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
    rpc(&mut a_client, "KickOutMember", &[Value::Int(target.character)], corp).await.unwrap();
    let first = db.query_one("SELECT issuer_id,requested_at,eligible_at FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
    let first_deadline = first.get::<_, i64>("eligible_at");
    assert_eq!(first.get::<_, i64>("issuer_id"), director_a.character);
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", target.character)]).await.unwrap().is_some());
    let job_id = crate::services::corps::exit::job_id(target.character);
    let now = crate::client::filetime_now();
    db.execute("UPDATE game.jobs SET due_at=$2 WHERE job_id=$1", &[&job_id, &now]).await.unwrap();
    let other_ids: Vec<String> = db.query("SELECT job_id FROM game.jobs WHERE job_id <> $1", &[&job_id]).await.unwrap().into_iter().map(|row| row.get("job_id")).collect();
    let excluded: Vec<&str> = other_ids.iter().map(String::as_str).collect();
    let claim = crate::dao::jobs::claim(&db, now, now + infinity_core::consts::FILETIME_SECOND, 1, &excluded).await.unwrap().into_iter().find(|claim| claim.job.id == job_id).unwrap();
    let (mut ceo_client, _) = tests::client(db.clone(), ceo.character).await;
    ceo_client.session.apply(vec![("charid", Value::Int(ceo.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
    let mut remove = vec![Value::None; 15]; remove[0] = Value::Int(director_a.character); for index in 4..12 { remove[index] = Value::Int(0); }
    rpc(&mut ceo_client, "UpdateMember", &remove, corp).await.unwrap();
    assert!(db.query_opt("SELECT character_id FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap().is_none());
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", target.character)]).await.unwrap().is_none());
    rpc(&mut ceo_client, "KickOutMember", &[Value::Int(target.character)], corp).await.unwrap();
    let replacement = db.query_one("SELECT issuer_id,requested_at,eligible_at FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
    let replacement_stamp = replacement.get::<_, i64>("requested_at");
    assert_eq!(replacement.get::<_, i64>("issuer_id"), ceo.character);
    // A replacement request does not restart the target's removal clock.
    assert_eq!(replacement.get::<_, i64>("eligible_at"), first_deadline);
    let job_before = db.query_one("SELECT payload,due_at,state FROM game.jobs WHERE job_id=$1", &[&job_id]).await.unwrap();
    crate::services::corps::pending_kicks::handle(db.clone(), ceo_client.world.clone(), claim.job).await.unwrap();
    assert_eq!(crate::dao::jobs::ack(&db, &job_id, claim.token).await.unwrap(), 0);
    let job_after = db.query_one("SELECT payload,due_at,state FROM game.jobs WHERE job_id=$1", &[&job_id]).await.unwrap();
    assert_eq!((job_before.get::<_, String>("payload"), job_before.get::<_, i64>("due_at"), job_before.get::<_, String>("state")), (job_after.get::<_, String>("payload"), job_after.get::<_, i64>("due_at"), job_after.get::<_, String>("state")));
    let (mut b_client, _) = tests::client(db.clone(), director_b.character).await;
    b_client.session.apply(vec![("charid", Value::Int(director_b.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
    rpc(&mut b_client, "KickOutMember", &[Value::Int(target.character)], corp).await.unwrap();
    let duplicate = db.query_one("SELECT issuer_id,requested_at,eligible_at FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
    assert_eq!(duplicate.get::<_, i64>("issuer_id"), ceo.character);
    assert_eq!(duplicate.get::<_, i64>("requested_at"), replacement_stamp);
    assert_eq!(duplicate.get::<_, i64>("eligible_at"), replacement.get::<_, i64>("eligible_at"));
    assert_eq!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=92 AND notification_id>$2", &[&target.character, &marker]).await.unwrap().len(), 0);
    assert_eq!(db.query("SELECT corporation_id FROM game.employment_history WHERE char_id=$1 AND corporation_id=$2", &[&target.character, &corp]).await.unwrap().len(), 0);
    }).await;
    db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1) AND notification_id>$2", &[&ids.as_slice(), &marker]).await.unwrap();
    db.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", target.character)]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0,corp_roles_hq=0 WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    for person in [ceo, director_a, director_b, target] { fixture::cleanup(&db, person).await.unwrap(); }
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock; run alone"]
async fn held_hq_role_kick_persists_pending_without_delivery() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let target = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let ids = [ceo.character, target.character];
    db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001 WHERE character_id=ANY($2)", &[&corp, &ids.as_slice()]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$2,corp_roles_hq=0 WHERE character_id=$1", &[&ceo.character, &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles_hq=$2 WHERE character_id=$1", &[&target.character, &infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
    let (mut manager, _) = tests::client(db.clone(), ceo.character).await;
    manager.session.apply(vec![("charid", Value::Int(ceo.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
    let notification_marker = super::fixtures::notification_marker(&db, &[target.character]).await;
    let result = rpc(&mut manager, "KickOutMember", &[Value::Int(target.character)], corp).await.unwrap();
    assert_eq!(marshal::loads(&marshal::dumps(&result)).unwrap(), Value::None);
    let row = db.query_one("SELECT issuer_id,eligible_at FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
    assert_eq!(row.get::<_, i64>("issuer_id"), ceo.character);
    assert_eq!(row.get::<_, Option<i64>>("eligible_at"), None);
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", target.character)]).await.unwrap().is_none());
    assert_eq!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=92 AND notification_id>$2", &[&target.character, &notification_marker]).await.unwrap().len(), 0);
    assert_eq!(db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&target.character]).await.unwrap().get::<_, i64>("corporation_id"), corp);
    let (mut target_client, _) = tests::client(db.clone(), target.character).await;
    target_client.session.apply(vec![("charid", Value::Int(target.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
    let mut clear = vec![Value::None; 15];
    clear[0] = Value::Int(target.character);
    for index in 4..12 { clear[index] = Value::Int(0); }
    clear[13] = Value::Int(0);
    clear[14] = Value::Int(1);
    rpc(&mut target_client, "UpdateMember", &clear, corp).await.unwrap();
    let removed: i64 = db.query_one("SELECT corp_roles_removed_at FROM game.characters WHERE character_id=$1", &[&target.character]).await.unwrap().get("corp_roles_removed_at");
    let deadline: i64 = db.query_one("SELECT eligible_at FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap().get("eligible_at");
    assert_eq!(deadline - removed, crate::config::game().corporations.role_removal_cooldown_secs * infinity_core::consts::FILETIME_SECOND);
    let (mut ceo_client, _) = tests::client(db.clone(), ceo.character).await;
    assert!(matches!(crate::services::corps::exit::admit(&db, Some(&ceo_client.world), target.character, ceo.character, corp, false, deadline - 1).await.unwrap(), crate::services::corps::exit::Admission::WaitUntil(_)));
    assert_eq!(crate::services::corps::exit::admit(&db, Some(&ceo_client.world), target.character, ceo.character, corp, false, deadline).await.unwrap(), crate::services::corps::exit::Admission::Ready);
    rpc(&mut target_client, "UpdateMember", &clear, corp).await.unwrap();
    assert_eq!(db.query_one("SELECT corp_roles_removed_at FROM game.characters WHERE character_id=$1", &[&target.character]).await.unwrap().get::<_, i64>("corp_roles_removed_at"), removed);
    let mut unblock = vec![Value::None; 15]; unblock[0] = Value::Int(target.character); unblock[14] = Value::Int(0);
    rpc(&mut target_client, "UpdateMember", &unblock, corp).await.unwrap();
    let mut grant = vec![Value::None; 15]; grant[0] = Value::Int(target.character); grant[6] = Value::Int(infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1);
    rpc(&mut ceo_client, "UpdateMember", &grant, corp).await.unwrap();
    assert_eq!(db.query_one("SELECT corp_roles_removed_at FROM game.characters WHERE character_id=$1", &[&target.character]).await.unwrap().get::<_, Option<i64>>("corp_roles_removed_at"), None);
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", target.character)]).await.unwrap().is_none());
    rpc(&mut target_client, "UpdateMember", &clear, corp).await.unwrap();
    assert!(db.query_one("SELECT corp_roles_removed_at FROM game.characters WHERE character_id=$1", &[&target.character]).await.unwrap().get::<_, Option<i64>>("corp_roles_removed_at").is_some());
    db.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", target.character)]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0,corp_roles_hq=0 WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, target).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock; run alone"]
async fn all_scoped_direct_and_grantable_rights_defer_real_kick() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let target = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let ids = [ceo.character, target.character];
    db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001 WHERE character_id=ANY($2)", &[&corp, &ids.as_slice()]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo.character, &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
    let (mut manager, _) = tests::client(db.clone(), ceo.character).await;
    let (mut member, _) = tests::client(db.clone(), target.character).await;
    manager.session.apply(vec![("charid", Value::Int(ceo.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
    member.session.apply(vec![("charid", Value::Int(target.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
    let cases = [
        ("direct_global", "corp_roles", crate::services::corps::ROLE_PERSONNEL_MANAGER),
        ("direct_hq", "corp_roles_hq", infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1),
        ("direct_base", "corp_roles_base", infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1),
        ("direct_other", "corp_roles_other", infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1),
        ("grantable_global", "corp_grantable_roles", crate::services::corps::ROLE_PERSONNEL_MANAGER),
        ("grantable_hq", "corp_grantable_hq", infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1),
        ("grantable_base", "corp_grantable_base", infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1),
        ("grantable_other", "corp_grantable_other", infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1),
    ];
    for (name, column, mask) in cases {
        db.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
        db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", target.character)]).await.unwrap();
        let marker = super::fixtures::notification_marker(&db, &[target.character]).await;
        db.execute("UPDATE game.characters SET corp_roles=0,corp_roles_hq=0,corp_roles_base=0,corp_roles_other=0,corp_grantable_roles=0,corp_grantable_hq=0,corp_grantable_base=0,corp_grantable_other=0,corp_title_mask=0,corp_block_roles=false,corp_roles_removed_at=NULL WHERE character_id=$1", &[&target.character]).await.unwrap();
        db.execute(&format!("UPDATE game.characters SET {column}=$2 WHERE character_id=$1"), &[&target.character, &mask]).await.unwrap();
        let can_leave = rpc(&mut member, "CanLeaveCurrentCorporation", &[], corp).await.unwrap();
        let wire = marshal::loads(&marshal::dumps(&can_leave)).unwrap();
        let fields = wire.items().expect("CanLeave tuple");
        assert_eq!(fields.first(), Some(&Value::Bool(false)), "{name}");
        assert_eq!(fields.get(1), Some(&Value::str("CrpCantQuitNotInStasis")), "{name}");
        let kicked = rpc(&mut manager, "KickOutMember", &[Value::Int(target.character)], corp).await.unwrap();
        assert_eq!(marshal::loads(&marshal::dumps(&kicked)).unwrap(), Value::None, "{name}");
        let pending = db.query_one("SELECT eligible_at FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
        assert_eq!(pending.get::<_, Option<i64>>("eligible_at"), None, "{name}");
        assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", target.character)]).await.unwrap().is_none(), "{name}");
        assert_eq!(db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&target.character]).await.unwrap().get::<_, i64>("corporation_id"), corp, "{name}");
        assert_eq!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=92 AND notification_id>$2", &[&target.character, &marker]).await.unwrap().len(), 0, "{name}");
    }
    db.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", target.character)]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=0,corp_roles_hq=0,corp_roles_base=0,corp_roles_other=0,corp_grantable_roles=0,corp_grantable_hq=0,corp_grantable_base=0,corp_grantable_other=0,corp_title_mask=1 WHERE character_id=$1", &[&target.character]).await.unwrap();
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id=$1 AND title_id=1", &[&corp]).await.unwrap();
    db.execute("INSERT INTO game.corp_titles(corporation_id,title_id,name,roles_hq) VALUES ($1,1,'held-title',$2)", &[&corp, &infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
    let title_leave = rpc(&mut member, "CanLeaveCurrentCorporation", &[], corp).await.unwrap();
    assert_eq!(marshal::loads(&marshal::dumps(&title_leave)).unwrap().items().unwrap().first(), Some(&Value::Bool(false)));
    let title_kick = rpc(&mut manager, "KickOutMember", &[Value::Int(target.character)], corp).await.unwrap();
    assert_eq!(marshal::loads(&marshal::dumps(&title_kick)).unwrap(), Value::None);
    assert_eq!(db.query_one("SELECT eligible_at FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap().get::<_, Option<i64>>("eligible_at"), None);
    db.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", target.character)]).await.unwrap();
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, target).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and a fresh isolated test process"]
async fn nondefault_cooldown_drives_real_admission_and_restart() {
    use std::{fs, path::PathBuf};
    let path: PathBuf = std::env::temp_dir().join(format!("infinity-corp-config-{}", std::process::id()));
    fs::write(&path, "[corporations]\nrole_removal_cooldown_secs = 90\n").unwrap();
    let config = crate::config::Config::load_with(&path, |_| None).unwrap();
    config.validate().unwrap();
    crate::config::init_game(config.game_config()).unwrap();
    fs::remove_file(&path).ok();
    let database = crate::config::deployment::DatabaseConfig {
        url: std::env::var("INFINITY_TEST_DATABASE_URL").expect("INFINITY_TEST_DATABASE_URL must name a dedicated test database"),
        ..Default::default()
    };
    let db = Db::connect(&database).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let target = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let ids = [ceo.character, target.character];
    db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001 WHERE character_id=ANY($2)", &[&corp, &ids.as_slice()]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$2,corp_roles_hq=0 WHERE character_id=$1", &[&ceo.character, &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles_hq=$2 WHERE character_id=$1", &[&target.character, &infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
    let (mut manager, _) = tests::client(db.clone(), ceo.character).await;
    manager.session.apply(vec![("charid", Value::Int(ceo.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
    rpc(&mut manager, "KickOutMember", &[Value::Int(target.character)], corp).await.unwrap();
    let (mut target_client, _) = tests::client(db.clone(), target.character).await;
    target_client.session.apply(vec![("charid", Value::Int(target.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
    let mut clear = vec![Value::None; 15]; clear[0] = Value::Int(target.character); for index in 4..12 { clear[index] = Value::Int(0); } clear[13] = Value::Int(0); clear[14] = Value::Int(1);
    rpc(&mut target_client, "UpdateMember", &clear, corp).await.unwrap();
    let removed: i64 = db.query_one("SELECT corp_roles_removed_at FROM game.characters WHERE character_id=$1", &[&target.character]).await.unwrap().get("corp_roles_removed_at");
    let deadline: i64 = db.query_one("SELECT eligible_at FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap().get("eligible_at");
    assert_eq!(deadline - removed, 90 * infinity_core::consts::FILETIME_SECOND);
    let (ceo_client, _) = tests::client(db.clone(), ceo.character).await;
    assert!(matches!(crate::services::corps::exit::admit(&db, Some(&ceo_client.world), target.character, ceo.character, corp, false, deadline - 1).await.unwrap(), crate::services::corps::exit::Admission::WaitUntil(_)));
    assert_eq!(crate::services::corps::exit::admit(&db, Some(&ceo_client.world), target.character, ceo.character, corp, false, deadline).await.unwrap(), crate::services::corps::exit::Admission::Ready);
    let original_requested: i64 = db.query_one("SELECT requested_at FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap().get("requested_at");
    let stale = removed + 86400 * infinity_core::consts::FILETIME_SECOND;
    db.execute("UPDATE game.corp_pending_kicks SET eligible_at=$3 WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character, &stale]).await.unwrap();
    db.execute("UPDATE game.jobs SET due_at=$2 WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", target.character), &stale]).await.unwrap();
    crate::services::corps::initialize(&db, &ceo_client.sde).await.unwrap();
    let recovered: i64 = db.query_one("SELECT eligible_at FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap().get("eligible_at");
    assert_eq!(recovered, deadline);
    let restored = db.query_one("SELECT issuer_id,requested_at FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
    assert_eq!(restored.get::<_, i64>("issuer_id"), ceo.character);
    assert_eq!(restored.get::<_, i64>("requested_at"), original_requested);
    db.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", target.character)]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0,corp_roles_hq=0 WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, target).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock; run alone"]
async fn due_space_job_waits_for_current_generation_logoff_once() {
    use infinity_core::session::OnlineSession;
    use crate::{chat::Hub, services::{identity::Identity, presence}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let target = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let ids = [ceo.character, target.character];
    db.execute("UPDATE game.characters SET corporation_id=$1,station_id=NULL WHERE character_id=$2", &[&corp, &target.character]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001,corp_roles=$2 WHERE character_id=$3", &[&corp, &infinity_core::consts::ROLE_DIRECTOR, &ceo.character]).await.unwrap();
    let (mut manager, _) = tests::client(db.clone(), ceo.character).await;
    manager.session.apply(vec![("charid", Value::Int(ceo.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
    let sde = manager.sde.clone();
    let world = manager.world.clone();
    let hub = Hub::new("chat.test", db.clone(), world.clone());
    let system = sde.map.systems().find(|system| system.solar_system_id > 0).unwrap().solar_system_id;
    let identity = Identity { char_id: target.character, corp_id: corp, ship_id: None, station_id: None, system_id: Some(system) };
    let (notify_tx, _notify_rx) = tokio::sync::mpsc::unbounded_channel();
    let session = OnlineSession { character_id: target.character, user_id: Some(target.account), session_id: 7, ship_id: None, station_id: None, system_id: Some(system), corporation_id: Some(corp), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 };
    presence::online(&identity, &world, &hub, notify_tx, None, session);
    let marker = super::fixtures::notification_marker(&db, &ids).await;
    let kick = rpc(&mut manager, "KickOutMember", &[Value::Int(target.character)], corp).await.unwrap();
    assert_eq!(marshal::loads(&marshal::dumps(&kick)).unwrap(), Value::None);
    let job_id = format!("corporations.pending_kick:{}", target.character);
    let now = crate::client::filetime_now();
    db.execute("UPDATE game.jobs SET due_at=$2 WHERE job_id=$1", &[&job_id, &now]).await.unwrap();
    let other_ids: Vec<String> = db.query("SELECT job_id FROM game.jobs WHERE job_id <> $1", &[&job_id]).await.unwrap().into_iter().map(|row| row.get("job_id")).collect();
    let excluded: Vec<&str> = other_ids.iter().map(String::as_str).collect();
    let claim = crate::dao::jobs::claim(&db, now, now + infinity_core::consts::FILETIME_SECOND, 1, &excluded).await.unwrap().into_iter().find(|claim| claim.job.id == job_id).unwrap();
    let old_job = claim.job.clone();
    crate::services::corps::pending_kicks::handle(db.clone(), world.clone(), claim.job).await.unwrap();
    assert_eq!(crate::dao::jobs::ack(&db, &job_id, claim.token).await.unwrap(), 1);
    assert!(db.query_opt("SELECT character_id FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap().is_some());
    presence::leave(&identity, Some(target.account), &db, &world, &hub, &sde, Some(system), Some(8), || {}).await.unwrap();
    assert!(world.online_session(target.character).is_some());
    let before_events: i64 = db.query_one("SELECT count(*) FROM game.corp_member_events WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap().get("count");
    presence::leave(&identity, Some(target.account), &db, &world, &hub, &sde, Some(system), Some(7), || {}).await.unwrap();
    assert_ne!(db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&target.character]).await.unwrap().get::<_, i64>("corporation_id"), corp);
    assert!(world.online_session(target.character).is_none());
    assert!(db.query_opt("SELECT character_id FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap().is_none());
    assert_eq!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=92 AND notification_id>$2", &[&target.character, &marker]).await.unwrap().len(), 1);
    let after_events: i64 = db.query_one("SELECT count(*) FROM game.corp_member_events WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap().get("count");
    assert_eq!(after_events, before_events + 1);
    crate::services::corps::pending_kicks::handle(db.clone(), world.clone(), old_job).await.unwrap();
    assert_eq!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=92 AND notification_id>$2", &[&target.character, &marker]).await.unwrap().len(), 1);
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&job_id]).await.unwrap();
    db.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &target.character]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1) AND notification_id>$2", &[&ids.as_slice(), &marker]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, target).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn public_pending_autokicks_lists_live_authorized_intents() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let director = fixture::create(&db).await.unwrap();
    let held = fixture::create(&db).await.unwrap();
    let cooling = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let ids = [ceo.character, director.character, held.character, cooling.character, outsider.character];
    let marker = super::fixtures::notification_marker(&db, &ids).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001 WHERE character_id=ANY($2)", &[&corp, &ids.as_slice()]).await.unwrap();
        db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=ANY($1)", &[&vec![ceo.character,director.character], &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
        db.execute("UPDATE game.characters SET corp_roles_hq=$2 WHERE character_id=$1", &[&held.character, &infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
        db.execute("UPDATE game.characters SET corp_roles_removed_at=$2 WHERE character_id=$1", &[&cooling.character, &crate::client::filetime_now()]).await.unwrap();
        let (mut manager, _) = tests::client(db.clone(), director.character).await;
        manager.session.apply(vec![("corpid", Value::Int(corp)), ("corprole", Value::Int(infinity_core::consts::ROLE_DIRECTOR))]);
        rpc(&mut manager, "KickOutMember", &[Value::Int(held.character)], corp).await.unwrap();
        rpc(&mut manager, "KickOutMember", &[Value::Int(cooling.character)], corp).await.unwrap();
        let kwargs = Value::Dict(Vec::new());
        let binding = Value::Int(corp);
        let call = Call { service: "corpRegistry", method: "GetPendingAutoKicks", args: &[], kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
        let actual = crate::services::dispatch(&mut manager, &call).await.expect("pending list routed");
        let mut targets = [held.character, cooling.character]; targets.sort_unstable();
        let expected = Value::list(targets.into_iter().map(|id| Value::tuple([Value::Int(id),Value::Int(director.character)])));
        assert_eq!(marshal::loads(&marshal::dumps(&actual)).unwrap(), expected);
        let (mut unprivileged, _) = tests::client(db.clone(), outsider.character).await;
        unprivileged.session.apply(vec![("corpid", Value::Int(corp))]);
        assert!(crate::services::dispatch(&mut unprivileged, &call).await.is_err());
        let foreign = Value::Int(1);
        let foreign_call = Call { bind_params: Some(&foreign), ..call };
        assert!(crate::services::dispatch(&mut manager, &foreign_call).await.is_err());
        let unchanged = crate::services::dispatch(&mut manager, &call).await.unwrap();
        assert_eq!(marshal::loads(&marshal::dumps(&unchanged)).unwrap(), expected);
        let (mut chief, _) = tests::client(db.clone(), ceo.character).await;
        chief.session.apply(vec![("corpid", Value::Int(corp))]);
        let mut clear = vec![Value::None; 15]; clear[0] = Value::Int(director.character); for value in &mut clear[4..12] { *value = Value::Int(0); }
        rpc(&mut chief, "UpdateMember", &clear, corp).await.unwrap();
        assert!(crate::services::dispatch(&mut manager, &call).await.is_err());
        let list = crate::services::dispatch(&mut chief, &call).await.unwrap();
        assert_eq!(marshal::loads(&marshal::dumps(&list)).unwrap(), Value::list([]));
        db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&ceo.character]).await.unwrap();
        assert!(crate::services::dispatch(&mut chief, &call).await.is_err());
        assert_eq!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=ANY($1) AND type_id=92 AND notification_id>$2", &[&ids.as_slice(), &marker]).await.unwrap().len(), 0);
    }).await;
    for target in [held.character, cooling.character] {
        db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{target}")]).await.unwrap();
    }
    db.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_role_history WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corp_member_events WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1) AND notification_id>$2", &[&ids.as_slice(), &marker]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0,corp_roles_hq=0 WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    for person in [ceo, director, held, cooling, outsider] { fixture::cleanup(&db, person).await.unwrap(); }
    result.unwrap();
}
