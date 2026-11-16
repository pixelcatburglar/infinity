use super::*;

#[test]
fn deadline_jobs_keep_distinct_immutable_member_deadlines() {
    let member = RecoveryItemId::new(infinity_core::consts::RECOVERY_ID_FIRST).unwrap();
    let jobs = [payment_job(member, 11), delivery_job(member, 22), withdraw_job(member, 33)];
    assert!(jobs[0].id != jobs[1].id && jobs[1].id != jobs[2].id);
    for (job, expected) in jobs.iter().zip([11, 22, 33]) {
        let payload: Payload = serde_json::from_str(&job.payload).unwrap();
        assert_eq!((payload.member, payload.deadline, job.due_at), (member.get(), expected, expected));
    }
    assert_eq!(jobs[0].id, format!("repairable.payment.expiry:{}", member.get()));
    assert_eq!(job_id(DELIVERY, member), jobs[1].id);
    assert_eq!(job_id(WITHDRAW, member), format!("repairable.withdrawal.expiry:{}", member.get()));
    assert_eq!(job_id(PAYMENT, member), format!("{}:{}", jobs[0].kind, member.get()));
    assert_eq!(job_id(&jobs[0].kind, member), jobs[0].id);
    assert_eq!(job_id(PAYMENT, member), format!("{PAYMENT}:{}", member.get()));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn deadline_due_time_survives_reconnection() {
    let cfg = crate::config::tests::test_database();
    let db = Db::connect(&cfg).await.unwrap();
    let raw: i64 = db.query_one("SELECT nextval('game.repairable_recovery_groups_group_id_seq') AS id", &[]).await.unwrap().get("id");
    let member = RecoveryItemId::new(raw).unwrap();
    let due = filetime_now() + infinity_core::consts::FILETIME_DAY;
    let job = withdraw_job(member, due);
    scheduler::schedule(&db, &job).await.unwrap();
    drop(db);
    let reopened = Db::connect(&cfg).await.unwrap();
    let row = reopened.query_one("SELECT due_at,payload FROM game.jobs WHERE job_id=$1", &[&job.id]).await.unwrap();
    let saved: Payload = serde_json::from_str(row.get("payload")).unwrap();
    let persisted = (row.get::<_, i64>("due_at"), saved.deadline, saved.member);
    scheduler::cancel(&reopened, &job.id).await.unwrap();
    assert_eq!(persisted, (due, due, member.get()));
}
