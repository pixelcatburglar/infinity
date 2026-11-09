use super::super::*;
use infinity_core::consts::CLIENT_MAX_ISK;

#[test]
fn journal_wire_fields_and_dates() {
    let row = journal_row(Row { transaction_id: 1, date: 2, entry_type_id: 2, owner1: 3, owner2: 4, reference_id: 5, amount: IskAmount::from_cents(-600).unwrap(), balance: "7.00".into(), description: "actual".into() }, personal_client_cap()).unwrap();
    let decoded = marshal::loads(&marshal::dumps(&row)).unwrap();
    for (name, expected) in [("transactionID", Value::Int(1)), ("transactionDate", Value::Int(2)), ("entryTypeID", Value::Int(2)), ("ownerID1", Value::Int(3)), ("ownerID2", Value::Int(4)), ("referenceID", Value::Int(5)), ("amount", Value::Float(-6.0)), ("balance", Value::Float(7.0)), ("description", Value::unicode("actual"))] {
        assert_eq!(super::super::super::value_field(&decoded, name), Some(&expected));
    }
    assert!(matches!(period(Some(&Value::None), Some(&Value::None)), Ok(Period::Since(_))));
    for (year, month) in [(Value::None, Value::Int(2)), (Value::Int(2024), Value::None), (Value::Int(1600), Value::Int(1)), (Value::Int(10000), Value::Int(1)), (Value::Int(2024), Value::Int(13)), (Value::str("2024"), Value::Int(2))] {
        assert!(period(Some(&year), Some(&month)).is_err());
    }
    assert!(matches!(period(Some(&Value::Int(2024)), Some(&Value::Int(2))), Ok(Period::Month { year: 2024, month: 2 })));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn entry_types_and_personal_journal_dispatch() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let other = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let account = crate::dao::wallets::accounts::ensure(&db, fixture.character, ACCOUNT_CASH).await.unwrap();
    let other_account = crate::dao::wallets::accounts::ensure(&db, other.character, ACCOUNT_CASH).await.unwrap();
    let now = filetime_now();
    for (id, owner) in [(account.id, fixture.character), (other_account.id, other.character)] {
        journal::append(&db, id, IskAmount::from_cents(100).unwrap(), &journal::Entry { actor_id: owner, entry_type_id: infinity_core::consts::REF_MARKET_TRANSACTION, owner_id1: owner, owner_id2: 42, reference_id: 55, created_at: now, description: "actual", market_transaction_id: None, order_id: None, side: 1 }).await.unwrap();
    }
    let (mut c, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "account", method: "GetEntryTypes", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    let types = crate::services::dispatch(&mut c, &call).await.unwrap();
    let entries = types.items().unwrap();
    for e in c.sde.accounting.iter() {
        assert!(entries.iter().any(|v| super::super::super::value_field(v, "entryTypeID") == Some(&Value::Int(e.id.into())) && super::super::super::value_field(v, "entryTypeNameID") == Some(&Value::Int(e.name_id.into()))));
    }
    let args = [Value::Int(ACCOUNT_CASH.into()), Value::None, Value::None, Value::Bool(false)];
    let call = Call { service: "account", method: "GetTransactions", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    let result = crate::services::dispatch(&mut c, &call).await.unwrap();
    let decoded = marshal::loads(&marshal::dumps(&result)).unwrap();
    let rows = decoded.items().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(super::super::super::value_field(&rows[0], "ownerID1"), Some(&Value::Int(fixture.character)));
    let entry_id = super::super::super::value_field(&rows[0], "entryTypeID").unwrap();
    assert!(entries.iter().any(|v| super::super::super::value_field(v, "entryTypeID") == Some(entry_id)));
    let bad = [Value::Int((ACCOUNT_CASH + DIVISIONS).into()), Value::None, Value::None, Value::Bool(false)];
    let call = Call { args: &bad, ..call };
    assert!(crate::services::dispatch(&mut c, &call).await.is_err());
    }).await;
    let owners = vec![fixture.character, other.character];
    db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id = ANY($1))", &[&owners]).await.unwrap();
    db.execute("DELETE FROM game.wallet_accounts WHERE owner_id = ANY($1)", &[&owners]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, other).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn journal_dispatch_uses_personal_and_corporate_caps() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = crate::services::tests::fixture::create(&db).await.unwrap();
    let corp = crate::services::tests::fixture::create_corporation(&db, ceo.character, 0).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        crate::services::tests::fixture::assign_corporation(&db, ceo.character, corp, infinity_core::consts::ROLE_DIRECTOR).await.unwrap();
        db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,5000000000000000000) ON CONFLICT (corporation_id,account_key) DO UPDATE SET balance=EXCLUDED.balance", &[&corp, &ACCOUNT_CASH]).await.unwrap();
        db.execute("UPDATE game.characters SET balance=5000000000000000000 WHERE character_id=$1", &[&ceo.character]).await.unwrap();
        for owner in [ceo.character, corp] {
            let account = crate::dao::wallets::accounts::ensure(&db, owner, ACCOUNT_CASH).await.unwrap();
            journal::append(&db, account.id, IskAmount::parse_decimal("5000000000000000000").unwrap(), &journal::Entry { actor_id: ceo.character, entry_type_id: infinity_core::consts::REF_MARKET_TRANSACTION, owner_id1: owner, owner_id2: 42, reference_id: 55, created_at: filetime_now(), description: "", market_transaction_id: None, order_id: None, side: 1 }).await.unwrap();
        }
        let (mut c, _peer) = crate::services::tests::client(db.clone(), ceo.character).await;
        c.session.apply(vec![("corpid", Value::Int(corp))]);
        let kwargs = Value::Dict(vec![]);
        for (is_corp, expected) in [(false, CLIENT_MAX_ISK), (true, 5e18)] {
            let args = [Value::Int(ACCOUNT_CASH.into()), Value::None, Value::None, Value::Bool(is_corp)];
            let call = Call { service: "account", method: "GetTransactions", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let value = super::super::dispatch(&mut c, &call).await.unwrap().unwrap();
            let decoded = marshal::loads(&marshal::dumps(&value)).unwrap();
            let rows = decoded.items().unwrap();
            assert_eq!(rows.len(), 1);
            for field in ["amount", "balance"] {
                assert_eq!(super::super::super::value_field(&rows[0], field), Some(&Value::Float(expected)));
            }
        }
    }).await;
    let owners = vec![ceo.character, corp];
    db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=ANY($1))", &[&owners]).await.unwrap();
    db.execute("DELETE FROM game.wallet_accounts WHERE owner_id=ANY($1)", &[&owners]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await.unwrap();
    crate::services::tests::fixture::cleanup_corporation(&db, corp, &[ceo.character], 0).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, ceo).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn transfer_success_and_refusals_preserve_balances_without_journal() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let a = crate::services::tests::fixture::create(&db).await.unwrap();
    let b = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    db.execute("UPDATE game.characters SET balance=100 WHERE character_id=$1", &[&a.character]).await.unwrap();
    db.execute("UPDATE game.characters SET balance=0 WHERE character_id=$1", &[&b.character]).await.unwrap();
    let (mut c, _peer) = crate::services::tests::client(db.clone(), a.character).await;
    let kwargs = Value::Dict(vec![]);
    let args = [Value::Int(b.character), Value::Float(10.0)];
    let call = Call { service: "account", method: "GiveCash", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    assert_eq!(crate::services::dispatch(&mut c, &call).await.unwrap(), Value::None);
    assert_eq!(super::super::super::wallets::balance(&db, a.character).await.unwrap(), IskAmount::from_cents(9000).unwrap());
    assert_eq!(super::super::super::wallets::balance(&db, b.character).await.unwrap(), IskAmount::from_cents(1000).unwrap());
    let args = [Value::Int(b.character), Value::Float(1000.0)];
    let call = Call { args: &args, ..call };
    assert!(crate::services::dispatch(&mut c, &call).await.is_err());
    assert_eq!(super::super::super::wallets::balance(&db, a.character).await.unwrap(), IskAmount::from_cents(9000).unwrap());
    assert_eq!(super::super::super::wallets::balance(&db, b.character).await.unwrap(), IskAmount::from_cents(1000).unwrap());
    let count: i64 = db.query_one("SELECT count(*) AS count FROM game.wallet_journal j JOIN game.wallet_accounts a ON a.account_id=j.account_id WHERE a.owner_id=ANY($1)", &[&vec![a.character,b.character]]).await.unwrap().get("count");
    assert_eq!(count, 0);
    }).await;
    crate::services::tests::fixture::cleanup(&db, a).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, b).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn corporate_transfer_success_and_refusals_preserve_balances_without_journal() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = crate::services::tests::fixture::create(&db).await.unwrap();
    let member = crate::services::tests::fixture::create(&db).await.unwrap();
    let recipient = crate::services::tests::fixture::create(&db).await.unwrap();
    let corp = crate::services::tests::fixture::create_corporation(&db, ceo.character, 0).await.unwrap();
    let destination = crate::services::tests::fixture::create_corporation(&db, recipient.character, 0).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let from = ACCOUNT_CASH + 1;
        let to = ACCOUNT_CASH + 2;
        crate::services::tests::fixture::assign_corporation(&db, ceo.character, corp, 0).await.unwrap();
        crate::services::tests::fixture::assign_corporation(&db, member.character, corp, infinity_core::consts::ROLE_ACCOUNT_CAN_TAKE_1 << 1).await.unwrap();
        for (owner, key, amount) in [(corp, ACCOUNT_CASH, 50.0_f64), (corp, from, 100.0), (destination, to, 20.0)] {
            db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,$3::float8::numeric) ON CONFLICT (corporation_id,account_key) DO UPDATE SET balance=EXCLUDED.balance", &[&owner, &key, &amount]).await.unwrap();
        }
        db.execute("UPDATE game.characters SET balance=0 WHERE character_id=$1", &[&recipient.character]).await.unwrap();
        let (mut c, _peer) = crate::services::tests::client(db.clone(), member.character).await;
        c.session.apply(vec![("corpid", Value::Int(corp))]);
        let kwargs = Value::Dict(vec![]);
        let args = [Value::Int(recipient.character), Value::Float(10.0), Value::Int(from.into())];
        let call = Call { service: "account", method: "GiveCashFromCorpAccount", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut c, &call).await.unwrap(), Value::None);
        let args = [Value::Int(destination), Value::Float(15.0), Value::Int(from.into()), Value::Int(to.into())];
        assert_eq!(crate::services::dispatch(&mut c, &Call { args: &args, ..call }).await.unwrap(), Value::None);
        let args = [Value::Int(recipient.character), Value::Float(1000.0), Value::Int(from.into())];
        let CallError::User { msg, dict } = crate::services::dispatch(&mut c, &Call { args: &args, ..call }).await.unwrap_err() else { panic!("expected insufficient balance") };
        assert_eq!(msg, messages::MSG_NOT_ENOUGH_MONEY);
        assert_eq!(dict, vec![("balance".into(), Value::Float(75.0)), ("amount".into(), Value::Float(1000.0))]);
        let args = [Value::Int(recipient.character), Value::Float(1.0), Value::Int(ACCOUNT_CASH.into())];
        let CallError::User { msg, dict } = crate::services::dispatch(&mut c, &Call { args: &args, ..call }).await.unwrap_err() else { panic!("expected unauthorized refusal") };
        assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
        assert_eq!(dict, vec![("notify".into(), Value::unicode(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED))]);
        assert_eq!(corp_balance(&db, corp, from).await.unwrap(), IskAmount::from_cents(7500).unwrap());
        assert_eq!(corp_balance(&db, corp, ACCOUNT_CASH).await.unwrap(), IskAmount::from_cents(5000).unwrap());
        assert_eq!(corp_balance(&db, destination, to).await.unwrap(), IskAmount::from_cents(3500).unwrap());
        assert_eq!(super::super::super::wallets::balance(&db, recipient.character).await.unwrap(), IskAmount::from_cents(1000).unwrap());
        let count: i64 = db.query_one("SELECT count(*) AS count FROM game.wallet_journal j JOIN game.wallet_accounts a ON a.account_id=j.account_id WHERE a.owner_id=ANY($1)", &[&vec![corp, destination, recipient.character]]).await.unwrap().get("count");
        assert_eq!(count, 0);
    }).await;
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=ANY($1)", &[&vec![corp, destination]]).await.unwrap();
    crate::services::tests::fixture::cleanup_corporation(&db, corp, &[ceo.character, member.character], 0).await.unwrap();
    crate::services::tests::fixture::cleanup_corporation(&db, destination, &[recipient.character], 0).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, recipient).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, member).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, ceo).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn rolling_period_includes_exact_lower_bound() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let owner = -8_861_203_017_i64;
        tx.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,100)", &[&owner, &ACCOUNT_CASH]).await?;
        let account = crate::dao::wallets::accounts::ensure(tx, owner, ACCOUNT_CASH).await?;
        let boundary = filetime_now() - JOURNAL_DAYS * FILETIME_DAY;
        for date in [boundary - 1, boundary, boundary + 1] {
            journal::append(tx, account.id, IskAmount::from_cents(100).unwrap(), &journal::Entry { actor_id: owner, entry_type_id: infinity_core::consts::REF_MARKET_TRANSACTION, owner_id1: owner, owner_id2: 42, reference_id: 55, created_at: date, description: "", market_transaction_id: None, order_id: None, side: 1 }).await.map_err(|e| crate::db::Error::Progress(e.into()))?;
        }
        let rows = journal::list(tx, owner, Some(ACCOUNT_CASH), Period::Since(boundary)).await.map_err(|e| crate::db::Error::Progress(e.into()))?;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.date == boundary));
        tx.execute("DELETE FROM game.wallet_journal WHERE account_id=$1", &[&account.id]).await?;
        tx.execute("DELETE FROM game.wallet_accounts WHERE account_id=$1", &[&account.id]).await?;
        tx.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&owner, &ACCOUNT_CASH]).await?;
        Ok::<(), crate::db::Error>(())
    }), |(), ()| async { Ok(()) }).await.unwrap();
}
