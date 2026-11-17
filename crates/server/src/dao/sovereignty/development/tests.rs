//! Real PostgreSQL checks for zero-state races, CAS, checkpoints and batch observations.
use super::*;

async fn activation(db: &crate::db::Db) -> (i64, String) {
    let version = format!("dev-test-{}", crate::handshake::random_u64());
    db.execute("INSERT INTO game.lua_content_versions (namespace,version,schema_version,compiler_identity,registration,source,data,fingerprint) VALUES ('sovereignty_indices',$1,1,'test',$2,$2,$2,$3)", &[&version, &&b""[..], &&[9u8; 32][..]]).await.unwrap();
    let ordinal: i64 = db.query_one("INSERT INTO game.lua_content_activations (namespace,version,fingerprint,published_at) VALUES ('sovereignty_indices',$1,$2,1) RETURNING ordinal", &[&version, &&[9u8; 32][..]]).await.unwrap().get("ordinal");
    (ordinal, version)
}

fn system() -> i64 {
    (crate::handshake::random_u64() >> 10) as i64 + 1
}

async fn finish(db: &crate::db::Db, systems: &[i64], alliances: &[i64], versions: &[&str], result: std::thread::Result<()>) {
    for system in systems {
        db.execute("DELETE FROM game.sov_development_checkpoints WHERE system_id=$1", &[system]).await.unwrap();
        db.execute("DELETE FROM game.sov_development WHERE system_id=$1", &[system]).await.unwrap();
        db.execute("DELETE FROM game.sov_systems WHERE system_id=$1", &[system]).await.unwrap();
    }
    for alliance in alliances {
        db.execute("DELETE FROM game.sov_alliance_policy WHERE alliance_id=$1", &[alliance]).await.unwrap();
    }
    for version in versions {
        db.execute("DELETE FROM game.lua_content_activations WHERE namespace='sovereignty_indices' AND version=$1", &[version]).await.unwrap();
        db.execute("DELETE FROM game.lua_content_versions WHERE namespace='sovereignty_indices' AND version=$1", &[version]).await.unwrap();
    }
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn state_cas_checkpoints_and_joined_observations() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (activation, version) = activation(&db).await;
    let (system, empty, developed, claimed, alliance) = (system(), system(), system(), system(), system());
    let history = vec![ActivationRow { ordinal: activation, namespace: NAMESPACE.into(), version: version.clone(), fingerprint: vec![9; 32], published_at: 1 }];
    let result = crate::tests::catch_scenario(async {
        assert!(ensure(&db, system, 100, activation + 1_000_000, &history).await.is_err());
        assert!(ensure(&db, system, 100, activation, &[ActivationRow { namespace: "other".into(), ..history[0].clone() }]).await.is_err());
        let (a, b) = tokio::join!(ensure(&db, system, 100, activation, &history), ensure(&db, system, 100, activation, &history));
        a.unwrap();
        b.unwrap();
        let row = read(&db, system).await.unwrap().unwrap();
        assert_eq!((row.military_points, row.industrial_points, row.evaluated_at, row.revision, row.activation_ordinal, row.trend_start_at, row.tail.as_ref()), (0.0, 0.0, 100, 0, activation, 100, None));
        let mut next = row.clone();
        next.evaluated_at = 200;
        next.military_points = 50.0;
        next.tail = Some(FactOrder { occurred_at: 150, source_namespace: "business".into(), source_key: "k".into(), ordinal: 0, slice_id: 0 });
        assert!(replace(&db, row.revision, &next, &history).await.unwrap());
        assert!(!replace(&db, row.revision, &next, &history).await.unwrap());
        let mut wrong = next.clone();
        wrong.activation_ordinal = activation + 1_000_000;
        assert!(replace(&db, 1, &wrong, &history).await.is_err());
        assert_eq!(lock(&db, system).await.unwrap().unwrap().revision, 1);
        assert!(read(&db, system + 1).await.unwrap().is_none());
        let checkpoints = |at: i64| CheckpointRow { system_id: system, checkpoint_at: at, military_points: 1.0, industrial_points: 2.0, activation_ordinal: activation };
        replace_checkpoints(&db, system, 0, &[checkpoints(50)], &history).await.unwrap();
        assert!(checkpoint_before(&db, system, 49).await.unwrap().is_none());
        assert_eq!(checkpoint_before(&db, system, 50).await.unwrap().unwrap().checkpoint_at, 50);
        replace_checkpoints(&db, system, 50, &[checkpoints(60), checkpoints(70)], &history).await.unwrap();
        assert_eq!(checkpoint_before(&db, system, 65).await.unwrap().unwrap().checkpoint_at, 60);
        assert_eq!(checkpoint_before(&db, system, 75).await.unwrap().unwrap().checkpoint_at, 70);
        assert!(active_systems(&db, 0, 64).await.unwrap().contains(&system));
        ensure(&db, developed, 100, activation, &history).await.unwrap();
        db.execute("INSERT INTO game.sov_systems (system_id,hub_item_id,claim_corporation_id,alliance_id,claim_time) VALUES ($1,$1,3,$2,4)", &[&claimed, &alliance]).await.unwrap();
        db.execute("INSERT INTO game.sov_alliance_policy (alliance_id,current_capital_system) VALUES ($1,$2)", &[&alliance, &claimed]).await.unwrap();
        let rows = observations(&db, &[empty, developed, claimed]).await.unwrap();
        assert_eq!(rows.len(), 3);
        let find = |system| rows.iter().find(|row| row.system_id == system).unwrap();
        let vacant = find(empty);
        assert!(vacant.system.is_none() && vacant.development.is_none() && vacant.capital.is_none());
        let scored = find(developed);
        assert_eq!(scored.development.as_ref().unwrap().system_id, developed);
        assert!(scored.system.is_none() && scored.capital.is_none());
        let occupied = find(claimed);
        let owned = occupied.system.as_ref().unwrap();
        assert_eq!((owned.alliance_id, owned.claim_time), (Some(alliance), Some(4)));
        assert!(occupied.development.is_none());
        assert_eq!(occupied.capital.as_ref().unwrap().current_capital_system, Some(claimed));
        assert!(observations(&db, &[]).await.unwrap().is_empty());
        assert!(active_systems(&db, 0, 64).await.unwrap().contains(&claimed));
        assert!(!active_systems(&db, 0, i64::MAX).await.unwrap().contains(&developed));
    }).await;
    finish(&db, &[system, empty, developed, claimed], &[alliance], &[version.as_str()], result).await;
}
