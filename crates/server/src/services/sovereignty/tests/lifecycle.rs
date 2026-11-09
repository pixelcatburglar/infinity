//! Physical Hub custody uses the actual inventory row and preserves placement.
use crate::dao::items::{loading::Item, sovereignty};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn placement_guards_the_source_and_preserves_position_on_transfer() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let id: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES (1,2,3,4,1,false) RETURNING item_id", &[]).await.unwrap().get("item_id");
    let item = Item { item_id: id, type_id: 1, owner_id: 2, location_id: 3, flag_id: 4, quantity: 1, singleton: false, group_id: 0, category_id: 0, custom_info: None };
    let mut stale = item.clone();
    stale.owner_id = 8;
    assert!(!sovereignty::place(&tx, &stale, 5, 6, [1.0, 2.0, 3.0]).await.unwrap());
    assert!(!sovereignty::place(&tx, &item, 5, 6, [f64::NAN, 2.0, 3.0]).await.unwrap());
    assert!(sovereignty::place(&tx, &item, 5, 6, [1.0, 2.0, 3.0]).await.unwrap());
    assert!(!sovereignty::place(&tx, &item, 5, 6, [1.0, 2.0, 3.0]).await.unwrap());
    assert!(sovereignty::transfer_owner(&tx, id, 5, 1, 6, 7).await.unwrap());
    let row = tx.query_one("SELECT owner_id,location_id,flag_id,singleton,x,y,z FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
    assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id"), row.get::<_, i32>("flag_id"), row.get::<_, bool>("singleton")), (7, 5, 0, true));
    assert_eq!((row.get::<_, f64>("x"), row.get::<_, f64>("y"), row.get::<_, f64>("z")), (1.0, 2.0, 3.0));
    tx.rollback().await.unwrap();
}

