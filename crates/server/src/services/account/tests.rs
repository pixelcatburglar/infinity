use super::*;
mod journal;
mod permissions;
mod wallet_take;
mod exact_amounts;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn alliance_recipient_is_rejected_without_spending() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let target = 99_500_123_i64;
    let prior_characters: i64 = db.query_one("SELECT count(*) AS count FROM game.characters WHERE character_id = $1", &[&target]).await.unwrap().get("count");
    let prior_wallets: i64 = db.query_one("SELECT count(*) AS count FROM game.corp_wallets WHERE corporation_id = $1", &[&target]).await.unwrap().get("count");
    assert_eq!((prior_characters, prior_wallets), (0, 0), "alliance-range target must be unused");
    let sender = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        db.execute("UPDATE game.characters SET balance=100 WHERE character_id=$1", &[&sender.character]).await.unwrap();
        let (mut c, _peer) = crate::services::tests::client(db.clone(), sender.character).await;
        let kwargs = Value::Dict(vec![]);
        let args = [Value::Int(target), Value::Float(10.0)];
        let call = Call { service: "account", method: "GiveCash", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let CallError::User { msg, dict } = crate::services::dispatch(&mut c, &call).await.unwrap_err() else { panic!("expected recipient refusal") };
        assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
        assert_eq!(dict, vec![("notify".into(), Value::unicode(messages::TRANSFER_RECIPIENT_NOT_PLAYER))]);
        assert_eq!(super::super::wallets::balance(&db, sender.character).await.unwrap(), IskAmount::from_cents(10_000).unwrap());
        let wallets: i64 = db.query_one("SELECT count(*) AS count FROM game.corp_wallets WHERE corporation_id = $1", &[&target]).await.unwrap().get("count");
        assert_eq!(wallets, 0);
    }).await;
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        crate::services::tests::fixture::cleanup_progress(tx, &[target], &[]).await?;
        tx.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&target, &ACCOUNT_CASH]).await?;
        Ok::<(), crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, sender).await.unwrap();
    result.unwrap();
}
