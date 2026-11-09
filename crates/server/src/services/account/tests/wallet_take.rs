use super::super::*;
use infinity_core::consts::{ROLE_ACCOUNTANT, ROLE_ACCOUNT_CAN_TAKE_1, ROLE_DIRECTOR, ROLE_JUNIOR_ACCOUNTANT};
use crate::services::tests::{self, fixture};

async fn fingerprint(db: &crate::db::Db, corp: i64, ceo: i64, member: i64, recipient: i64) -> Vec<String> {
    let owners = vec![corp, recipient];
    let subjects = vec![ceo, member, recipient];
    let item_owners = vec![corp, ceo, member, recipient];
    let corporations = vec![corp];
    let mut result = Vec::new();
    for (sql, ids) in [
        ("SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY corporation_id,account_key), '')) AS fingerprint FROM game.corp_wallets t WHERE corporation_id=ANY($1)", &corporations),
        ("SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY character_id), '')) AS fingerprint FROM game.characters t WHERE character_id=ANY($1)", &subjects),
        ("SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY account_id), '')) AS fingerprint FROM game.wallet_accounts t WHERE owner_id=ANY($1)", &owners),
        ("SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id), '')) AS fingerprint FROM game.items t WHERE owner_id=ANY($1)", &item_owners),
        ("SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY journal_id), '')) AS fingerprint FROM game.wallet_journal t WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=ANY($1))", &owners),
    ] { result.push(db.query_one(sql, &[ids]).await.unwrap().get("fingerprint")); }
    for sql in [
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY event_id), '')) AS fingerprint FROM game.progress_events t WHERE subject_character_id=ANY($2) OR (kind='WalletChanged' AND (payload->'WalletChanged'->>'owner_id')::bigint=ANY($1))",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY consumer_key,event_id), '')) AS fingerprint FROM game.progress_deliveries t WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE subject_character_id=ANY($2) OR (kind='WalletChanged' AND (payload->'WalletChanged'->>'owner_id')::bigint=ANY($1)))",
    ] { result.push(db.query_one(sql, &[&owners, &subjects]).await.unwrap().get("fingerprint")); }
    result
}

