//! Database boarding transitions, refusals, rollback and competing transactions.
use super::*;
use tokio_postgres::{Client, NoTls, Transaction};

async fn connect() -> Client {
    let cfg = crate::config::tests::test_database();
    let (client, connection) = cfg.to_pg_config().unwrap().connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    client
}

#[derive(Clone, Copy)]
struct Fixture { account: i64, character: i64, other: i64, old: i64, target: i64, spare: i64 }

async fn fixture(tx: &Transaction<'_>) -> Fixture {
    let account = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('boarding-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let mut chars = [0_i64; 2];
    for character in &mut chars {
        *character = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id) VALUES ($1,'boarding-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    }
    let mut ships = [0_i64; 3];
    for ship in &mut ships {
        *ship = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES (1,$1,1,0,1,true,0,0,0) RETURNING item_id", &[&chars[0]]).await.unwrap().get("item_id");
    }
    tx.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&chars[0], &ships[0]]).await.unwrap();
    Fixture { account, character: chars[0], other: chars[1], old: ships[0], target: ships[1], spare: ships[2] }
}

async fn hashes(tx: &Transaction<'_>, f: &Fixture) -> Vec<String> {
    tx.query("SELECT md5(row_to_json(t)::text) AS hash FROM game.characters t WHERE character_id=ANY($1) UNION ALL SELECT md5(row_to_json(t)::text) AS hash FROM game.items t WHERE item_id=ANY($2) ORDER BY hash", &[&&[f.character, f.other][..], &&[f.old, f.target, f.spare][..]]).await.unwrap().iter().map(|r| r.get("hash")).collect()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn boarding_guards_and_rollback() {
    let mut client = connect().await;
    let mut tx = client.transaction().await.unwrap();
    let f = fixture(&tx).await;
    let before = hashes(&tx, &f).await;
    let save = tx.savepoint("board").await.unwrap();
    assert!(board_in_space(&save, f.character, f.old, f.target, 1, 1, 1, [1.0, 2.0, 3.0], [4.0, 5.0, 6.0]).await.unwrap());
    let row = save.query_one("SELECT ship_id FROM game.characters WHERE character_id=$1", &[&f.character]).await.unwrap();
    assert_eq!(row.get::<_, i64>("ship_id"), f.target);
    let row = save.query_one("SELECT x,y,z FROM game.items WHERE item_id=$1", &[&f.old]).await.unwrap();
    assert_eq!([row.get::<_, f64>("x"), row.get("y"), row.get("z")], [1.0, 2.0, 3.0]);
    save.rollback().await.unwrap();
    assert_eq!(before, hashes(&tx, &f).await);
    for pos in [[f64::NAN, 0.0, 0.0], [0.0, f64::INFINITY, 0.0], [0.0, 0.0, f64::NEG_INFINITY]] {
        assert!(!board_in_space(&tx, f.character, f.old, f.target, 1, 1, 1, pos, [0.0; 3]).await.unwrap());
        assert!(!board_in_space(&tx, f.character, f.old, f.target, 1, 1, 1, [0.0; 3], pos).await.unwrap());
    }
    assert!(!board_in_space(&tx, f.character, f.old, f.old, 1, 1, 1, [0.0; 3], [0.0; 3]).await.unwrap());
    assert!(!board_in_space(&tx, f.character, f.old, f.target, 2, 1, 1, [0.0; 3], [0.0; 3]).await.unwrap());
    assert!(!board_in_space(&tx, f.character, f.old, f.target, 1, 2, 1, [0.0; 3], [0.0; 3]).await.unwrap());
    assert!(!board_in_space(&tx, f.character, f.old, f.target, 1, 1, 2, [0.0; 3], [0.0; 3]).await.unwrap());
    for sql in ["UPDATE game.items SET owner_id=0 WHERE item_id=$1", "UPDATE game.items SET quantity=2 WHERE item_id=$1", "UPDATE game.items SET singleton=false WHERE item_id=$1", "UPDATE game.items SET location_id=2 WHERE item_id=$1", "UPDATE game.items SET flag_id=1 WHERE item_id=$1", "UPDATE game.items SET x='NaN'::float8 WHERE item_id=$1", "UPDATE game.items SET y='Infinity'::float8 WHERE item_id=$1", "UPDATE game.items SET z=NULL WHERE item_id=$1"] {
        for id in [f.old, f.target] {
            let save = tx.savepoint("bad_item").await.unwrap();
            save.execute(sql, &[&id]).await.unwrap();
            assert!(!board_in_space(&save, f.character, f.old, f.target, 1, 1, 1, [0.0; 3], [0.0; 3]).await.unwrap());
            save.rollback().await.unwrap();
        }
    }
    for sql in ["UPDATE game.characters SET deleted=true WHERE character_id=$1", "UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", "UPDATE game.characters SET station_id=1 WHERE character_id=$1", "UPDATE game.characters SET solar_system_id=2 WHERE character_id=$1"] {
        let save = tx.savepoint("bad_character").await.unwrap();
        save.execute(sql, &[&f.character]).await.unwrap();
        assert!(!board_in_space(&save, f.character, f.old, f.target, 1, 1, 1, [0.0; 3], [0.0; 3]).await.unwrap());
        save.rollback().await.unwrap();
    }
    let save = tx.savepoint("occupied").await.unwrap();
    save.execute("UPDATE game.characters SET ship_id=$2,deleted=true WHERE character_id=$1", &[&f.other, &f.target]).await.unwrap();
    assert!(!board_in_space(&save, f.character, f.old, f.target, 1, 1, 1, [0.0; 3], [0.0; 3]).await.unwrap());
    save.rollback().await.unwrap();
    assert_eq!(before, hashes(&tx, &f).await);
    tx.execute("UPDATE game.characters SET station_id=1 WHERE character_id=$1", &[&f.character]).await.unwrap();
    tx.execute("UPDATE game.items SET flag_id=$2 WHERE item_id=ANY($1)", &[&&[f.old, f.target][..], &FLAG_HANGAR]).await.unwrap();
    assert!(!board_in_station(&tx, f.character, f.old, f.target, 2, 1, 1).await.unwrap());
    assert!(board_in_station(&tx, f.character, f.old, f.target, 1, 1, 1).await.unwrap());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn moved_ship_selection_checks_account_position_and_occupancy() {
    let mut client = connect().await;
    let tx = client.transaction().await.unwrap();
    let f = fixture(&tx).await;
    assert!(!guard_current(&tx, f.character, f.account + 1, f.old, 1, 1).await.unwrap());
    assert!(!guard_current(&tx, f.character, f.account, f.old, 1, 2).await.unwrap());
    assert!(guard_current(&tx, f.character, f.account, f.old, 1, 1).await.unwrap());
    assert!(!position_moved(&tx, f.target, f.character, 1, [f64::NAN, 0.0, 0.0]).await.unwrap());
    assert!(!position_moved(&tx, f.target, f.other, 1, [1.0; 3]).await.unwrap());
    assert!(position_moved(&tx, f.target, f.character, 1, [1.0; 3]).await.unwrap());
    assert!(unselected(&tx, f.target).await.unwrap());
    assert!(!select_moved(&tx, f.character, f.account + 1, f.old, f.target, 1).await.unwrap());
    assert!(select_moved(&tx, f.character, f.account, f.old, f.target, 1).await.unwrap());
    assert!(!unselected(&tx, f.target).await.unwrap());
    assert!(!select_moved(&tx, f.character, f.account, f.old, f.spare, 1).await.unwrap());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn recovery_proves_complete_state_or_returns_unknown() {
    let mut client = connect().await;
    let mut tx = client.transaction().await.unwrap();
    let f = fixture(&tx).await;
    let board = |user, system, old_type, target_type| RecoveryPlan::Board {
        character: f.character, user, system, old: f.old, old_type, target: f.target, target_type,
    };
    assert!(matches!(recovery(&tx, &board(f.account, 1, 1, 1)).await.unwrap(), Recovery::Aborted));
    for plan in [board(f.account + 1, 1, 1, 1), board(f.account, 2, 1, 1),
        board(f.account, 1, 2, 1), board(f.account, 1, 1, 2)] {
        assert!(matches!(recovery(&tx, &plan).await.unwrap(), Recovery::Unknown));
    }
    let stored = crate::dao::items::creation::rookie_ship(&tx, 1, f.other, f.spare, infinity_core::consts::FLAG_SHIP_HANGAR).await.unwrap();
    let plan = |host, prior_owner| RecoveryPlan::BoardStored {
        character: f.character, user: f.account, system: 1, old: f.old, old_type: 1,
        host, target: stored, target_type: 1, prior_owner,
    };
    assert!(matches!(recovery(&tx, &plan(f.spare, f.other)).await.unwrap(), Recovery::Aborted));
    assert!(matches!(recovery(&tx, &plan(f.target, f.other)).await.unwrap(), Recovery::Unknown));
    assert!(matches!(recovery(&tx, &plan(f.spare, f.character)).await.unwrap(), Recovery::Unknown));
    let packaged = crate::dao::items::transfers::insert(&tx, 1, f.other, f.spare, infinity_core::consts::FLAG_SHIP_HANGAR, 1).await.unwrap();
    let wrong_flag = crate::dao::items::creation::rookie_ship(&tx, 1, f.other, f.spare, FLAG_HANGAR).await.unwrap();
    crate::dao::items::creation::fitted_items(&tx, f.spare, f.other, &[(2, infinity_core::consts::FLAG_SHIP_HANGAR, 2)]).await.unwrap();
    let rows = crate::dao::items::loading::load_items(&tx, crate::dao::items::loading::ItemQuery::Location(f.spare)).await.unwrap();
    let too_many = rows.iter().find(|row| row.type_id == 2).unwrap().item_id;
    for (target, target_type) in [(packaged, 1), (wrong_flag, 1), (too_many, 2)] {
        let invalid = RecoveryPlan::BoardStored { character: f.character, user: f.account, system: 1,
            old: f.old, old_type: 1, host: f.spare, target, target_type, prior_owner: f.other };
        assert!(matches!(recovery(&tx, &invalid).await.unwrap(), Recovery::Unknown));
    }
    let clone = crate::dao::characters::clones::create(&tx, f.character, 1).await.unwrap();
    let dormant = crate::dao::items::creation::rookie_ship(&tx, 1, f.character, f.character, infinity_core::consts::FLAG_IMPLANT).await.unwrap();
    crate::dao::characters::clones::lock_character(&tx, f.character).await.unwrap().unwrap();
    assert!(crate::dao::items::brain::install(&tx, dormant, f.character, f.character, infinity_core::consts::FLAG_IMPLANT,
        infinity_core::consts::FLAG_IMPLANT, 1, None, None, None).await.unwrap());
    assert!(crate::dao::items::clones::park(&tx, f.character, clone.clone_id, dormant).await.unwrap());
    assert!(crate::dao::items::loading::load_items(&tx, crate::dao::items::loading::ItemQuery::Id(dormant)).await.unwrap().is_empty());
    let cloned = RecoveryPlan::Board { character: f.character, user: f.account, system: 1,
        old: f.old, old_type: 1, target: dormant, target_type: 1 };
    assert!(matches!(recovery(&tx, &cloned).await.unwrap(), Recovery::Unknown));
    assert!(!board_in_space(&tx, f.character, f.old, dormant, 1, 1, 1, [0.0; 3], [0.0; 3]).await.unwrap());
    let store = RecoveryPlan::Store { character: f.character, user: f.account, system: 1,
        old: f.old, old_type: 1, host: f.spare };
    assert!(matches!(recovery(&tx, &store).await.unwrap(), Recovery::Aborted));
    let save = tx.savepoint("stored_commit").await.unwrap();
    let locks = crate::dao::items::parent_locks::lock(&save, &[stored], &[crate::dao::items::parent_locks::LockRequest {
        item_id: stored, strength: crate::dao::items::parent_locks::LockStrength::Update,
    }]).await.unwrap();
    crate::dao::items::transfers::hand_over(&save, &[crate::dao::items::transfers::Transfer {
        item_id: stored, owner: f.character, location: 1, flag: 0,
    }], &[stored], &locks, &[]).await.unwrap();
    assert!(position_moved(&save, stored, f.character, 1, [3.0; 3]).await.unwrap());
    assert!(select_moved(&save, f.character, f.account, f.old, stored, 1).await.unwrap());
    let Recovery::Committed { target, old: Some(old) } = recovery(&save, &plan(f.spare, f.other)).await.unwrap() else { panic!("expected committed stored boarding") };
    assert_eq!((target.item_id, target.pos, old.item_id), (stored, [3.0; 3], f.old));
    save.rollback().await.unwrap();
    let save = tx.savepoint("store_commit").await.unwrap();
    let locks = crate::dao::items::parent_locks::lock(&save, &[f.old], &[crate::dao::items::parent_locks::LockRequest {
        item_id: f.old, strength: crate::dao::items::parent_locks::LockStrength::Update,
    }]).await.unwrap();
    crate::dao::items::transfers::hand_over(&save, &[crate::dao::items::transfers::Transfer {
        item_id: f.old, owner: f.character, location: f.spare, flag: infinity_core::consts::FLAG_SHIP_HANGAR,
    }], &[f.old], &locks, &[]).await.unwrap();
    let capsule = crate::dao::items::creation::positioned(&save, infinity_core::consts::TYPE_CAPSULE, f.character, 1, [4.0; 3]).await.unwrap();
    assert!(select_moved(&save, f.character, f.account, f.old, capsule, 1).await.unwrap());
    let Recovery::Committed { target, old: None } = recovery(&save, &store).await.unwrap() else { panic!("expected committed store") };
    assert_eq!((target.item_id, target.pos), (capsule, [4.0; 3]));
    save.rollback().await.unwrap();
    assert!(board_in_space(&tx, f.character, f.old, f.target, 1, 1, 1, [1.0; 3], [2.0; 3]).await.unwrap());
    let Recovery::Committed { target, old: Some(old) } = recovery(&tx, &board(f.account, 1, 1, 1)).await.unwrap() else { panic!("expected committed boarding") };
    assert_eq!((target.item_id, target.pos, old.item_id, old.pos), (f.target, [2.0; 3], f.old, [1.0; 3]));
    assert!(matches!(recovery(&tx, &store).await.unwrap(), Recovery::Unknown));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn competing_transactions_have_one_winner() {
    let mut first = connect().await;
    let setup = first.transaction().await.unwrap();
    let f = fixture(&setup).await;
    setup.commit().await.unwrap();
    let result = tokio::spawn(async move {
        let tx = first.transaction().await.unwrap();
        assert!(board_in_space(&tx, f.character, f.old, f.target, 1, 1, 1, [1.0; 3], [2.0; 3]).await.unwrap());
        let character = f.character; let old = f.old; let spare = f.spare;
        let (ready, started) = tokio::sync::oneshot::channel();
        let contender = tokio::spawn(async move {
            let mut client = connect().await;
            let tx = client.transaction().await.unwrap();
            ready.send(()).unwrap();
            let won = board_in_space(&tx, character, old, spare, 1, 1, 1, [3.0; 3], [4.0; 3]).await.unwrap();
            tx.rollback().await.unwrap();
            won
        });
        started.await.unwrap();
        let monitor = connect().await;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let waiting: bool = monitor.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT character_id FROM game.characters WHERE character_id=%FOR UPDATE') AS waiting", &[]).await.unwrap().get("waiting");
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("contender did not wait for the character lock");
        tx.commit().await.unwrap();
        assert!(!contender.await.unwrap());
        let check = first.query_one("SELECT ship_id FROM game.characters WHERE character_id=$1", &[&f.character]).await.unwrap();
        assert_eq!(check.get::<_, i64>("ship_id"), f.target);
    }).await;
    let mut first = connect().await;
    let cleanup = first.transaction().await.unwrap();
    cleanup.execute("DELETE FROM game.items WHERE item_id=ANY($1)", &[&&[f.old, f.target, f.spare][..]]).await.unwrap();
    cleanup.execute("DELETE FROM game.characters WHERE user_id=$1", &[&f.account]).await.unwrap();
    cleanup.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&f.account]).await.unwrap();
    cleanup.commit().await.unwrap();
    result.unwrap();
}
