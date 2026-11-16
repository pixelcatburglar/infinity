//! Real-database coverage for the read-only operator integrity-check script.

use super::*;

const SQL: &str = include_str!("../../../sql/integrity_checks.sql");
type Report = std::collections::BTreeMap<String, Vec<(Option<String>, String)>>;

/// Run the shipped statements verbatim and group the returned rows by check.
async fn report(ex: &(impl crate::db::executor::Executor + ?Sized)) -> Report {
    let mut all = Report::new();
    for statement in SQL.split_terminator(';').map(str::trim)
        .filter(|s| s.lines().any(|l| { let l = l.trim(); !l.is_empty() && !l.starts_with("--") }))
    {
        for row in ex.query(statement, &[]).await.unwrap() {
            all.entry(row.get::<_, String>("check_name")).or_default()
                .push((row.get("primary_key"), row.get("explanation")));
        }
    }
    all
}
/// Expect the named check to report exactly one detail per key plus a summary
/// row carrying the same count, and every other check to stay silent.
fn assert_only(all: &Report, name: &str, mut pks: Vec<String>) {
    pks.sort();
    let rows = all.get(name).unwrap_or_else(|| panic!("{name} did not report"));
    assert_eq!(rows.len(), pks.len() + 1, "{name}");
    assert_eq!(rows[0], (None, format!("total_count={}", pks.len())), "{name}");
    assert_eq!(rows[1..].iter().map(|r| r.0.clone().unwrap()).collect::<Vec<_>>(), pks, "{name}");
    assert!(all.iter().all(|(k, v)| k == name || v.is_empty()), "{name} crossed into {all:?}");
}

/// An allocated-but-never-inserted key; the sequence keeps its value.
async fn missing(tx: &tokio_postgres::Transaction<'_>, seq: &str) -> i64 {
    tx.query_one(&format!("SELECT nextval('game.{seq}') AS id"), &[]).await.unwrap().get("id")
}

async fn case(tx: &tokio_postgres::Transaction<'_>, name: &str, setup: &[String], pks: Vec<String>) {
    tx.batch_execute("SAVEPOINT dirty").await.unwrap();
    for sql in setup { tx.batch_execute(sql).await.unwrap(); }
    assert_only(&report(tx).await, name, pks);
    tx.batch_execute("ROLLBACK TO SAVEPOINT dirty").await.unwrap();
}

const STATION: i64 = 60003760; // Jita 4-4; location below the player-item band.
/// (kind, job_id template, sequence of the target entity) for every accepted
/// entity-job name shape; two rows share kind when a kind has two shapes.
const JOBS: &[(&str, &str, &str)] = &[
    ("skills.complete", "skills.complete:{}", "characters_character_id_seq"),
    ("skill_plans.recover", "skill_plans.recover:{}", "characters_character_id_seq"),
    ("ship_skins.license.expire", "ship_skins.license.expire:{}", "characters_character_id_seq"),
    ("corporations.pending_kick", "corporations.pending_kick:{}", "characters_character_id_seq"),
    ("alpha-injection-available", "alpha-injection-available:{}:1", "characters_character_id_seq"),
    ("subscription.clone_grade.expire", "subscription.clone_grade.expire:{}", "accounts_user_id_seq"),
    ("industry.ready", "industry.ready:{}", "industry_jobs_job_id_seq"),
    ("bounty-payout", "bounty-payout:{}", "killmails_kill_id_seq"),
    ("corp.vote.close", "corp.vote.close:{}", "corp_vote_cases_id_seq"),
    ("corp.sanction.expire", "corp.sanction.expire:{}", "corp_vote_cases_id_seq"),
    ("repairable.payment.expiry", "repairable.payment.expiry:{}", "repairable_recovery_groups_group_id_seq"),
    ("repairable.delivery.arrival", "repairable.delivery.arrival:{}", "repairable_recovery_groups_group_id_seq"),
    ("repairable.withdrawal.expiry", "repairable.withdrawal.expiry:{}", "repairable_recovery_groups_group_id_seq"),
    ("trade.idle_timeout", "trade-timeout:{}", "items_item_id_seq"),
    ("wars.deadline", "wars.deadline:start:{}", "wars_war_id_seq"),
    ("wars.deadline", "wars.deadline:finish:{}", "wars_war_id_seq"),
];

