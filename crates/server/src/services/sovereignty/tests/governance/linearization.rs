//! Guard-order regression for committed capture anchors and organization changes.
use crate::{client::filetime_now, dao::{corporations::registry, sovereignty::{capture, sources, structures, systems}}, db::outbox::Outbox, services::{alliances, sovereignty::governance::{self, AdmittedAllianceChange, PreparedAllianceChange}}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn waiting_alliance_departure_cannot_backdate_source_before_checkpoint() {
    let cfg = crate::config::tests::test_database();
    let (mut setup, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let ids = setup.query_one("SELECT nextval('game.accounts_user_id_seq') AS account, nextval('game.characters_character_id_seq') AS character, nextval('game.corporations_corporation_id_seq') AS corp, nextval('game.alliances_alliance_id_seq') AS alliance, nextval('game.items_item_id_seq') AS item", &[]).await.unwrap();
    let account: i64 = ids.get("account");
    let character: i64 = ids.get("character");
    let corp: i64 = ids.get("corp");
    let alliance: i64 = ids.get("alliance");
    let item: i64 = ids.get("item");
    let namespace = format!("sov-linearization-{item}");
    let tx = setup.transaction().await.unwrap();
    tx.execute("INSERT INTO game.accounts (user_id,user_name,password_hash,role) VALUES ($1,'sov-linearization-' || $1::bigint::text,$2,0)", &[&account, &&b"fixture"[..]]).await.unwrap();
    tx.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,$2,'sov-linearization-' || $1::bigint::text,1,1,1,$3)", &[&character, &account, &corp]).await.unwrap();
    tx.execute("INSERT INTO game.corporations (corporation_id,name,ticker,ceo_id,creator_id,station_id,created,alliance_id,alliance_joined) VALUES ($1,'sov-linearization-' || $1::bigint::text,'L' || $1::bigint::text,$2,$2,1,1,$3,1)", &[&corp, &character, &alliance]).await.unwrap();
    tx.execute("INSERT INTO game.alliances (alliance_id,name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ($1,'sov-linearization-' || $1::bigint::text,'L' || $1::bigint::text,$2,$2,$3,1)", &[&alliance, &corp, &character]).await.unwrap();
    tx.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id) VALUES ($1,1,$2,$2,1)", &[&item, &character]).await.unwrap();
    tx.execute("INSERT INTO game.lua_content_versions (namespace,version,schema_version,compiler_identity,registration,source,data,fingerprint) VALUES ($1,'1',1,'test',$2,$2,$2,$3)", &[&namespace, &&b""[..], &&[0u8; 32][..]]).await.unwrap();
    sources::ensure(&tx, character).await.unwrap();
    let initial = systems::lock(&tx, item).await.unwrap();
    let deployed = systems::SystemChange { hub_item_id: Some(item), claim_corporation_id: None, alliance_id: None, claim_time: None, ownership_epoch: 0 };
    systems::replace(&tx, &initial, &deployed).await.unwrap().unwrap();
    let structure = structures::StructureRow { item_id: item, system_id: item, source_item_id: item, deployer_character_id: character, deployer_corporation_id: corp, deployed_at: 1, content_namespace: namespace.clone(), content_version: "1".into(), self_destruct_due_at: None, self_destruct_actor: None };
    assert!(structures::insert(&tx, &structure).await.unwrap());
    let target = capture::CaptureRow { target_item_id: item, system_id: item, ownership_epoch: 0, controlling_team: None, scoring_team: None, defender_team: alliance, score: 0.0, rate: 0.0, blocked: false, score_sim_time: 1, checkpoint_wall_time: 1, revision: 0, capture_time_seconds: 1.0, defensive_regen_rate: 0.0, checkpoint_interval_ms: 1, content_namespace: namespace.clone(), content_version: "1".into() };
    assert!(capture::insert(&tx, &target).await.unwrap());
    tx.commit().await.unwrap();

    // A child task lets cleanup run even if an assertion or database operation panics.
    let result = tokio::spawn(async move {
        let cfg = crate::config::tests::test_database();
        let (mut checkpoint, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let (mut leave, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let (mut observer, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let checkpoint_pid: i32 = checkpoint.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
        let leave_pid: i32 = leave.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
        let guard = checkpoint.transaction().await.unwrap();
        assert!(registry::lock_admission(&guard, &[alliance], &[corp], &[character]).await.unwrap());
        let business_now = filetime_now();
        let preparation = leave.transaction().await.unwrap();
        assert!(matches!(governance::prepare(&preparation, corp, business_now).await.unwrap(), PreparedAllianceChange::NoOwnershipChange));
        preparation.commit().await.unwrap();
        let departure = async {
            let tx = leave.transaction().await.unwrap();
            assert!(registry::lock_admission(&tx, &[alliance], &[corp], &[character]).await.unwrap());
            let mut outbox: Outbox = Outbox::default();
            alliances::leave_alliance(&tx, &mut outbox, corp, business_now, &AdmittedAllianceChange::NoOwnershipChange).await.unwrap();
            tx.commit().await.unwrap();
        };
        tokio::pin!(departure);
        loop {
            tokio::select! {
                _ = &mut departure => panic!("departure passed held admission guards"),
                blocked = async { observer.query_one("SELECT $1 = ANY(pg_blocking_pids($2)) AS blocked", &[&checkpoint_pid, &leave_pid]).await } => {
                    if blocked.unwrap().get::<_, bool>("blocked") { break; }
                }
            }
        }
        let anchor = loop { let now = filetime_now(); if now > business_now { break now; } tokio::task::yield_now().await; };
        let mut next = target;
        next.checkpoint_wall_time = anchor;
        assert!(capture::checkpoint(&guard, next.revision, &next).await.unwrap());
        guard.commit().await.unwrap();
        departure.await;
        let observation = observer.transaction().await.unwrap();
        let committed = capture::read(&observation, item).await.unwrap().unwrap();
        let change = sources::first_change_after(&observation, character, 0).await.unwrap().unwrap();
        observation.commit().await.unwrap();
        assert_eq!(committed.checkpoint_wall_time, anchor);
        assert!(change.effective_at > committed.checkpoint_wall_time);
    }).await;

    let tx = setup.transaction().await.unwrap();
    tx.execute("DELETE FROM game.sov_link_source_changes WHERE character_id=$1", &[&character]).await.unwrap();
    tx.execute("DELETE FROM game.sov_link_sources WHERE character_id=$1", &[&character]).await.unwrap();
    tx.execute("DELETE FROM game.sov_capture_targets WHERE target_item_id=$1", &[&item]).await.unwrap();
    tx.execute("DELETE FROM game.alliance_history WHERE corporation_id=$1", &[&corp]).await.unwrap();
    tx.execute("DELETE FROM game.sov_structures WHERE item_id=$1", &[&item]).await.unwrap();
    tx.execute("DELETE FROM game.sov_systems WHERE system_id=$1", &[&item]).await.unwrap();
    tx.execute("DELETE FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
    tx.execute("DELETE FROM game.lua_content_versions WHERE namespace=$1 AND version='1'", &[&namespace]).await.unwrap();
    tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    tx.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    tx.execute("DELETE FROM game.alliances WHERE alliance_id=$1", &[&alliance]).await.unwrap();
    tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    tx.commit().await.unwrap();
    result.unwrap();
}
