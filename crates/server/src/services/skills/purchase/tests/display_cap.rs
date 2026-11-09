//! Direct skill purchase debits and the client wallet display cap.
//! Mirrors characterskills/common/purchase.py pricing and the account wallet service.
//! Uses SDE types and isolated game characters, skills, history, items and jobs.
//! These tests require a dedicated initialized database and do not inspect a live user's balance.
use super::*;
use infinity_core::consts::CLIENT_MAX_ISK;

/// SDE type ID for Capital Repair Systems.
const EXAMPLE_SKILL: i32 = 21803;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn bound_purchase_debits_exact_balance_even_above_client_display_cap() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ordinary = fixture::create(&db).await.unwrap();
    let capped = match fixture::create(&db).await {
        Ok(f) => f,
        Err(e) => { fixture::cleanup(&db, ordinary).await.unwrap(); panic!("creating second fixture: {e}"); }
    };
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (c, _peer) = client(test_db.clone(), ordinary.character).await;
        let mut c = c;
        c.login.user_id = ordinary.account;
        let selectable = |id: i32| sde_validate(&c.sde, &[i64::from(id)], &HashSet::new()).ok().is_some_and(|(_, price)| price > 0.0 && price < 1_000_000_000.0);
        let skill = if selectable(EXAMPLE_SKILL) { EXAMPLE_SKILL } else { c.sde.types.iter().find(|ty| selectable(ty.type_id)).expect("positive priced purchasable skill").type_id };
        let (_, price) = sde_validate(&c.sde, &[i64::from(skill)], &HashSet::new()).unwrap();
        let base = c.sde.types.get(skill).unwrap().base_price.unwrap();
        assert_eq!(price, market::round2(base * (1.0 + DIRECT_PURCHASE_TAX_RATE)));
        for (f, initial, is_capped) in [(ordinary, format!("{:.2}", price + 100.0), false), (capped, "1000000000000000000000000.00".to_string(), true)] {
            let (mut c, _peer) = client(test_db.clone(), f.character).await;
            c.login.user_id = f.account;
            let (sender, mut notifications) = tokio::sync::mpsc::unbounded_channel();
            c.notify_tx = sender.clone();
            let initial_amount = crate::isk::IskAmount::parse_decimal(&initial).unwrap();
            assert_eq!(characters::set(&test_db, f.character, initial_amount).await.unwrap(), crate::dao::wallets::WalletMutation::Applied(initial_amount));
            let kwargs = Value::None;
            let cash_call = crate::services::Call { service: "account", method: "GetCashBalance", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
            let cash_before = crate::services::dispatch(&mut c, &cash_call).await.unwrap();
            let before: String = test_db.query_one("SELECT balance::text AS exact FROM game.characters WHERE character_id=$1", &[&f.character]).await.unwrap().get("exact");
            let books_before: i64 = test_db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&f.character, &skill]).await.unwrap().get("n");
            assert_eq!(books_before, 0);
            let args = [Value::list([Value::Int(i64::from(skill))])];
            let call = crate::services::Call { service: super::super::super::SKILL_HANDLER, method: "PurchaseSkills", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
            let response = super::super::super::dispatch(&mut c, &call).await.unwrap().unwrap();
            assert_eq!(response, Value::list([Value::Int(i64::from(skill))]));
            let exact_price = crate::isk::IskAmount::from_isk(price).unwrap().decimal();
            let row = test_db.query_one("SELECT balance::text AS exact, $2::text::numeric - balance = $3::text::numeric AS charged FROM game.characters WHERE character_id=$1", &[&f.character, &before, &exact_price]).await.unwrap();
            let after: String = row.get("exact");
            assert!(row.get::<_, bool>("charged"));
            let journal = test_db.query_one("SELECT j.journal_id, j.amount = -$2::text::numeric AS amount_ok, j.balance_after = $3::text::numeric AS balance_ok, j.balance_after::float8 AS balance FROM game.wallet_journal j JOIN game.wallet_accounts a ON a.account_id=j.account_id WHERE a.owner_id=$1", &[&f.character, &exact_price, &after]).await.unwrap();
            assert!(journal.get::<_, bool>("amount_ok") && journal.get::<_, bool>("balance_ok"));
            let journal_args = [Value::Int(ACCOUNT_CASH.into()), Value::None, Value::None, Value::Bool(false)];
            let journal_call = crate::services::Call { service: "account", method: "GetTransactions", args: &journal_args, kwargs: &kwargs, bound: false, bind_params: None };
            let native = crate::services::dispatch(&mut c, &journal_call).await.unwrap();
            let native = marshal::loads(&marshal::dumps(&native)).unwrap();
            let Value::List(rows) = native else { panic!("expected journal list") };
            assert_eq!(rows.len(), 1);
            for (field, expected) in [("transactionID", Value::Int(journal.get("journal_id"))), ("entryTypeID", Value::Int(i64::from(REF_SKILL_PURCHASE))), ("ownerID1", Value::Int(f.character)), ("ownerID2", Value::Int(OWNER_UNOWNED)), ("referenceID", Value::Int(i64::from(skill))), ("amount", Value::Float(-price)), ("balance", Value::Float(journal.get::<_, f64>("balance").min(CLIENT_MAX_ISK)))] {
                assert_eq!(crate::services::value_field(&rows[0], field), Some(&expected));
            }
            println!("skill={skill} base={base} price={price:.2} exact_before={before} exact_after={after}");
            let cash_after = crate::services::dispatch(&mut c, &cash_call).await.unwrap();
            if is_capped {
                assert_eq!(cash_before, Value::Float(CLIENT_MAX_ISK));
                assert_eq!(cash_after, Value::Float(CLIENT_MAX_ISK));
            } else {
                assert_eq!(cash_before, Value::Float(initial.parse::<f64>().unwrap()));
                assert_eq!(cash_after, Value::Float(100.0));
            }
            let mut account_changes = Vec::new();
            while let Ok((event, args)) = notifications.try_recv() {
                if event == "OnAccountChange" { account_changes.push(args); }
            }
            assert_eq!(account_changes, vec![vec![Value::str("cash"), Value::Int(f.character), cash_after]]);
            let skills = training::current(&test_db, f.character).await.unwrap();
            assert!(skills.iter().any(|row| row.type_id == skill && row.level == 0 && row.skill_points == 0));
            let books_after: i64 = test_db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&f.character, &skill]).await.unwrap().get("n");
            assert_eq!(books_after, 0);
        }
    }).await;
    for f in [ordinary, capped] {
        let job = format!("{}:{}", super::super::completion::KIND, f.character);
        crate::scheduler::cancel(&db, &job).await.unwrap();
        db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&f.character]).await.unwrap();
        fixture::cleanup(&db, f).await.unwrap();
        assert_eq!(fixture::remaining(&db, f).await.unwrap(), 0);
        let remaining: i64 = db.query_one("SELECT (SELECT count(*) FROM game.skill_history WHERE character_id=$1) + (SELECT count(*) FROM game.jobs WHERE job_id=$2) AS n", &[&f.character, &job]).await.unwrap().get("n");
        assert_eq!(remaining, 0);
    }
    result.unwrap();
}
