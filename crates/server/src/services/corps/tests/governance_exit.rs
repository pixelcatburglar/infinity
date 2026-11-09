use crate::{client::Client, db::Db, services::{Call, CallResult, tests::fixture}};
use marshal::{self, Value};

async fn rpc(client: &mut Client, method: &str, args: &[Value], bind: i64) -> CallResult {
    let kwargs = Value::Dict(Vec::new());
    let binding = Value::Int(bind);
    let call = Call { service: "corpRegistry", method, args, kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
    super::super::dispatch(client, &call).await.expect("corporation method routed")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn roleless_member_can_leave_via_real_rpc() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 1).await.unwrap();
    fixture::assign_corporation(&db, ceo.character, corp, infinity_core::consts::ROLE_DIRECTOR).await.unwrap();
    db.execute("UPDATE game.corporations SET member_limit=10 WHERE corporation_id=$1", &[&corp]).await.unwrap();

    // Use the production application acceptance RPC to admit the roleless member.
    let app: i64 = db.query_one(
        "INSERT INTO game.corp_applications (corporation_id,character_id,application_text,status,created) VALUES ($1,$2,'申请',6,123) RETURNING application_id",
        &[&corp, &member.character],
    ).await.unwrap().get("application_id");
    let mut applicant = crate::services::tests::client(db.clone(), member.character).await.0;
    applicant.session.apply(vec![("charid", Value::Int(member.character)), ("corpid", Value::Int(1))]);
    let accept = [Value::Int(app), Value::Int(member.character), Value::Int(corp), Value::unicode("申请"), Value::Int(2), Value::unicode(""), Value::None];
    rpc(&mut applicant, "UpdateApplicationOffer", &accept, 1).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=1 WHERE character_id=$1", &[&member.character]).await.unwrap();

    let mut member_client = crate::services::tests::client(db.clone(), member.character).await.0;
    member_client.session.apply(vec![("charid", Value::Int(member.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(1))]);
    let can_leave = rpc(&mut member_client, "CanLeaveCurrentCorporation", &[], corp).await.unwrap();
    let wire = marshal::loads(&marshal::dumps(&can_leave)).unwrap();
    let fields = wire.items().expect("CanLeave returns a tuple");
    assert_eq!(fields.first(), Some(&Value::Bool(true)));

    let before_events: i64 = db.query_one("SELECT count(*) FROM game.corp_member_events WHERE corporation_id=$1 AND character_id=$2", &[&corp, &member.character]).await.unwrap().get("count");
    let before_jobs: i64 = db.query_one("SELECT count(*) FROM game.employment_history WHERE char_id=$1", &[&member.character]).await.unwrap().get("count");
    let before_notes: i64 = db.query_one("SELECT count(*) FROM game.notifications WHERE receiver_id=$1", &[&member.character]).await.unwrap().get("count");
    let marker = super::fixtures::notification_marker(&db, &[member.character]).await;
    let mut manager = crate::services::tests::client(db.clone(), ceo.character).await.0;
    manager.session.apply(vec![("charid", Value::Int(ceo.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(1))]);
    let kicked = rpc(&mut manager, "KickOutMember", &[Value::Int(member.character)], corp).await.unwrap();
    assert_eq!(marshal::loads(&marshal::dumps(&kicked)).unwrap(), Value::None);

    let current: i64 = db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&member.character]).await.unwrap().get("corporation_id");
    assert_ne!(current, corp);
    let pending: i64 = db.query_one("SELECT count(*) FROM game.corp_pending_kicks WHERE character_id=$1", &[&member.character]).await.unwrap().get("count");
    assert_eq!(pending, 0);
    let employment: i64 = db.query_one("SELECT count(*) FROM game.employment_history WHERE char_id=$1", &[&member.character]).await.unwrap().get("count");
    assert!(employment >= 1);
    let after_events: i64 = db.query_one("SELECT count(*) FROM game.corp_member_events WHERE corporation_id=$1 AND character_id=$2", &[&corp, &member.character]).await.unwrap().get("count");
    let after_jobs: i64 = db.query_one("SELECT count(*) FROM game.employment_history WHERE char_id=$1", &[&member.character]).await.unwrap().get("count");
    let after_notes: i64 = db.query_one("SELECT count(*) FROM game.notifications WHERE receiver_id=$1", &[&member.character]).await.unwrap().get("count");
    assert_eq!(after_events, before_events + 1);
    assert_eq!(after_jobs, before_jobs + 1);
    assert!(after_notes > before_notes);
    let kicked_notice = db.query_one("SELECT type_id,sender_id,data FROM game.notifications WHERE receiver_id=$1 AND type_id=92 AND notification_id>$2 ORDER BY notification_id DESC LIMIT 1", &[&member.character, &marker]).await.unwrap();
    assert_eq!(kicked_notice.get::<_, i32>("type_id"), 92);
    assert_eq!(kicked_notice.get::<_, i64>("sender_id"), corp);
    assert!(kicked_notice.get::<_, String>("data").contains(&corp.to_string()));

    db.execute("DELETE FROM game.corp_applications WHERE corporation_id=$1 OR character_id=$2", &[&corp, &member.character]).await.unwrap();
    fixture::cleanup_corporation(&db, corp, &[ceo.character, member.character], 0).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, member).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn bulk_kick_separates_offline_removed_pending_and_ceo_refusal() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let victim = fixture::create(&db).await.unwrap();
    let held = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let ids = [ceo.character, actor.character, victim.character, held.character];
    let marker = super::fixtures::notification_marker(&db, &ids).await;
    db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001 WHERE character_id=ANY($2)", &[&corp, &ids.as_slice()]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&actor.character, &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=0,corp_roles_hq=0,station_id=NULL WHERE character_id=$1", &[&victim.character]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=0,corp_roles_hq=$2 WHERE character_id=$1", &[&held.character, &infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=0 WHERE character_id=$1", &[&ceo.character]).await.unwrap();
    let (mut manager, _) = crate::services::tests::client(db.clone(), actor.character).await;
    manager.session.apply(vec![("charid", Value::Int(actor.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
    let invalid = rpc(&mut manager, "KickOutMembers", &[Value::list([Value::Int(victim.character), Value::Int(actor.character)])], corp).await;
    assert!(invalid.is_err());
    assert_eq!(db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&victim.character]).await.unwrap().get::<_, i64>("corporation_id"), corp);
    assert_eq!(db.query("SELECT event_id FROM game.corp_member_events WHERE corporation_id=$1 AND character_id=$2", &[&corp, &victim.character]).await.unwrap().len(), 0);
    assert_eq!(db.query("SELECT character_id FROM game.corp_pending_kicks WHERE corporation_id=$1", &[&corp]).await.unwrap().len(), 0);
    let bulk = rpc(&mut manager, "KickOutMembers", &[Value::list([Value::Int(victim.character), Value::Int(victim.character), Value::Int(held.character), Value::Int(ceo.character)])], corp).await.unwrap();
    let wire = marshal::loads(&marshal::dumps(&bulk)).unwrap();
    let Value::Dict(entries) = wire else { panic!("expected bulk dict") };
    let field = |name: &str| entries.iter().find(|(key, _)| key.as_text().as_deref() == Some(name)).map(|(_, value)| value.items().unwrap()).unwrap();
    let kicked = field("kicked");
    let refused = field("notKicked");
    assert_eq!(kicked, &[Value::Int(victim.character)]);
    assert_eq!(refused, &[Value::Int(ceo.character), Value::Int(held.character)]);
    assert_ne!(db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&victim.character]).await.unwrap().get::<_, i64>("corporation_id"), corp);
    assert_eq!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=92 AND notification_id>$2", &[&victim.character, &marker]).await.unwrap().len(), 1);
    assert_eq!(db.query("SELECT character_id FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2 AND eligible_at IS NULL", &[&corp, &held.character]).await.unwrap().len(), 1);
    assert_eq!(db.query("SELECT character_id FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &ceo.character]).await.unwrap().len(), 0);
    db.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp, &held.character]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1) AND notification_id>$2", &[&ids.as_slice(), &marker]).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", held.character)]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0,corp_roles_hq=0,station_id=60000001 WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    for person in [ceo, actor, victim, held] { fixture::cleanup(&db, person).await.unwrap(); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn bulk_self_inclusion_refuses_without_changes() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    // Victim sorts before the caller, so a per-target guard would mutate it first.
    let victim = fixture::create(&db).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let ids = [victim.character, ceo.character];
    let test_db = db.clone();
    let marker = super::fixtures::notification_marker(&db, &ids).await;
    let result = tokio::spawn(async move {
        let db = test_db;
        db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001,corp_roles=0,corp_roles_hq=0,corp_roles_base=0,corp_roles_other=0,corp_roles_removed_at=NULL WHERE character_id=ANY($2)", &[&corp, &ids.as_slice()]).await.unwrap();
        db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo.character, &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
        let (mut client, _peer) = crate::services::tests::client(db.clone(), ceo.character).await;
        client.session.apply(vec![("charid", Value::Int(ceo.character)), ("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(ceo.character, sender);
        let jobs = [format!("corporations.pending_kick:{}", victim.character), format!("corporations.pending_kick:{}", ceo.character)];
        let snapshot = |db: Db, ids: [i64; 2], jobs: [String; 2]| async move {
            db.query_one("SELECT \
                (SELECT COALESCE(string_agg(row_to_json(x)::text,'' ORDER BY row_to_json(x)::text),'') FROM (SELECT * FROM game.characters WHERE character_id=ANY($1)) x) || \
                (SELECT COALESCE(string_agg(row_to_json(x)::text,'' ORDER BY row_to_json(x)::text),'') FROM (SELECT * FROM game.corporations WHERE corporation_id=$2) x) || \
                (SELECT COALESCE(string_agg(row_to_json(x)::text,'' ORDER BY row_to_json(x)::text),'') FROM (SELECT * FROM game.corp_pending_kicks WHERE corporation_id=$2 OR character_id=ANY($1)) x) || \
                (SELECT COALESCE(string_agg(row_to_json(x)::text,'' ORDER BY row_to_json(x)::text),'') FROM (SELECT * FROM game.jobs WHERE job_id=ANY($3)) x) || \
                (SELECT COALESCE(string_agg(row_to_json(x)::text,'' ORDER BY row_to_json(x)::text),'') FROM (SELECT * FROM game.corp_member_events WHERE corporation_id=$2 OR character_id=ANY($1)) x) || \
                (SELECT COALESCE(string_agg(row_to_json(x)::text,'' ORDER BY row_to_json(x)::text),'') FROM (SELECT * FROM game.corp_role_history WHERE corporation_id=$2 OR character_id=ANY($1)) x) || \
                (SELECT COALESCE(string_agg(row_to_json(x)::text,'' ORDER BY row_to_json(x)::text),'') FROM (SELECT * FROM game.employment_history WHERE char_id=ANY($1)) x) || \
                (SELECT COALESCE(string_agg(row_to_json(x)::text,'' ORDER BY row_to_json(x)::text),'') FROM (SELECT * FROM game.notifications WHERE receiver_id=ANY($1)) x) AS state",
                &[&ids.as_slice(), &corp, &jobs.as_slice()]).await.unwrap().get::<_, String>("state")
        };
        let before = snapshot(db.clone(), ids, jobs.clone()).await;
        let args = [Value::list([Value::Int(victim.character), Value::Int(ceo.character)])];
        let kwargs = Value::Dict(Vec::new());
        let binding = Value::Int(corp);
        let call = Call { service: "corpRegistry", method: "KickOutMembers", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
        let error = crate::services::dispatch(&mut client, &call).await.unwrap_err();
        let crate::services::CallError::User { msg, dict } = error else { panic!("expected native self-inclusion refusal") };
        assert_eq!(msg, infinity_core::messages::corporations::CANNOT_KICK_SELF_IN_MULTIKICK);
        assert!(dict.is_empty());
        assert_eq!(snapshot(db.clone(), ids, jobs).await, before);
        assert!(matches!(receiver.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
        client.world.unregister_pilot_generation(ceo.character, None);
    }).await;
    db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1) AND notification_id>$2", &[&ids.as_slice(), &marker]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0 WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, victim).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    result.unwrap();
}
