use super::super::*;
use infinity_core::consts::{ROLE_ACCOUNTANT, ROLE_JUNIOR_ACCOUNTANT, ROLE_DIRECTOR, ROLE_ACCOUNT_CAN_TAKE_1};

fn assert_denied(error: CallError) {
    let CallError::User { msg, dict } = error else { panic!("expected access denial") };
    assert_eq!(msg, messages::MSG_CRP_ACCESS_DENIED);
    assert_eq!(dict, vec![("reason".into(), Value::tuple([Value::Int(UE_LOC), Value::str(messages::account::NO_WALLET_DIVISION_ACCESS)]))]);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn current_membership_role_matrix_and_denied_dispatch() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = crate::services::tests::fixture::create(&db).await.unwrap();
    let member = crate::services::tests::fixture::create(&db).await.unwrap();
    let corp = crate::services::tests::fixture::create_corporation(&db, ceo.character, 0).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        crate::services::tests::fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR).await.unwrap();
        for i in 0..DIVISIONS {
            let key = ACCOUNT_CASH + i;
            db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,$3::float8::numeric) ON CONFLICT (corporation_id,account_key) DO UPDATE SET balance=EXCLUDED.balance", &[&corp, &key, &(100.0 + f64::from(i))]).await.unwrap();
            if i < 2 {
                let account = crate::dao::wallets::accounts::ensure(&db, corp, key).await.unwrap();
                journal::append(&db, account.id, IskAmount::from_cents(100).unwrap(), &journal::Entry { actor_id: ceo.character, entry_type_id: infinity_core::consts::REF_MARKET_TRANSACTION, owner_id1: corp, owner_id2: ceo.character, reference_id: i.into(), created_at: filetime_now(), description: "division", market_transaction_id: None, order_id: None, side: 1 }).await.unwrap();
            }
        }
        let (mut c, _peer) = crate::services::tests::client(db.clone(), member.character).await;
        c.session.apply(vec![("corpid", Value::Int(corp))]);
        let kwargs = Value::Dict(vec![]);
        c.session.apply(vec![("corpAccountKey", Value::Int((ACCOUNT_CASH + 1).into()))]);
        for i in 0..DIVISIONS {
            crate::services::tests::fixture::assign_corporation(&db, member.character, corp, ROLE_ACCOUNT_CAN_TAKE_1 << i).await.unwrap();
            let key = ACCOUNT_CASH + i;
            let args = [Value::Int(1), Value::Int(key.into())];
            let call = Call { service: "account", method: "GetCashBalance", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            assert_eq!(crate::services::dispatch(&mut c, &call).await.unwrap(), Value::Float(100.0 + f64::from(i)));
            for bad in [Value::None, Value::Int((ACCOUNT_CASH + (i + 1) % DIVISIONS).into())] {
                let args = [Value::Int(1), bad.clone()];
                assert_denied(crate::services::dispatch(&mut c, &Call { args: &args, ..call }).await.unwrap_err());
                let args = [bad, Value::None, Value::None, Value::Bool(true)];
                let call = Call { service: "account", method: "GetTransactions", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                assert_denied(crate::services::dispatch(&mut c, &call).await.unwrap_err());
            }
            let args = [Value::Int(key.into()), Value::None, Value::None, Value::Bool(true)];
            let call = Call { service: "account", method: "GetTransactions", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let rows = crate::services::dispatch(&mut c, &call).await.unwrap();
            assert_eq!(rows.items().unwrap().len(), if i < 2 { 1 } else { 0 });
            if i < 2 { assert_eq!(crate::services::value_field(&rows.items().unwrap()[0], "referenceID"), Some(&Value::Int(i.into()))); }
            let call = Call { service: "account", method: "GetWalletDivisionsInfo", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
            let rows = crate::services::dispatch(&mut c, &call).await.unwrap();
            assert_eq!(rows.items().unwrap().len(), DIVISIONS as usize);
            for (j, row) in rows.items().unwrap().iter().enumerate() {
                assert_eq!(crate::services::value_field(row, "balance"), Some(&Value::Float(if j == i as usize { 100.0 + f64::from(i) } else { 0.0 })));
            }
        }
        for role in [ROLE_ACCOUNTANT, ROLE_JUNIOR_ACCOUNTANT, ROLE_DIRECTOR] {
            crate::services::tests::fixture::assign_corporation(&db, member.character, corp, role).await.unwrap();
            if role == ROLE_ACCOUNTANT {
                let args = [Value::Int(1), Value::None];
                let call = Call { service: "account", method: "GetCashBalance", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                assert_eq!(crate::services::dispatch(&mut c, &call).await.unwrap(), Value::Float(101.0));
                for key in [i64::from(ACCOUNT_CASH + DIVISIONS), i64::MAX] {
                    c.session.apply(vec![("corpAccountKey", Value::Int(key))]);
                    assert_denied(crate::services::dispatch(&mut c, &call).await.unwrap_err());
                }
                c.session.apply(vec![("corpAccountKey", Value::Int(ACCOUNT_CASH.into()))]);
            }
            for key in std::iter::once(None).chain((ACCOUNT_CASH..ACCOUNT_CASH + DIVISIONS).map(Some)) {
                let key = key.map_or(Value::None, |k| Value::Int(k.into()));
                let args = [Value::Int(1), key.clone()];
                let call = Call { service: "account", method: "GetCashBalance", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                assert!(crate::services::dispatch(&mut c, &call).await.unwrap().as_float().unwrap() >= 100.0);
                let args = [key.clone(), Value::None, Value::None, Value::Bool(true)];
                let call = Call { service: "account", method: "GetTransactions", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                let rows = crate::services::dispatch(&mut c, &call).await.unwrap();
                assert_eq!(rows.items().unwrap().len(), if key == Value::None { 2 } else if key == Value::Int(ACCOUNT_CASH.into()) || key == Value::Int((ACCOUNT_CASH + 1).into()) { 1 } else { 0 });
            }
        }
        let (mut ceo_client, _peer) = crate::services::tests::client(db.clone(), ceo.character).await;
        ceo_client.session.apply(vec![("corpid", Value::Int(corp))]);
        let args = [Value::None, Value::None, Value::None, Value::Bool(true)];
        let call = Call { service: "account", method: "GetTransactions", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut ceo_client, &call).await.unwrap().items().unwrap().len(), 2);
        for roles in [0, ROLE_DIRECTOR] {
            crate::services::tests::fixture::assign_corporation(&db, member.character, 1, roles).await.unwrap();
            let args = [Value::Int(1), Value::Int(ACCOUNT_CASH.into())];
            let call = Call { service: "account", method: "GetCashBalance", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            assert_denied(crate::services::dispatch(&mut c, &call).await.unwrap_err());
            let args = [Value::None, Value::None, Value::None, Value::Bool(true)];
            let call = Call { service: "account", method: "GetTransactions", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            assert_denied(crate::services::dispatch(&mut c, &call).await.unwrap_err());
            let call = Call { service: "account", method: "GetWalletDivisionsInfo", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
            let rows = crate::services::dispatch(&mut c, &call).await.unwrap();
            for row in rows.items().unwrap() { assert_eq!(crate::services::value_field(row, "balance"), Some(&Value::Float(0.0))); }
        }
        for key in ACCOUNT_CASH..ACCOUNT_CASH + DIVISIONS {
            let args = [Value::Int(1), Value::Int(key.into())];
            let call = Call { service: "account", method: "GetCashBalance", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            assert_eq!(crate::services::dispatch(&mut ceo_client, &call).await.unwrap(), Value::Float(100.0 + f64::from(key - ACCOUNT_CASH)));
        }
        let args = [Value::Int(1), Value::None];
        let call = Call { service: "account", method: "GetCashBalance", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(crate::services::dispatch(&mut ceo_client, &call).await.unwrap().as_float().unwrap() >= 100.0);
    }).await;
    db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1)", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.wallet_accounts WHERE owner_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await.unwrap();
    crate::services::tests::fixture::cleanup_corporation(&db, corp, &[ceo.character, member.character], 0).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, member).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, ceo).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn corporation_balance_push_respects_current_division_read_rights() {
    use crate::db::outbox::{Effect, Notification, Outbox};
    use crate::lua::events::GameEvent;

    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = crate::services::tests::fixture::create(&db).await.unwrap();
    let member = crate::services::tests::fixture::create(&db).await.unwrap();
    let corp = crate::services::tests::fixture::create_corporation(&db, ceo.character, 0).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        crate::services::tests::fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR).await.unwrap();
        for key in [ACCOUNT_CASH, ACCOUNT_CASH + 1] {
            db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,100) ON CONFLICT (corporation_id,account_key) DO UPDATE SET balance=EXCLUDED.balance", &[&corp, &key]).await.unwrap();
        }
        for (role, key, member_allowed) in [
            (0, ACCOUNT_CASH, false),
            (ROLE_ACCOUNT_CAN_TAKE_1, ACCOUNT_CASH + 1, false),
            (ROLE_ACCOUNT_CAN_TAKE_1, ACCOUNT_CASH, true),
            (ROLE_ACCOUNTANT, ACCOUNT_CASH + 1, true),
            (ROLE_JUNIOR_ACCOUNTANT, ACCOUNT_CASH + 1, true),
            (ROLE_DIRECTOR, ACCOUNT_CASH + 1, true),
            (0, ACCOUNT_CASH, false),
        ] {
            crate::services::tests::fixture::assign_corporation(&db, member.character, corp, role).await.unwrap();
            let before = corp_balance(&db, corp, key).await.unwrap();
            let expected = IskAmount::from_cents(crate::services::tests::fixture::isk_cents(before) + 100).unwrap();
            let event_count = crate::db::outbox::tests::wallet_events(&db, corp).await.len();
            let effects = db.transaction(
                |tx, outbox: &mut Outbox<()>| Box::pin(async move {
                    crate::services::wallets::corp_credit(tx, outbox, corp, key, IskAmount::from_cents(100).unwrap()).await
                }),
                |(), outbox| async move { Ok::<_, CallError>(outbox.into_entries()) },
            ).await.unwrap();
            assert_eq!(corp_balance(&db, corp, key).await.unwrap(), expected);
            let args = vec![Value::str(if key == ACCOUNT_CASH { "cash" } else { "cash2" }), Value::Int(corp), Value::Float(expected.client_isk(corporation_client_cap()))];
            let notices: Vec<_> = effects.iter().filter_map(|effect| match effect {
                Effect::Notification(Notification::Pilot { char_id, event, args: actual }) if event == "OnAccountChange" => {
                    assert_eq!(actual, &args);
                    Some(*char_id)
                }
                _ => None,
            }).collect();
            assert_eq!(notices.iter().filter(|&&id| id == ceo.character).count(), 1);
            assert_eq!(notices.iter().filter(|&&id| id == member.character).count(), usize::from(member_allowed));
            assert_eq!(notices.len(), 1 + usize::from(member_allowed));
            let events = crate::db::outbox::tests::wallet_events(&db, corp).await;
            assert_eq!(events.len(), event_count + 1);
            assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::WalletChanged { owner_id, account_key, balance, delta } if *owner_id == corp && *account_key == key && *balance == expected && *delta == IskAmount::from_cents(100).unwrap())).count(), 1);
            assert_eq!(effects.len(), notices.len());
        }
    }).await;
    db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1)", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.wallet_accounts WHERE owner_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await.unwrap();
    crate::services::tests::fixture::cleanup_corporation(&db, corp, &[ceo.character, member.character], 0).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, member).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, ceo).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn current_wallet_read_matrix_matches_canonical_authority() {
    use crate::{dao::wallets::corporations, services::{corps::permissions, tests::fixture}};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 0).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        fixture::assign_corporation(&db, ceo.character, corp, 0).await.unwrap();
        fixture::assign_corporation(&db, member.character, corp, 0).await.unwrap();
        db.execute("INSERT INTO game.corp_titles(corporation_id,title_id,name,roles) VALUES ($1,1,'wallet', $2)", &[&corp, &ROLE_ACCOUNT_CAN_TAKE_1]).await.unwrap();
        for (ceo_role, member_role, title, blocked, deleted_corp, deleted_member, ceo_expected, member_division) in [
            (ROLE_DIRECTOR, 0, false, false, false, false, true, -1),
            (0, 0, false, false, false, false, false, -1),
            (ROLE_ACCOUNTANT, 0, false, false, false, false, true, -1),
            (ROLE_DIRECTOR, ROLE_ACCOUNT_CAN_TAKE_1 << 2, false, false, false, false, true, 2),
            (ROLE_DIRECTOR, 0, true, false, false, false, true, 0),
            (ROLE_DIRECTOR, 0, true, true, false, false, true, -1),
            (ROLE_DIRECTOR, ROLE_ACCOUNT_CAN_TAKE_1, true, true, false, false, true, 0),
            (ROLE_DIRECTOR, 0, false, false, true, false, false, -1),
            (ROLE_DIRECTOR, ROLE_ACCOUNTANT, false, false, false, true, true, -1),
        ] {
            db.execute("UPDATE game.corporations SET deleted=$2 WHERE corporation_id=$1", &[&corp, &deleted_corp]).await.unwrap();
            db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo.character, &ceo_role]).await.unwrap();
            let title_mask: i64 = if title { 1 } else { 0 };
            db.execute("UPDATE game.characters SET corp_roles=$2,corp_title_mask=$3,corp_block_roles=$4,deleted=$5 WHERE character_id=$1", &[&member.character, &member_role, &title_mask, &blocked, &deleted_member]).await.unwrap();
            for division in 0..DIVISIONS {
                let mask = ROLE_DIRECTOR | ROLE_ACCOUNTANT | ROLE_JUNIOR_ACCOUNTANT | (ROLE_ACCOUNT_CAN_TAKE_1 << division);
                let audience = corporations::readable_members(&db, corp, mask).await.unwrap();
                assert!(audience.windows(2).all(|ids| ids[0] < ids[1]));
                for (id, expected) in [(ceo.character, ceo_expected), (member.character, member_division == division)] {
                    let canonical = permissions::load(&db, id).await.unwrap().is_some_and(|authority| permissions::allows(&authority, corp, mask));
                    assert_eq!(canonical, expected);
                    assert_eq!(corporations::read_allowed(&db, id, corp, mask).await.unwrap(), expected);
                    assert_eq!(audience.binary_search(&id).is_ok(), expected);
                }
            }
        }
    }).await;
    db.execute("UPDATE game.corporations SET deleted=false WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET deleted=false,corp_title_mask=0,corp_block_roles=false WHERE character_id=$1", &[&member.character]).await.unwrap();
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup_corporation(&db, corp, &[ceo.character, member.character], 0).await.unwrap();
    fixture::cleanup(&db, member).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn real_corporate_withdrawal_waits_for_revocation_and_refuses() {
    use anyhow::{anyhow, ensure, Result};
    use crate::{db::Db, services::tests::{self, fixture}};
    use std::time::Duration;
    use tokio::task::JoinSet;
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = Db::connect(&cfg).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let recipient = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let db = test_db;
        let run: Result<()> = async {
            fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR).await?;
            fixture::assign_corporation(&db, member.character, corp, ROLE_ACCOUNT_CAN_TAKE_1).await?;
            db.execute("UPDATE game.characters SET station_id=60000001 WHERE character_id=ANY($1)", &[&vec![ceo.character,member.character]]).await?;
            db.execute("UPDATE game.characters SET balance=0 WHERE character_id=$1", &[&recipient.character]).await?;
            db.execute("INSERT INTO game.corp_wallets(corporation_id,account_key,balance) VALUES ($1,$2,100)", &[&corp, &ACCOUNT_CASH]).await?;
            let mut isolated = cfg.clone(); isolated.pool_size = 1; isolated.schema_on_start = false;
            let revoker_db = Db::connect(&isolated).await?;
            let withdraw_db = Db::connect(&isolated).await?;
            let (mut chief, _chief_peer) = tests::client(revoker_db.clone(), ceo.character).await;
            let (mut payer, _payer_peer) = tests::client(withdraw_db.clone(), member.character).await;
            chief.session.apply(vec![("corpid", Value::Int(corp))]);
            payer.session.apply(vec![("corpid", Value::Int(corp)), ("corprole", Value::Int(ROLE_ACCOUNT_CAN_TAKE_1))]);
            let (notify, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            payer.world.register_pilot(member.character, notify);
            let args = [Value::Int(recipient.character), Value::Float(10.0), Value::Int(ACCOUNT_CASH.into())];
            let kwargs = Value::Dict(vec![]);
            let transfer = Call { service: "account", method: "GiveCashFromCorpAccount", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let positive = crate::services::dispatch(&mut payer, &transfer).await.map_err(|e| anyhow!("positive transfer: {e:?}"))?;
            ensure!(positive == Value::None, "positive transfer not accepted");
            ensure!(corp_balance(&db, corp, ACCOUNT_CASH).await.map_err(|e| anyhow!("{e:?}"))? == IskAmount::from_cents(9000).unwrap(), "positive transfer did not debit");
            ensure!(crate::services::wallets::balance(&db, recipient.character).await.map_err(|e| anyhow!("{e:?}"))? == IskAmount::from_cents(1000).unwrap(), "positive transfer did not credit");
            while receiver.try_recv().is_ok() {}
            let snapshot = db.query_one("SELECT (SELECT balance::text FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2) AS corp, (SELECT balance::text FROM game.characters WHERE character_id=$3) AS recipient, (SELECT count(*) FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=ANY($4)) AS journals", &[&corp, &ACCOUNT_CASH, &recipient.character, &vec![corp, recipient.character]]).await?;
            let before = (snapshot.get::<_, String>("corp"), snapshot.get::<_, String>("recipient"), snapshot.get::<_, i64>("journals"));
            let revoker_pid: i32 = revoker_db.query_one("SELECT pg_backend_pid() AS pid", &[]).await?.get("pid");
            let withdraw_pid: i32 = withdraw_db.query_one("SELECT pg_backend_pid() AS pid", &[]).await?.get("pid");
            let (mut gate, connection) = cfg.to_pg_config()?.connect(tokio_postgres::NoTls).await?;
            let driver = tokio::spawn(connection);
            let gate_pid: i32 = gate.query_one("SELECT pg_backend_pid() AS pid", &[]).await?.get("pid");
            let mut gate_tx = Some(gate.transaction().await?);
            let mut tasks = JoinSet::new();
            let race: Result<()> = async {
                gate_tx.as_ref().unwrap().query_one("SELECT character_id FROM game.characters WHERE character_id=$1 FOR UPDATE", &[&member.character]).await?;
                let mut update = vec![Value::None; 15]; update[0] = Value::Int(member.character); update[4] = Value::Int(0);
                tasks.spawn(async move {
                    let kwargs = Value::Dict(vec![]); let binding = Value::Int(corp);
                    let call = Call { service: "corpRegistry", method: "UpdateMember", args: &update, kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
                    ("revocation", crate::services::dispatch(&mut chief, &call).await)
                });
                tokio::time::timeout(Duration::from_secs(30), async {
                    loop {
                        let blocked: bool = db.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND $2=ANY(pg_blocking_pids(pid)) AND wait_event_type='Lock') AS blocked", &[&revoker_pid, &gate_pid]).await?.get("blocked");
                        if blocked { break; } tokio::task::yield_now().await;
                    }
                    Ok::<(), anyhow::Error>(())
                }).await.map_err(|_| anyhow!("revoker did not block on member row"))??;
                tasks.spawn(async move {
                    let args = [Value::Int(recipient.character), Value::Float(10.0), Value::Int(ACCOUNT_CASH.into())]; let kwargs = Value::Dict(vec![]);
                    let call = Call { service: "account", method: "GiveCashFromCorpAccount", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                    ("withdrawal", crate::services::dispatch(&mut payer, &call).await)
                });
                tokio::time::timeout(Duration::from_secs(30), async {
                    loop {
                        let blocked: bool = db.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND $2=ANY(pg_blocking_pids(pid)) AND wait_event_type='Lock') AS blocked", &[&withdraw_pid, &revoker_pid]).await?.get("blocked");
                        if blocked { break; } tokio::task::yield_now().await;
                    }
                    Ok::<(), anyhow::Error>(())
                }).await.map_err(|_| anyhow!("withdrawal did not wait on revoker"))??;
                gate_tx.take().unwrap().commit().await?;
                let mut revocation_ok = false; let mut withdrawal_denied = false;
                for _ in 0..2 {
                    let (kind, outcome) = tokio::time::timeout(Duration::from_secs(30), tasks.join_next()).await.map_err(|_| anyhow!("RPC task timed out"))?.ok_or_else(|| anyhow!("RPC task missing"))??;
                    match kind {
                        "revocation" => revocation_ok = matches!(outcome, Ok(Value::None)),
                        "withdrawal" => withdrawal_denied = matches!(outcome, Err(CallError::User { msg, dict })
                            if msg == messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED))]),
                        _ => unreachable!(),
                    }
                }
                ensure!(revocation_ok && withdrawal_denied, "revocation or withdrawal outcome incorrect");
                let after = db.query_one("SELECT (SELECT balance::text FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2) AS corp, (SELECT balance::text FROM game.characters WHERE character_id=$3) AS recipient, (SELECT count(*) FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=ANY($4)) AS journals", &[&corp, &ACCOUNT_CASH, &recipient.character, &vec![corp, recipient.character]]).await?;
                ensure!((after.get::<_, String>("corp"), after.get::<_, String>("recipient"), after.get::<_, i64>("journals")) == before, "failed withdrawal changed financial state");
                ensure!(!std::iter::from_fn(|| receiver.try_recv().ok()).any(|(event, _)| event == "OnAccountChange"), "failed withdrawal emitted wallet notice");
                Ok(())
            }.await;
            if let Some(tx) = gate_tx.take() { let _ = tx.rollback().await; }
            tasks.abort_all(); while tasks.join_next().await.is_some() {}
            drop(gate_tx); drop(gate); let _ = driver.await;
            race
        }.await;
        run
    }).await;
    let ids = [ceo.character, member.character, recipient.character];
    db.execute("DELETE FROM game.corp_role_history WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corp_member_events WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=ANY($1))", &[&vec![corp,recipient.character]]).await.unwrap();
    db.execute("DELETE FROM game.wallet_accounts WHERE owner_id=ANY($1)", &[&vec![corp,recipient.character]]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup_corporation(&db, corp, &[ceo.character,member.character], 0).await.unwrap();
    for person in [ceo,member,recipient] { fixture::cleanup(&db, person).await.unwrap(); }
    scenario.unwrap().unwrap();
}
