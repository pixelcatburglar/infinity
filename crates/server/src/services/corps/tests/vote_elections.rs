//! Election eligibility, role projection, and failed-election restoration.
use super::fixtures::{cleanup_votes, typed_vote_options, vote_call};
use crate::{dao::corporations::{sanctions, shares, votes}, db::Db, scheduler::Job,
    services::{corps::{permissions, vote_close, vote_request}, tests::{self, fixture}}};
use infinity_core::consts::{ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_HANGAR_CAN_TAKE_1};
use marshal::Value;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn election_threshold_freezes_director_and_failure_restores_stored_roles() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let candidate = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let proxy = fixture::create_corporation(&db, candidate.character, 60000001).await.unwrap();
    let result = crate::tests::catch_scenario(async {
    fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR | ROLE_ACCOUNTANT | ROLE_HANGAR_CAN_TAKE_1).await.unwrap();
    fixture::assign_corporation(&db, candidate.character, corp, 0).await.unwrap();
    db.execute("INSERT INTO game.corp_titles(corporation_id,title_id,name,roles) VALUES ($1,1,'director',$2)", &[&corp, &ROLE_DIRECTOR]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_title_mask=1,corp_grantable_roles=$2 WHERE character_id=$1", &[&ceo.character, &ROLE_ACCOUNTANT]).await.unwrap();
    let skill = crate::services::corps::SKILL_CORPORATION_MANAGEMENT;
    let level = i32::try_from(crate::config::game().corporations.min_management_skill_level).unwrap().max(1);
    db.execute("INSERT INTO game.skills(character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0)", &[&candidate.character, &skill, &level]).await.unwrap();
    assert!(shares::initialize(&db, corp, 1000).await.unwrap());
    assert!(shares::transfer(&db, corp, corp, candidate.character, 49).await.unwrap());
    let (mut nominee, _peer) = tests::client(db.clone(), candidate.character).await;
    let station = nominee.sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).expect("station with complete location");
    db.execute("UPDATE game.characters SET station_id=$2,solar_system_id=$3,constellation_id=$4,region_id=$5 WHERE character_id=ANY($1)",
        &[&[ceo.character, candidate.character].as_slice(), &station.station_id, &station.solar_system_id, &station.constellation_id, &station.region_id]).await.unwrap();
    nominee.session.apply(vec![("corpid", Value::Int(corp))]);
    let (mut chief, _chief_peer) = tests::client_with_sde(db.clone(), ceo.character, nominee.sde.clone()).await;
    chief.session.apply(vec![("corpid", Value::Int(corp))]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    nominee.world.register_pilot(ceo.character, tx);
    let marker = super::fixtures::notification_marker(&db, &[ceo.character, candidate.character]).await;
    let options = typed_vote_options([
        "UI/Corporations/CorporationWindow/Politics/SomeoneForCEO",
        "UI/Corporations/CorporationWindow/Politics/DontChangeCEO",
    ], [Some(candidate.character), None, None]);
    let args = [Value::unicode("Election"), Value::unicode(""), Value::Int(vote_request::VOTE_CEO), options, Value::Int(1)];
    assert!(shares::transfer(&db, corp, corp, proxy, 50).await.unwrap());
    assert!(vote_call(&mut nominee, "InsertVoteCase", &args).await.is_err());
    assert!(shares::transfer(&db, corp, proxy, corp, 50).await.unwrap());
    assert!(vote_call(&mut nominee, "InsertVoteCase", &args).await.is_err());
    assert!(shares::transfer(&db, corp, corp, candidate.character, 1).await.unwrap());
    db.execute("UPDATE game.characters SET corp_roles=0,corp_title_mask=0 WHERE character_id=$1", &[&ceo.character]).await.unwrap();
    assert_eq!(vote_call(&mut chief, "CanViewVotes", &[Value::Int(corp)]).await.unwrap(), Value::Bool(false));
    let Value::Int(inactive) = vote_call(&mut nominee, "InsertVoteCase", &args).await.unwrap() else { panic!("case ID") };
    assert_eq!(vote_call(&mut chief, "CanViewVotes", &[Value::Int(corp)]).await.unwrap(), Value::Bool(false));
    assert!(vote_call(&mut chief, "InsertVote", &[Value::Int(corp), Value::Int(inactive), Value::Int(0)]).await.is_err());
    let end = crate::client::filetime_now() - 1;
    db.execute("UPDATE game.corp_vote_cases SET ends=$2 WHERE id=$1", &[&inactive, &end]).await.unwrap();
    let job = Job { id: format!("{}:{inactive}", vote_close::KIND), kind: vote_close::KIND.into(), due_at: end, payload: inactive.to_string() };
    vote_close::handle(db.clone(), nominee.world.clone(), job).await.unwrap();
    assert!(votes::election(&db, corp).await.unwrap().is_none());
    assert!(sanctions::get(&db, inactive).await.unwrap().is_none());
    assert!(!permissions::allows(&permissions::load(&db, ceo.character).await.unwrap().unwrap(), corp, ROLE_DIRECTOR));
    db.execute("UPDATE game.characters SET corp_roles=$2,corp_title_mask=1 WHERE character_id=$1", &[&ceo.character, &(ROLE_DIRECTOR | ROLE_ACCOUNTANT | ROLE_HANGAR_CAN_TAKE_1)]).await.unwrap();
    let Value::Int(case) = vote_call(&mut nominee, "InsertVoteCase", &args).await.unwrap() else { panic!("case ID") };
    let authority = permissions::load(&db, ceo.character).await.unwrap().unwrap();
    assert!(!permissions::allows(&authority, corp, ROLE_DIRECTOR));
    assert!(permissions::allows(&authority, corp, ROLE_ACCOUNTANT));
    assert!(permissions::allows_at(&authority, corp, 60000001, ROLE_HANGAR_CAN_TAKE_1));
    let selected = crate::dao::characters::selection::selected(&db, ceo.character, ceo.account).await.unwrap().unwrap();
    assert_eq!(selected.authority.roles, authority.roles);
    assert_eq!(selected.authority.grantable, authority.grantable);
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(events.iter().any(|(name, args)| name == "__session" && args.contains(&Value::tuple([Value::str("corprole"), Value::Int(authority.roles[0])]))));
    assert!(!events.iter().any(|(name, _)| name == "OnCorporationChanged"));
    assert_eq!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=26 AND notification_id>$2", &[&ceo.character, &marker]).await.unwrap().len(), 1);
    let row = db.query_one("SELECT corp_roles,corp_title_mask,corp_grantable_roles FROM game.characters WHERE character_id=$1", &[&ceo.character]).await.unwrap();
    assert_ne!(row.get::<_, i64>("corp_roles") & ROLE_DIRECTOR, 0);
    assert_eq!(row.get::<_, i64>("corp_title_mask"), 1);
    assert_eq!(row.get::<_, i64>("corp_grantable_roles"), ROLE_ACCOUNTANT);
    assert_eq!(vote_call(&mut chief, "CanViewVotes", &[Value::Int(corp)]).await.unwrap(), Value::Bool(true));
    vote_call(&mut chief, "InsertVote", &[Value::Int(corp), Value::Int(case), Value::Int(0)]).await.unwrap();
    assert_eq!(votes::ballot(&db, case, ceo.character).await.unwrap().unwrap().weight, 0);
    let end = crate::client::filetime_now() - 1;
    db.execute("UPDATE game.corp_vote_cases SET ends=$2 WHERE id=$1", &[&case, &end]).await.unwrap();
    let job = Job { id: format!("{}:{case}", vote_close::KIND), kind: vote_close::KIND.into(), due_at: end, payload: case.to_string() };
    vote_close::handle(db.clone(), nominee.world.clone(), job).await.unwrap();
    assert!(votes::election(&db, corp).await.unwrap().is_none());
    assert!(sanctions::get(&db, case).await.unwrap().is_none());
    assert!(permissions::allows(&permissions::load(&db, ceo.character).await.unwrap().unwrap(), corp, ROLE_DIRECTOR));
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let restored = permissions::load(&db, ceo.character).await.unwrap().unwrap();
    assert!(events.iter().any(|(name, args)| name == "__session" && args.contains(&Value::tuple([Value::str("corprole"), Value::Int(restored.roles[0])]))));
    assert!(!events.iter().any(|(name, _)| name == "OnCorporationChanged"));
    assert!(shares::transfer(&db, corp, corp, ceo.character, 50).await.unwrap());
    for tie in [false, true] {
        let Value::Int(failed) = vote_call(&mut nominee, "InsertVoteCase", &args).await.unwrap() else { panic!("case ID") };
        if tie {
            vote_call(&mut nominee, "InsertVote", &[Value::Int(corp), Value::Int(failed), Value::Int(0)]).await.unwrap();
        }
        vote_call(&mut chief, "InsertVote", &[Value::Int(corp), Value::Int(failed), Value::Int(1)]).await.unwrap();
        let end = crate::client::filetime_now() - 1;
        db.execute("UPDATE game.corp_vote_cases SET ends=$2 WHERE id=$1", &[&failed, &end]).await.unwrap();
        let job = Job { id: format!("{}:{failed}", vote_close::KIND), kind: vote_close::KIND.into(), due_at: end, payload: failed.to_string() };
        vote_close::handle(db.clone(), nominee.world.clone(), job).await.unwrap();
        assert!(votes::election(&db, corp).await.unwrap().is_none());
        assert!(sanctions::get(&db, failed).await.unwrap().is_none());
        assert!(permissions::allows(&permissions::load(&db, ceo.character).await.unwrap().unwrap(), corp, ROLE_DIRECTOR));
    }
    let Value::Int(departed) = vote_call(&mut nominee, "InsertVoteCase", &args).await.unwrap() else { panic!("case ID") };
    vote_call(&mut nominee, "InsertVote", &[Value::Int(corp), Value::Int(departed), Value::Int(0)]).await.unwrap();
    assert_eq!(super::super::exit::execute(&db, &nominee.sde, &nominee.world, corp, candidate.character, candidate.character, None).await.unwrap(), super::super::exit::Admission::Ready);
    let end = crate::client::filetime_now() - 1;
    db.execute("UPDATE game.corp_vote_cases SET ends=$2 WHERE id=$1", &[&departed, &end]).await.unwrap();
    let job = Job { id: format!("{}:{departed}", vote_close::KIND), kind: vote_close::KIND.into(), due_at: end, payload: departed.to_string() };
    vote_close::handle(db.clone(), nominee.world.clone(), job).await.unwrap();
    assert!(votes::election(&db, corp).await.unwrap().is_none());
    assert!(sanctions::get(&db, departed).await.unwrap().is_none());
    assert!(permissions::allows(&permissions::load(&db, ceo.character).await.unwrap().unwrap(), corp, ROLE_DIRECTOR));
    assert_ne!(db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&candidate.character]).await.unwrap().get::<_, i64>("corporation_id"), corp);
    fixture::assign_corporation(&db, candidate.character, corp, 0).await.unwrap();
    assert!(shares::transfer(&db, corp, ceo.character, corp, 50).await.unwrap());
    // Recovery after the manual-action window still honors the original snapshot.
    let Value::Int(success) = vote_call(&mut nominee, "InsertVoteCase", &args).await.unwrap() else { panic!("case ID") };
    assert!(shares::transfer(&db, corp, candidate.character, corp, 50).await.unwrap());
    vote_call(&mut nominee, "InsertVote", &[Value::Int(corp), Value::Int(success), Value::Int(0)]).await.unwrap();
    assert_eq!(votes::ballot(&db, success, candidate.character).await.unwrap().unwrap().weight, 50);
    let old_end = crate::client::filetime_now() - 30 * infinity_core::consts::FILETIME_DAY;
    db.execute("UPDATE game.corp_vote_cases SET ends=$2 WHERE id=$1", &[&success, &old_end]).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{success}", vote_close::KIND)]).await.unwrap();
    vote_close::seed(&db).await.unwrap();
    while rx.try_recv().is_ok() {}
    let job = crate::dao::jobs::lock_job(&db, &format!("{}:{success}", vote_close::KIND)).await.unwrap().unwrap();
    assert_eq!(job.due_at, old_end);
    vote_close::handle(db.clone(), nominee.world.clone(), job).await.unwrap();
    let ceo_id: Option<i64> = db.query_one("SELECT ceo_id FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap().get("ceo_id");
    assert_eq!(ceo_id, Some(candidate.character));
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let change = Value::dict([("ceoID", Value::tuple([Value::Int(ceo.character), Value::Int(candidate.character)]))]);
    assert!(events.iter().any(|(name, args)| name == "OnCorporationChanged" && args.contains(&change)));
    assert!(events.iter().any(|(name, _)| name == "__session"));
    assert_eq!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=22 AND notification_id>$2", &[&candidate.character, &marker]).await.unwrap().len(), 1);
    assert!(sanctions::get(&db, success).await.unwrap().unwrap().acted_upon);
    assert!(votes::election(&db, corp).await.unwrap().is_none());
    let job = Job { id: format!("{}:{success}", vote_close::KIND), kind: vote_close::KIND.into(), due_at: old_end, payload: success.to_string() };
    vote_close::handle(db.clone(), nominee.world.clone(), job).await.unwrap();
    assert!(!std::iter::from_fn(|| rx.try_recv().ok()).any(|(name, _)| name == "__session" || name == "OnCorporationChanged"));
    assert_eq!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=22 AND notification_id>$2", &[&candidate.character, &marker]).await.unwrap().len(), 1);
    assert_eq!(db.query_one("SELECT ceo_id FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap().get::<_, Option<i64>>("ceo_id"), Some(candidate.character));
    }).await;
    let cleanup = crate::tests::catch_scenario(async {
    cleanup_votes(&db, &[corp, proxy]).await.unwrap();
    db.execute("DELETE FROM game.corp_member_events WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_role_history WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_shares WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1)", &[&[ceo.character, candidate.character].as_slice()]).await.unwrap();
    fixture::cleanup_corporation(&db, corp, &[ceo.character, candidate.character], 0).await.unwrap();
    fixture::cleanup_corporation(&db, proxy, &[], 0).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, candidate).await.unwrap();
    }).await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
    if let Err(panic) = cleanup { std::panic::resume_unwind(panic); }
}