async fn run(db: &Db, fx: crate::services::tests::fixture::Fixture) {
    let mut client = db.pool.get().await.unwrap();
    let tx = client.transaction().await.unwrap();
    let c = fx.character;
    tx.execute("UPDATE game.characters SET solar_system_id=30000142 WHERE character_id=$1", &[&c]).await.unwrap();
    assert!(report(&*tx).await.values().all(Vec::is_empty), "baseline is clean");
    let mut ids = Vec::new();
    for _ in 0..12 { ids.push(missing(&tx, "items_item_id_seq").await); }
    let item = |k: usize| ids[k];
    case(&tx, "item_parent_missing", &[format!("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id) VALUES ({},1,{c},{},5)", item(0), item(1))], vec![item(0).to_string()]).await;
    case(&tx, "item_location_cycle", &[format!("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id) VALUES ({},1,{c},{},5)", item(2), item(2))], vec![item(2).to_string()]).await;
    case(&tx, "item_owner_missing", &[format!("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id) VALUES ({},1,{},{STATION},5)", item(3), item(4))], vec![item(3).to_string()]).await;
    case(&tx, "item_quantity_shape", &[
        format!("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ({},1,{c},{STATION},5,2,true)", item(5)),
        format!("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity) VALUES ({},1,{c},{STATION},5,-1)", item(6))],
        vec![item(5).to_string(), item(6).to_string()]).await;
    case(&tx, "module_charge_ship_missing", &[format!("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ({},11,1,1)", item(7))], vec![format!("{}:11", item(7))]).await;
    case(&tx, "weapon_bank_ship_missing", &[format!("INSERT INTO game.weapon_banks (ship_id,master_id,slave_id) VALUES ({},{},{})", item(8), item(9), item(10))], vec![item(10).to_string()]).await;
    case(&tx, "insurance_ship_missing", &[format!("INSERT INTO game.insurance (ship_id,owner_id,type_id,fraction,start_date,end_date) VALUES ({},{c},1,0.5,1,2)", item(11))], vec![item(11).to_string()]).await;
    for (n, &(kind, name, seq)) in JOBS.iter().enumerate() {
        let job_id = name.replace("{}", &missing(&tx, seq).await.to_string());
        tx.batch_execute("SAVEPOINT dirty").await.unwrap();
        tx.execute("INSERT INTO game.jobs (job_id,kind,due_at,state,created_at,updated_at) VALUES ($1,$2,0,$3,0,0)",
            &[&job_id, &kind, &["scheduled", "running", "dead"][n % 3]]).await.unwrap();
        assert_only(&report(&*tx).await, "entity_job_target_missing", vec![job_id]);
        tx.batch_execute("ROLLBACK TO SAVEPOINT dirty").await.unwrap();
    }
    case(&tx, "character_ship_invalid", &[format!("UPDATE game.characters SET ship_id={} WHERE character_id={c}", item(0))], vec![c.to_string()]).await;
    case(&tx, "character_ship_invalid", &[format!("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,singleton) VALUES ({},606,1,{STATION},5,true)", item(1)),
        format!("UPDATE game.characters SET ship_id={} WHERE character_id={c}", item(1))], vec![c.to_string()]).await;
    case(&tx, "character_system_missing", &[format!("UPDATE game.characters SET solar_system_id=NULL WHERE character_id={c}")], vec![c.to_string()]).await;
    tx.batch_execute("SAVEPOINT legal").await.unwrap();
    let dead: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,solar_system_id,deleted) VALUES ($1,'gone',1,1,1,1,30000142,true) RETURNING character_id", &[&fx.account]).await.unwrap().get("character_id");
    tx.execute("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES (1,$1,$2,5,-1,true),(1,$1,$2,5,-2,true),(3468,$1,$2,5,-5,true),(1,$1,$2,5,0,false),(1,1,$2,5,1,false),(1,$1,-7,5,1,false)", &[&dead, &STATION]).await.unwrap();
    tx.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id,solar_system_id) VALUES (200000000,$1,'brain',1,1,1,1,30000142)", &[&fx.account]).await.unwrap();
    tx.execute("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,brain_slot) VALUES (1,200000000,200000000,88,1)", &[]).await.unwrap();
    let trade: i64 = tx.query_one("INSERT INTO game.trades (station_id,char_a,char_b,state,created_at,updated_at) VALUES (60003760,$1,$1,'open',0,0) RETURNING trade_id", &[&c]).await.unwrap().get("trade_id");
    let member = missing(&tx, "repairable_recovery_groups_group_id_seq").await;
    tx.execute(&format!("INSERT INTO game.items (type_id,owner_id,location_id,flag_id) VALUES (1,{c},{trade},5),(1,{c},{member},5)"), &[]).await.unwrap();
    for (table, col) in [("npc_characters", "character_id"), ("crp_npc_corporations", "corporation_id"), ("chr_factions", "faction_id")] {
        let owner: i64 = tx.query_one(&format!("SELECT {col}::bigint AS id FROM sde.{table} LIMIT 1"), &[]).await.unwrap().get("id");
        tx.execute("INSERT INTO game.items (type_id,owner_id,location_id,flag_id) VALUES (1,$1,$2,5)", &[&owner, &STATION]).await.unwrap();
    }
    let ship = crate::dao::items::creation::rookie_ship(&*tx, 606, c, STATION, 4).await.unwrap();
    tx.execute(&format!("UPDATE game.characters SET ship_id={ship} WHERE character_id={c}"), &[]).await.unwrap();
    let ghost = missing(&tx, "characters_character_id_seq").await;
    for (job_id, kind) in [
        (format!("skills.complete:{c}"), "skills.complete"),
        (format!("subscription.clone_grade.expire:{}", fx.account), "subscription.clone_grade.expire"),
        (format!("alpha-injection-available:{c}:{ghost}"), "alpha-injection-available"),
        (format!("trade-timeout:{trade}"), "trade.idle_timeout"),
        (format!("wars.deadline:invite:{c}:{c}"), "wars.deadline"),
        ("kill-right-expiry:1".to_string(), "kill-right-expiry"),
        ("periodic:market.expire".to_string(), "periodic:market.expire"),
    ] {
        tx.execute("INSERT INTO game.jobs (job_id,kind,due_at,state,created_at,updated_at) VALUES ($1,$2,0,'scheduled',0,0)", &[&job_id, &kind]).await.unwrap();
    }
    assert!(report(&*tx).await.values().all(Vec::is_empty), "legal shapes stay silent");
    tx.batch_execute("ROLLBACK TO SAVEPOINT legal").await.unwrap();
    tx.batch_execute("SAVEPOINT cap").await.unwrap();
    tx.batch_execute("INSERT INTO game.insurance (ship_id,owner_id,type_id,fraction,start_date,end_date) SELECT nextval('game.items_item_id_seq'),1,1,0.5,1,2 FROM generate_series(1,101)").await.unwrap();
    let all = report(&*tx).await;
    let rows = all.get("insurance_ship_missing").unwrap();
    assert_eq!(rows.len(), 101);
    assert_eq!(rows[0], (None, "total_count=101".to_string()));
    assert!(rows[1..].windows(2).all(|w| w[0].0 < w[1].0), "details sorted by key");
    assert!(all.iter().all(|(k, v)| k == "insurance_ship_missing" || v.is_empty()));
    tx.batch_execute("ROLLBACK TO SAVEPOINT cap").await.unwrap();
    assert!(report(&*tx).await.values().all(Vec::is_empty), "rollback restores the clean baseline");
}

#[tokio::test]
#[ignore = "requires a dedicated initialized PostgreSQL database; run serially"]
async fn checks_report_only_injected_violations() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let result = crate::tests::catch_scenario(run(&db, fixture)).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
