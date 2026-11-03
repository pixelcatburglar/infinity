use super::*;
use crate::dao::store::{purchases, rewards};

async fn grade(db: &Db, user: i64, value: i16) {
    db.execute("UPDATE game.account_clone_grades SET grade=$2, omega_until=NULL, grace_until=NULL WHERE user_id=$1", &[&user, &value]).await.unwrap();
}

async fn cleanup(db: &Db, user: i64) {
    db.execute("DELETE FROM game.login_reward_retro_grants WHERE user_id=$1", &[&user]).await.unwrap();
    db.execute("DELETE FROM game.login_reward_claims WHERE user_id=$1", &[&user]).await.unwrap();
    db.execute("DELETE FROM game.redeem_tokens WHERE user_id=$1", &[&user]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&user]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn seasonal_account_settlement_and_rollback() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let seq: i64 = db.query_one("SELECT nextval('game.accounts_user_id_seq') AS id", &[]).await.unwrap().get("id");
    let user = crate::dao::accounts::authentication::register(&db, &format!("seasonal-test-{seq}"), b"fixture", 0, 0).await.unwrap().user_id;
    let (client, _peer) = crate::services::tests::client(db.clone(), 0).await;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut campaign = Campaign { id: 1, alpha: vec![(101, 1), (102, 1), (103, 1)], omega: vec![(201, 1), (202, 1), (203, 1)], retro_claim: true };
    grade(&db, user, rules::ALPHA).await;
    assert!(claim_reward(&db, &client.world, &campaign, user, sender.clone()).await.unwrap());
    assert_eq!(purchases::tokens(&db, user).await.unwrap().len(), 1);
    assert_eq!(rewards::history(&db, user, campaign.id).await.unwrap()[0].clone_grade, 0);
    assert_eq!(receiver.try_recv().unwrap().0, "OnRedeemingQueueUpdated");
    assert!(!claim_reward(&db, &client.world, &campaign, user, sender.clone()).await.unwrap());
    assert!(receiver.try_recv().is_err());
    grade(&db, user, rules::OMEGA).await;
    let (a, b) = tokio::join!(claim_reward(&db, &client.world, &campaign, user, sender.clone()), claim_reward(&db, &client.world, &campaign, user, sender.clone()));
    assert_ne!(a.unwrap(), b.unwrap());
    assert_eq!(purchases::tokens(&db, user).await.unwrap().iter().map(|t| t.type_id).collect::<Vec<_>>(), vec![101, 201]);
    assert_eq!(receiver.try_recv().unwrap().0, "OnRedeemingQueueUpdated");
    assert!(receiver.try_recv().is_err());
    grade(&db, user, rules::ALPHA).await;
    grade(&db, user, rules::OMEGA).await;
    assert!(!claim_reward(&db, &client.world, &campaign, user, sender.clone()).await.unwrap());
    let audits: i64 = db.query_one("SELECT count(*) AS n FROM game.login_reward_retro_grants WHERE user_id=$1", &[&user]).await.unwrap().get("n");
    assert_eq!(audits, 1);

    // Separate campaign with mixed historical grades: indices follow dates, not Alpha count.
    campaign.id = 2;
    db.execute("INSERT INTO game.login_reward_claims (user_id,campaign_id,day,clone_grade) VALUES ($1,$2,current_date-3,0),($1,$2,current_date-2,1),($1,$2,current_date-1,0)", &[&user, &campaign.id]).await.unwrap();
    assert!(claim_reward(&db, &client.world, &campaign, user, sender.clone()).await.unwrap());
    let types = purchases::tokens(&db, user).await.unwrap().iter().map(|t| t.type_id).collect::<Vec<_>>();
    assert_eq!(types, vec![101, 201, 201, 203]);
    assert!(!claim_reward(&db, &client.world, &campaign, user, sender.clone()).await.unwrap());
    assert!(rewards::history(&db, user, campaign.id).await.unwrap().iter().all(|r| r.clone_grade == 1));

    campaign.id = 3;
    campaign.retro_claim = false;
    db.execute("INSERT INTO game.login_reward_claims (user_id,campaign_id,day,clone_grade) VALUES ($1,$2,current_date-1,0),($1,$2,current_date-2,0),($1,$2,current_date-3,0)", &[&user, &campaign.id]).await.unwrap();
    assert!(!claim_reward(&db, &client.world, &campaign, user, sender.clone()).await.unwrap());
    assert!(rewards::history(&db, user, campaign.id).await.unwrap().iter().all(|r| r.clone_grade == 0));

    campaign.id = 4;
    campaign.retro_claim = true;
    assert!(claim_reward(&db, &client.world, &campaign, user, sender.clone()).await.unwrap());
    assert_eq!(rewards::history(&db, user, campaign.id).await.unwrap()[0].clone_grade, 1);
    assert_eq!(purchases::tokens(&db, user).await.unwrap().len(), 6);

    // The second token cannot fit its database representation; the first grant and claim roll back.
    campaign.id = 5;
    campaign.omega[0].1 = i64::MAX;
    assert!(claim_reward(&db, &client.world, &campaign, user, sender.clone()).await.is_err());
    assert!(rewards::history(&db, user, campaign.id).await.unwrap().is_empty());
    assert_eq!(purchases::tokens(&db, user).await.unwrap().len(), 6);
    // An existing audit key forces an audit INSERT failure after the mark and grant.
    campaign.id = 6;
    campaign.omega[0].1 = 1;
    db.execute("INSERT INTO game.login_reward_claims (user_id,campaign_id,day,clone_grade) VALUES ($1,$2,current_date-1,0),($1,$2,current_date-2,1),($1,$2,current_date-3,1)", &[&user, &campaign.id]).await.unwrap();
    db.execute("INSERT INTO game.login_reward_retro_grants (user_id,campaign_id,day,type_id,quantity,granted_at) VALUES ($1,$2,current_date-1,203,1,0)", &[&user, &campaign.id]).await.unwrap();
    assert!(claim_reward(&db, &client.world, &campaign, user, sender.clone()).await.is_err());
    assert_eq!(rewards::history(&db, user, campaign.id).await.unwrap()[2].clone_grade, 0);
    assert_eq!(purchases::tokens(&db, user).await.unwrap().len(), 6);
    cleanup(&db, user).await;
}
