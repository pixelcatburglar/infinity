//! Real PostgreSQL persistence and rollback checks using transaction-owned fixtures.
use super::{systems,structures,capture,policy,history,sources};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn hub_state_cas_constraints_history_invalidation_and_capture_dao_lock() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let user:i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('sov-' || nextval('game.accounts_user_id_seq'),$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character:i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'sov-' || nextval('game.characters_character_id_seq'),1,1,1,1) RETURNING character_id", &[&user]).await.unwrap().get("character_id");
    let item:i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id) VALUES (1,$1,$1,1) RETURNING item_id", &[&character]).await.unwrap().get("item_id");
    let system = item;
    let ns = format!("sov-fixture-{item}");
    tx.execute("INSERT INTO game.lua_content_versions (namespace,version,schema_version,compiler_identity,registration,source,data,fingerprint) VALUES ($1,'1',1,'test',$2,$2,$2,$3)", &[&ns, &&b""[..], &&[0u8;32][..]]).await.unwrap();
    let initial = systems::lock(&tx,system).await.unwrap();
    assert_eq!(initial.ownership_epoch,0);
    let deployed = systems::SystemChange { hub_item_id:Some(item),claim_corporation_id:None,alliance_id:None,claim_time:None,ownership_epoch:0 };
    let deployed_row = systems::replace(&tx,&initial,&deployed).await.unwrap().unwrap();
    assert!(systems::replace(&tx,&initial,&deployed).await.unwrap().is_none());
    let structure = structures::StructureRow { item_id:item,system_id:system,source_item_id:item,deployer_character_id:character,deployer_corporation_id:1,deployed_at:1,content_namespace:ns.clone(),content_version:"1".into(),self_destruct_due_at:None,self_destruct_actor:None };
    assert!(structures::insert(&tx,&structure).await.unwrap());
    let other = systems::lock(&tx,system + 1).await.unwrap();
    let save = tx.savepoint("unique_hub").await.unwrap();
    assert!(systems::replace(&save,&other,&deployed).await.is_err());
    save.rollback().await.unwrap();
    assert!(!structures::insert(&tx,&structure).await.unwrap());
    let claim = systems::SystemChange { hub_item_id:Some(item),claim_corporation_id:Some(1),alliance_id:Some(2),claim_time:Some(3),ownership_epoch:1 };
    let claimed = systems::replace(&tx,&deployed_row,&claim).await.unwrap().unwrap();
    let transfer = systems::SystemChange { claim_corporation_id:Some(4),..claim };
    let transferred = systems::replace(&tx,&claimed,&transfer).await.unwrap().unwrap();
    assert_eq!(transferred.ownership_epoch,1);
    let invalid = systems::SystemChange { claim_time:None,..transfer };
    let save = tx.savepoint("grouped_null").await.unwrap();
    assert!(systems::replace(&save,&transferred,&invalid).await.is_err());
    save.rollback().await.unwrap();
    let p = policy::lock(&tx,character).await.unwrap();
    assert!(policy::replace(&tx,p.revision,&p).await.unwrap());
    assert!(!policy::replace(&tx,p.revision,&p).await.unwrap());
    let h = history::HistoryRow { history_id:0,system_id:system,hub_item_id:item,old_corporation_id:None,new_corporation_id:Some(1),old_alliance_id:None,new_alliance_id:Some(2),old_epoch:0,new_epoch:1,reason:"claim".into(),at:3,actor_id:Some(character),operation_key:ns.clone(),content_namespace:ns.clone(),content_version:"1".into() };
    let id=history::insert(&tx,&h).await.unwrap().unwrap();
    assert!(history::insert(&tx,&h).await.unwrap().is_none());
    assert_eq!(history::recent(&tx).await.unwrap().into_iter().find(|row| row.operation_key == ns).unwrap().history_id,id);
    let mut c = capture::CaptureRow { target_item_id:item,system_id:system,ownership_epoch:1,controlling_team:None,scoring_team:None,defender_team:1,score:0.0,rate:0.0,blocked:false,score_sim_time:1,checkpoint_wall_time:1,revision:0,capture_time_seconds:1.0,defensive_regen_rate:0.0,checkpoint_interval_ms:1,content_namespace:ns.clone(),content_version:"1".into() };
    assert!(capture::insert(&tx,&c).await.unwrap());
    c.score=0.5;
    assert!(capture::checkpoint(&tx,0,&c).await.unwrap());
    assert!(!capture::checkpoint(&tx,0,&c).await.unwrap());
    let lock_query = "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = pg_backend_pid() AND relation = 'game.sov_capture_targets'::regclass AND mode = 'RowShareLock' AND granted) AS locked";
    assert!(!tx.query_one(lock_query,&[]).await.unwrap().get::<_,bool>("locked"));
    let current = capture::lock_for_system(&tx,system).await.unwrap().unwrap();
    assert_eq!(current.revision,1);
    assert_eq!(current.score,0.5);
    assert!(tx.query_one(lock_query,&[]).await.unwrap().get::<_,bool>("locked"));
    c.score=f64::NAN;
    let save=tx.savepoint("finite_score").await.unwrap();
    assert!(capture::checkpoint(&save,1,&c).await.is_err());
    save.rollback().await.unwrap();
    let v=sources::ensure(&tx,character).await.unwrap();
    assert_eq!(v.revision,0);
    assert_eq!(sources::invalidate(&tx,&[character],10).await.unwrap()[0].revision,1);
    assert_eq!(sources::invalidate(&tx,&[character],20).await.unwrap()[0].revision,2);
    assert_eq!(sources::first_change_after(&tx,character,0).await.unwrap(),Some(sources::SourceChange { character_id:character,revision:2,effective_at:10 }));
    let save=tx.savepoint("rollback_change").await.unwrap();
    sources::invalidate(&save,&[character],30).await.unwrap();
    save.rollback().await.unwrap();
    assert_eq!(sources::versions(&tx,&[character]).await.unwrap()[0].revision,2);
    assert!(sources::first_change_after(&tx,character,2).await.unwrap().is_none());
    let lost = systems::SystemChange { hub_item_id:None,claim_corporation_id:None,alliance_id:None,claim_time:None,ownership_epoch:2 };
    assert_eq!(systems::replace(&tx,&transferred,&lost).await.unwrap().unwrap().ownership_epoch,2);
    assert_eq!(systems::read(&tx,system).await.unwrap().unwrap().ownership_epoch,2);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn source_cutoff_uses_earliest_time_and_highest_revision() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let user:i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('sov-' || nextval('game.accounts_user_id_seq'),$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let character:i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'sov-' || nextval('game.characters_character_id_seq'),1,1,1,1) RETURNING character_id", &[&user]).await.unwrap().get("character_id");
    let other:i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'sov-' || nextval('game.characters_character_id_seq'),1,1,1,1) RETURNING character_id", &[&user]).await.unwrap().get("character_id");
    assert_eq!(sources::ensure(&tx,character).await.unwrap().revision,0);
    assert_eq!(sources::ensure(&tx,other).await.unwrap().revision,0);
    assert!(sources::first_change_after(&tx,character,0).await.unwrap().is_none());
    assert!(sources::first_change_after(&tx,-1,0).await.unwrap().is_none());
    sources::invalidate(&tx,&[character],20).await.unwrap();
    sources::invalidate(&tx,&[character],10).await.unwrap();
    sources::invalidate(&tx,&[other],10).await.unwrap();
    sources::invalidate(&tx,&[other],20).await.unwrap();
    assert_eq!(sources::first_change_after(&tx,character,0).await.unwrap(),Some(sources::SourceChange { character_id:character,revision:2,effective_at:10 }));
    assert_eq!(sources::first_change_after(&tx,other,0).await.unwrap(),Some(sources::SourceChange { character_id:other,revision:2,effective_at:10 }));
    assert_eq!(sources::first_change_after(&tx,character,1).await.unwrap(),Some(sources::SourceChange { character_id:character,revision:2,effective_at:10 }));
    assert!(sources::first_change_after(&tx,character,2).await.unwrap().is_none());
    assert!(sources::first_change_after(&tx,character,3).await.unwrap().is_none());
    assert_eq!(sources::versions(&tx,&[character]).await.unwrap()[0].revision,2);
    let save=tx.savepoint("rollback_source_cutoff").await.unwrap();
    sources::invalidate(&save,&[character],5).await.unwrap();
    assert_eq!(sources::first_change_after(&save,character,0).await.unwrap(),Some(sources::SourceChange { character_id:character,revision:3,effective_at:5 }));
    save.rollback().await.unwrap();
    assert_eq!(sources::versions(&tx,&[character]).await.unwrap()[0].revision,2);
    assert_eq!(sources::first_change_after(&tx,character,0).await.unwrap(),Some(sources::SourceChange { character_id:character,revision:2,effective_at:10 }));
    assert!(sources::first_change_after(&tx,character,2).await.unwrap().is_none());
    tx.rollback().await.unwrap();
}