fn assert_withdrawal_denied(error: CallError) {
    let CallError::User { msg, dict } = error else { panic!("expected withdrawal denial") };
    assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
    assert_eq!(dict, vec![("notify".into(), Value::unicode(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED))]);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn accountant_withdrawal_and_selection_require_selected_take() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let recipient = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 0).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR).await.unwrap();
        fixture::assign_corporation(&db, member.character, corp, ROLE_ACCOUNTANT).await.unwrap();
        for key in [ACCOUNT_CASH, ACCOUNT_CASH + 1] {
            db.execute("INSERT INTO game.corp_wallets(corporation_id,account_key,balance) VALUES ($1,$2,100)", &[&corp, &key]).await.unwrap();
        }
        db.execute("UPDATE game.characters SET balance=0 WHERE character_id=$1", &[&recipient.character]).await.unwrap();
        let (mut client, _peer) = tests::client(db.clone(), member.character).await;
        client.session.apply(vec![("corpid", Value::Int(corp)), ("corpAccountKey", Value::Int(ACCOUNT_CASH.into()))]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(member.character, tx);
        let kwargs = Value::Dict(vec![]);
        let selected = ACCOUNT_CASH + 1;
        for role in [ROLE_ACCOUNTANT, ROLE_JUNIOR_ACCOUNTANT, ROLE_ACCOUNTANT | ROLE_ACCOUNT_CAN_TAKE_1] {
            fixture::assign_corporation(&db, member.character, corp, role).await.unwrap();
            let read = [Value::Int(1), Value::Int(selected.into())];
            let call = Call { service: "account", method: "GetCashBalance", args: &read, kwargs: &kwargs, bound: false, bind_params: None };
            assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::Float(100.0));
            let before = fingerprint(&db, corp, ceo.character, member.character, recipient.character).await;
            let args = [Value::Int(recipient.character), Value::Float(10.0), Value::Int(selected.into())];
            let call = Call { service: "account", method: "GiveCashFromCorpAccount", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            assert_withdrawal_denied(crate::services::dispatch(&mut client, &call).await.unwrap_err());
            let args = [Value::Int(selected.into())];
            let call = Call { service: "corpRegistry", method: "SetAccountKey", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            assert_withdrawal_denied(crate::services::dispatch(&mut client, &call).await.unwrap_err());
            assert_eq!(client.session.int("corpAccountKey"), Some(i64::from(ACCOUNT_CASH)));
            assert_eq!(fingerprint(&db, corp, ceo.character, member.character, recipient.character).await, before);
            assert!(matches!(rx.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
            assert!(client.outbox.is_empty());
        }
        fixture::assign_corporation(&db, member.character, corp, 0).await.unwrap();
        let args = [Value::Int(1), Value::Int(selected.into())];
        let call = Call { service: "account", method: "GetCashBalance", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let CallError::User { msg, dict } = crate::services::dispatch(&mut client, &call).await.unwrap_err() else { panic!("expected read denial") };
        assert_eq!(msg, messages::MSG_CRP_ACCESS_DENIED);
        assert_eq!(dict, vec![("reason".into(), Value::tuple([Value::Int(UE_LOC), Value::str(messages::account::NO_WALLET_DIVISION_ACCESS)]))]);
        fixture::assign_corporation(&db, member.character, corp, ROLE_ACCOUNTANT | (ROLE_ACCOUNT_CAN_TAKE_1 << 1)).await.unwrap();
        let args = [Value::Int(selected.into())];
        let call = Call { service: "corpRegistry", method: "SetAccountKey", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
        assert_eq!(client.session.int("corpAccountKey"), Some(i64::from(selected)));
        let args = [Value::Int(recipient.character), Value::Float(10.0), Value::Int(selected.into())];
        let call = Call { service: "account", method: "GiveCashFromCorpAccount", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
        assert_eq!(corp_balance(&db, corp, selected).await.unwrap(), IskAmount::from_cents(9000).unwrap());
        assert_eq!(crate::services::wallets::balance(&db, recipient.character).await.unwrap(), IskAmount::from_cents(1000).unwrap());
        fixture::assign_corporation(&db, member.character, corp, ROLE_ACCOUNTANT).await.unwrap();
        let title_roles = ROLE_ACCOUNT_CAN_TAKE_1 << 1;
        db.execute("INSERT INTO game.corp_titles(corporation_id,title_id,name,roles) VALUES ($1,1,'wallet', $2)", &[&corp, &title_roles]).await.unwrap();
        db.execute("UPDATE game.characters SET corp_title_mask=1,corp_block_roles=false WHERE character_id=$1", &[&member.character]).await.unwrap();
        let args = [Value::Int(ACCOUNT_CASH.into())];
        client.session.apply(vec![("corpAccountKey", args[0].clone())]);
        let args = [Value::Int(selected.into())];
        let call = Call { service: "corpRegistry", method: "SetAccountKey", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
        assert_eq!(client.session.int("corpAccountKey"), Some(i64::from(selected)));
        let args = [Value::Int(recipient.character), Value::Float(10.0), Value::Int(selected.into())];
        let call = Call { service: "account", method: "GiveCashFromCorpAccount", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
        assert_eq!(corp_balance(&db, corp, selected).await.unwrap(), IskAmount::from_cents(8000).unwrap());
        assert_eq!(crate::services::wallets::balance(&db, recipient.character).await.unwrap(), IskAmount::from_cents(2000).unwrap());
        db.execute("UPDATE game.characters SET corp_title_mask=0 WHERE character_id=$1", &[&member.character]).await.unwrap();
        db.execute("DELETE FROM game.corp_titles WHERE corporation_id=$1", &[&corp]).await.unwrap();
        fixture::assign_corporation(&db, member.character, corp, ROLE_ACCOUNTANT | (ROLE_ACCOUNT_CAN_TAKE_1 << 1)).await.unwrap();
        loop {
            match rx.try_recv() {
                Ok(_) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => panic!("pilot notification channel disconnected"),
            }
        }
        client.outbox.clear();
        let before = fingerprint(&db, corp, ceo.character, member.character, recipient.character).await;
        let args = [Value::Int(recipient.character), Value::Float(10.0), Value::Int(ACCOUNT_CASH.into())];
        let call = Call { service: "account", method: "GiveCashFromCorpAccount", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_withdrawal_denied(crate::services::dispatch(&mut client, &call).await.unwrap_err());
        let args = [Value::Int(ACCOUNT_CASH.into())];
        let call = Call { service: "corpRegistry", method: "SetAccountKey", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_withdrawal_denied(crate::services::dispatch(&mut client, &call).await.unwrap_err());
        assert_eq!(client.session.int("corpAccountKey"), Some(i64::from(selected)));
        assert_eq!(fingerprint(&db, corp, ceo.character, member.character, recipient.character).await, before);
        assert!(matches!(rx.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
        assert!(client.outbox.is_empty());
        fixture::assign_corporation(&db, member.character, corp, ROLE_DIRECTOR).await.unwrap();
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
        let args = [Value::Int(recipient.character), Value::Float(10.0), Value::Int(ACCOUNT_CASH.into())];
        let call = Call { service: "account", method: "GiveCashFromCorpAccount", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::None);
        assert_eq!(corp_balance(&db, corp, ACCOUNT_CASH).await.unwrap(), IskAmount::from_cents(9000).unwrap());
        client.world.unregister_pilot_generation(member.character, None);
    }).await;
    db.execute("UPDATE game.characters SET corp_title_mask=0,corp_block_roles=false WHERE character_id=$1", &[&member.character]).await.unwrap();
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        fixture::cleanup_progress(tx, &[corp, recipient.character], &[member.character, recipient.character]).await
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=ANY($1))", &[&vec![corp, recipient.character]]).await.unwrap();
    db.execute("DELETE FROM game.wallet_accounts WHERE owner_id=ANY($1)", &[&vec![corp, recipient.character]]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup_corporation(&db, corp, &[ceo.character, member.character], 0).await.unwrap();
    for person in [ceo, member, recipient] { fixture::cleanup(&db, person).await.unwrap(); }
    result.unwrap();
}
