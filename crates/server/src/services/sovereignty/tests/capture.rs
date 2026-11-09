//! Transaction-owned checkpoint and source-generation rollback checks.
use crate::{dao::sovereignty::{capture as dao, sources, structures, systems}, services::sovereignty::{capture::{checkpoint_in, CheckpointOutcome}, CaptureContribution, CaptureContributor, CaptureObservation}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn checkpoint_uses_locked_anchor_and_rolls_back_stale_generation() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let user: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('capture-' || nextval('game.accounts_user_id_seq'),$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'capture-' || nextval('game.characters_character_id_seq'),1,1,1,1) RETURNING character_id", &[&user]).await.unwrap().get("character_id");
    let item: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id) VALUES (1,$1,$1,1) RETURNING item_id", &[&character]).await.unwrap().get("item_id");
    let ns = format!("capture-fixture-{item}");
    tx.execute("INSERT INTO game.lua_content_versions (namespace,version,schema_version,compiler_identity,registration,source,data,fingerprint) VALUES ($1,'1',1,'test',$2,$2,$2,$3)", &[&ns, &&b""[..], &&[0u8;32][..]]).await.unwrap();
    let initial = systems::lock(&tx, item).await.unwrap();
    let change = systems::SystemChange { hub_item_id: Some(item), claim_corporation_id: None, alliance_id: None, claim_time: None, ownership_epoch: 0 };
    let system = systems::replace(&tx, &initial, &change).await.unwrap().unwrap();
    let structure = structures::StructureRow { item_id: item, system_id: item, source_item_id: item, deployer_character_id: character, deployer_corporation_id: 1, deployed_at: 1, content_namespace: ns.clone(), content_version: "1".into(), self_destruct_due_at: None, self_destruct_actor: None };
    assert!(structures::insert(&tx, &structure).await.unwrap());
    let row = dao::CaptureRow { target_item_id: item, system_id: item, ownership_epoch: 0, controlling_team: None, scoring_team: None, defender_team: 1, score: 0.0, rate: 0.0, blocked: false, score_sim_time: 1, checkpoint_wall_time: 1, revision: 0, capture_time_seconds: 1.0, defensive_regen_rate: 0.0, checkpoint_interval_ms: 1, content_namespace: ns, content_version: "1".into() };
    assert!(dao::insert(&tx, &row).await.unwrap());
    sources::ensure(&tx, character).await.unwrap();
    let mut observation = CaptureObservation { target: row, system_revision: system.revision, contributors: Vec::new(), claimant: None };
    observation.target.score = 0.5;
    let save = tx.savepoint("checkpoint").await.unwrap();
    let outcome = checkpoint_in(&save, &[observation.clone()]).await.unwrap();
    let CheckpointOutcome::Accepted(anchor) = &outcome[0] else { panic!("expected accepted checkpoint") };
    let lock_query = "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = pg_backend_pid() AND relation = 'game.sov_capture_targets'::regclass AND mode = 'RowShareLock' AND granted) AS locked";
    assert!(save.query_one(lock_query, &[]).await.unwrap().get::<_, bool>("locked"));
    assert_eq!(anchor.revision, 1);
    assert_eq!(anchor.score, 0.5);
    let stale = checkpoint_in(&save, &[observation.clone()]).await.unwrap();
    let CheckpointOutcome::Rejected { current, .. } = &stale[0] else { panic!("expected stale rejection") };
    assert_eq!(current.capture.as_ref().unwrap(), anchor);
    save.rollback().await.unwrap();
    assert_eq!(dao::for_system(&tx, item).await.unwrap().unwrap().revision, 0);
    let mut wrong_parameters = observation.clone();
    wrong_parameters.target.capture_time_seconds = 2.0;
    let rejected = checkpoint_in(&tx, &[wrong_parameters]).await.unwrap();
    let CheckpointOutcome::Rejected { current, .. } = &rejected[0] else { panic!("expected parameter rejection") };
    assert_eq!(current.capture.as_ref().unwrap().revision, 0);
    assert_eq!(current.capture.as_ref().unwrap().score, 0.0);
    let other_item: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id) VALUES (1,$1,$1,1) RETURNING item_id", &[&character]).await.unwrap().get("item_id");
    let other_initial = systems::lock(&tx, other_item).await.unwrap();
    let other_change = systems::SystemChange { hub_item_id: Some(other_item), claim_corporation_id: None, alliance_id: None, claim_time: None, ownership_epoch: 0 };
    let other_system = systems::replace(&tx, &other_initial, &other_change).await.unwrap().unwrap();
    let other_structure = structures::StructureRow { item_id: other_item, system_id: other_item, ..structure.clone() };
    assert!(structures::insert(&tx, &other_structure).await.unwrap());
    let other_row = dao::CaptureRow { target_item_id: other_item, system_id: other_item, ..observation.target.clone() };
    assert!(dao::insert(&tx, &other_row).await.unwrap());
    let other_observation = CaptureObservation { target: other_row, system_revision: other_system.revision, contributors: Vec::new(), claimant: None };
    let mut wrong = observation.clone();
    wrong.target.content_version = "wrong".into();
    let batch = checkpoint_in(&tx, &[other_observation, wrong]).await.unwrap();
    assert!(batch.iter().all(|outcome| matches!(outcome, CheckpointOutcome::Rejected { .. })));
    assert_eq!(dao::for_system(&tx, other_item).await.unwrap().unwrap().revision, 0);
    let save = tx.savepoint("generation").await.unwrap();
    let other_character: i64 = save.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'capture-' || nextval('game.characters_character_id_seq'),1,1,1,1) RETURNING character_id", &[&user]).await.unwrap().get("character_id");
    let mut orgs = Vec::new();
    for pilot in [character, other_character] {
        let corp: i64 = save.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('capture-' || nextval('game.corporations_corporation_id_seq'),'P' || currval('game.corporations_corporation_id_seq'),$1,$1,1,1) RETURNING corporation_id", &[&pilot]).await.unwrap().get("corporation_id");
        let alliance: i64 = save.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('capture-' || nextval('game.alliances_alliance_id_seq'),'P' || currval('game.alliances_alliance_id_seq'),$1,$1,$2,1) RETURNING alliance_id", &[&corp, &pilot]).await.unwrap().get("alliance_id");
        save.execute("UPDATE game.corporations SET alliance_id=$1 WHERE corporation_id=$2", &[&alliance, &corp]).await.unwrap();
        save.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&corp, &pilot]).await.unwrap();
        sources::ensure(&save, pilot).await.unwrap();
        orgs.push((corp, alliance));
    }
    sources::invalidate(&save, &[character], 10).await.unwrap();
    save.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&character]).await.unwrap();
    observation.target.score_sim_time = 20;
    observation.contributors.push(CaptureContribution { contributor: CaptureContributor { character_id: character, ship_id: item, module_id: item, corporation_id: orgs[0].0, alliance_id: orgs[0].1, source_revision: 0 }, through: 20 });
    observation.contributors.push(CaptureContribution { contributor: CaptureContributor { character_id: other_character, ship_id: item, module_id: item, corporation_id: orgs[1].0, alliance_id: orgs[1].1, source_revision: 0 }, through: 10 });
    let rejected = checkpoint_in(&save, &[observation.clone()]).await.unwrap();
    let CheckpointOutcome::Rejected { current, changes } = &rejected[0] else { panic!("expected source rejection") };
    assert_eq!(current.capture.as_ref().unwrap().revision, 0);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].effective_at, 10);
    observation.target.score_sim_time = 10;
    observation.contributors[0].through = 10;
    let accepted = checkpoint_in(&save, &[observation]).await.unwrap();
    let CheckpointOutcome::Accepted(anchor) = &accepted[0] else { panic!("expected corrected prefix") };
    assert_eq!(anchor.revision, 1);
    assert_eq!(anchor.score_sim_time, 10);
    save.rollback().await.unwrap();
    assert_eq!(sources::versions(&tx, &[character]).await.unwrap()[0].revision, 0);
    assert!(sources::first_change_after(&tx, character, 0).await.unwrap().is_none());
    tx.rollback().await.unwrap();
}