#[test]
fn transfer_projection_preserves_current_capital_without_change() {
    use crate::{dao::sovereignty::{policy::PolicyRow, structures::{HubRow, StructureRow}, systems::SystemRow}, services::sovereignty::{CapitalPublication, CommittedPublication, HubPublication}};
    let system = SystemRow { system_id: 42, hub_item_id: Some(1), claim_corporation_id: Some(8), alliance_id: Some(9), claim_time: Some(1), ownership_epoch: 1, revision: 2 };
    let policy = PolicyRow { alliance_id: 9, current_capital_system: Some(42), last_capital_changed_at: Some(1), capital_lost_at: None, capital_change_not_before: None, revision: 1 };
    let hub = HubRow { system: system.clone(), structure: StructureRow { item_id: 1, system_id: 42, source_item_id: 2, deployer_character_id: 3, deployer_corporation_id: 8, deployed_at: 1, content_namespace: "test".into(), content_version: "test".into(), self_destruct_due_at: None, self_destruct_actor: None }, type_id: 1, owner_id: 8, x: None, y: None, z: None };
    let publication = CommittedPublication {
        hubs: vec![HubPublication { system, hub: Some(hub), capture: None }],
        capitals: vec![CapitalPublication::new(policy, Some(42))],
        ..Default::default()
    };
    let hub = &publication.hubs[0];
    let capital = &publication.capitals[0];
    assert_eq!(hub.system.alliance_id, Some(capital.policy.alliance_id));
    assert_eq!(capital.policy.current_capital_system, Some(hub.system.system_id));
    assert!(!capital.changed);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn historical_contributors_keep_org_proofs_but_claimant_must_be_in_proven_ship() {
    use crate::{dao::{characters::ship, corporations::registry, sovereignty::sources},
        services::sovereignty::{authority::validate_contributors, lifecycle::{claimant_location_matches, endpoint_contributor}, CaptureContribution, CaptureContributor, CaptureObservation}};
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let user: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('sov-proof-' || nextval('game.accounts_user_id_seq'),$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let mut characters = Vec::new();
    for _ in 0..2 {
        let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'sov-proof-' || nextval('game.characters_character_id_seq'),1,1,1,1) RETURNING character_id", &[&user]).await.unwrap().get("character_id");
        characters.push(character);
    }
    let corp: i64 = tx.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('sov-proof-' || nextval('game.corporations_corporation_id_seq'),'P' || currval('game.corporations_corporation_id_seq'),$1,$1,1,1) RETURNING corporation_id", &[&characters[0]]).await.unwrap().get("corporation_id");
    let alliance: i64 = tx.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('sov-proof-' || nextval('game.alliances_alliance_id_seq'),'P' || currval('game.alliances_alliance_id_seq'),$1,$1,$2,1) RETURNING alliance_id", &[&corp, &characters[0]]).await.unwrap().get("alliance_id");
    tx.execute("UPDATE game.corporations SET alliance_id=$1 WHERE corporation_id=$2", &[&alliance, &corp]).await.unwrap();
    tx.execute("UPDATE game.characters SET corporation_id=$1,ship_id=10,solar_system_id=20,station_id=NULL WHERE character_id=ANY($2)", &[&corp, &characters]).await.unwrap();
    assert!(registry::lock_admission(&tx, &[alliance], &[corp], &characters).await.unwrap());
    let mut contributors = Vec::new();
    for &character in &characters {
        let version = sources::ensure(&tx, character).await.unwrap();
        contributors.push(CaptureContribution { contributor: CaptureContributor { character_id: character, ship_id: 10, module_id: 11, corporation_id: corp, alliance_id: alliance, source_revision: version.revision }, through: 20 });
    }
    tx.execute("UPDATE game.characters SET ship_id=12,solar_system_id=21 WHERE character_id=$1", &[&characters[0]]).await.unwrap();
    let result = validate_contributors(&tx, &contributors, 20).await.unwrap();
    assert!(result.valid && result.changes.is_empty());
    let historical_location = ship::current(&tx, characters[0]).await.unwrap().unwrap();
    assert!(!claimant_location_matches(&historical_location, &contributors[0].contributor, 20));
    let claimant_location = ship::current(&tx, characters[1]).await.unwrap().unwrap();
    assert!(claimant_location_matches(&claimant_location, &contributors[1].contributor, 20));
    tx.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&characters[0]]).await.unwrap();
    assert!(!validate_contributors(&tx, &contributors, 20).await.unwrap().valid);
    tx.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&corp, &characters[0]]).await.unwrap();
    sources::invalidate(&tx, &[characters[0]], 10).await.unwrap();
    contributors.push(contributors[0].clone());
    let result = validate_contributors(&tx, &contributors, 20).await.unwrap();
    assert!(!result.valid);
    assert_eq!(result.changes.len(), 1);
    assert_eq!(result.changes[0].effective_at, 10);
    contributors[0].through = 10;
    contributors[2].through = 10;
    assert!(validate_contributors(&tx, &contributors, 20).await.unwrap().valid);
    assert!(!validate_contributors(&tx, &contributors, 9).await.unwrap().valid);
    let corp_b: i64 = tx.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('sov-proof-' || nextval('game.corporations_corporation_id_seq'),'P' || currval('game.corporations_corporation_id_seq'),$1,$1,1,1) RETURNING corporation_id", &[&characters[0]]).await.unwrap().get("corporation_id");
    tx.execute("UPDATE game.corporations SET alliance_id=$1 WHERE corporation_id=$2", &[&alliance, &corp_b]).await.unwrap();
    tx.execute("UPDATE game.characters SET corporation_id=$1,ship_id=10,solar_system_id=20 WHERE character_id=$2", &[&corp_b, &characters[0]]).await.unwrap();
    let current = CaptureContributor { corporation_id: corp_b, source_revision: 1, ..contributors[0].contributor.clone() };
    contributors.push(CaptureContribution { contributor: current.clone(), through: 20 });
    assert!(validate_contributors(&tx, &contributors, 20).await.unwrap().valid);
    let location = ship::current(&tx, characters[0]).await.unwrap().unwrap();
    assert!(claimant_location_matches(&location, &current, 20));
    assert!(!claimant_location_matches(&location, &contributors[0].contributor, 20));
    let target = crate::dao::sovereignty::capture::CaptureRow { target_item_id: 1, system_id: 20, ownership_epoch: 0, controlling_team: Some(alliance), scoring_team: Some(alliance), defender_team: 1, score: 1.0, rate: 1.0, blocked: false, score_sim_time: 20, checkpoint_wall_time: 20, revision: 0, capture_time_seconds: 1.0, defensive_regen_rate: 0.0, checkpoint_interval_ms: 1, content_namespace: "test".into(), content_version: "test".into() };
    let mut observation = CaptureObservation { target, system_revision: 0, contributors: contributors.clone(), claimant: Some(current.clone()) };
    assert_eq!(endpoint_contributor(&observation, Some(alliance)), Some(&current));
    observation.claimant = Some(contributors[0].contributor.clone());
    assert!(endpoint_contributor(&observation, Some(alliance)).is_none());
    contributors[0].contributor.source_revision = 2;
    assert!(!validate_contributors(&tx, &contributors, 20).await.unwrap().valid);
    contributors[0].contributor.source_revision = 0;
    tx.execute("DELETE FROM game.sov_link_source_changes WHERE character_id=$1", &[&characters[0]]).await.unwrap();
    assert!(!validate_contributors(&tx, &contributors, 20).await.unwrap().valid);
    tx.rollback().await.unwrap();
}
