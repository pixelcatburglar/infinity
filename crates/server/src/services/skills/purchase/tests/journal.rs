//! Direct skill purchase journal persistence and native account reads.
//! Exercises the bound skill handler and account service against isolated game data.
//! Requires an initialized SDE and does not cover corporation journals.
use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn purchase_journal_is_atomic_and_visible_to_buyer() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let other = match fixture::create(&db).await {
        Ok(f) => f,
        Err(e) => { fixture::cleanup(&db, f).await.unwrap(); panic!("creating second fixture: {e}"); }
    };
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let (mut c, _peer) = client(db.clone(), f.character).await;
        c.login.user_id = f.account;
        let (mut stranger, _peer) = client(db.clone(), other.character).await;
        stranger.login.user_id = other.account;
        let mut skills: Vec<_> = c.sde.types.iter().filter_map(|ty| {
            let id = i64::from(ty.type_id);
            sde_validate(&c.sde, &[id], &HashSet::new()).ok().filter(|(_, price)| *price > 0.0 && *price < 1_000_000_000.0).map(|_| id)
        }).take(2).collect();
        assert_eq!(skills.len(), 2);
        skills.sort_unstable_by(|a, b| b.cmp(a));
        let (prices, total) = sde_validate(&c.sde, &skills, &HashSet::new()).unwrap();
        let kwargs = Value::None;
        let args = [Value::list(skills.iter().copied().map(Value::Int))];
        let call = crate::services::Call { service: super::super::super::SKILL_HANDLER, method: "PurchaseSkills", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        let (sender, mut notifications) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = sender;
        let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
        let history_before = history_fingerprint(&db, f.character).await;
        let journal_count = || async { db.query_one("SELECT count(*) AS n FROM game.wallet_journal j JOIN game.wallet_accounts a ON a.account_id=j.account_id WHERE a.owner_id=$1", &[&f.character]).await.unwrap().get::<_, i64>("n") };
        characters::set(&db, f.character, crate::isk::IskAmount::ZERO).await.unwrap();
        let refusal = super::super::super::dispatch(&mut c, &call).await.unwrap();
        assert!(matches!(refusal, Err(CallError::User { msg, dict }) if msg == messages::MSG_NOT_ENOUGH_MONEY && dict == vec![("balance".into(), Value::Float(0.0)), ("amount".into(), Value::Float(total))]));
        let unavailable = c.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SKILL) && sde_validate(&c.sde, &[i64::from(ty.type_id)], &HashSet::new()).is_err()).expect("unavailable skill").type_id;
        for (ids, expected, argument, value) in [
            (vec![skills[0], skills[0]], messages::MSG_SKILL_PURCHASE_UNKNOWN_ERROR, "", skills[0]),
            (vec![i64::MAX], messages::MSG_ITEM_NOT_A_SKILL, "skillName", i64::MAX),
            (vec![i64::from(unavailable)], messages::MSG_SKILL_UNAVAILABLE_FOR_PURCHASE, "type", i64::from(unavailable)),
        ] {
            let args = [Value::list(ids.into_iter().map(Value::Int))];
            let error = super::super::super::dispatch(&mut c, &crate::services::Call { args: &args, ..call }).await.unwrap();
            let expected_args = if argument.is_empty() { vec![] } else { vec![(argument.into(), Value::Int(value))] };
            assert!(matches!(error, Err(CallError::User { msg, dict }) if msg == expected && dict == expected_args));
        }
        assert_eq!(journal_count().await, 0);
        assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), before);
        assert_eq!(history_fingerprint(&db, f.character).await, history_before);
        assert!(notifications.try_recv().is_err());
        let funded = market::round2(total + 100.0);
        let funded = crate::isk::IskAmount::from_isk(funded).unwrap();
        characters::set(&db, f.character, funded).await.unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let dispatched = flag.clone();
        let fences = crate::services::effects::online::capture_online_fences(&c.world, &[f.character]).await;
        {
            let _market = market::lock().await;
            let failed: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
                let sde = c.sde.clone(); let ids = skills.clone(); let fences = fences.clone();
                Box::pin(async move {
                    purchase_tx(tx, outbox, &sde, &fences, f.character, &ids, filetime_now()).await?;
                    tx.query_one("SELECT 1 / 0", &[]).await.map_err(|e| db_err(crate::db::Error::from(e)))?;
                    Ok(())
                })
            }, |(), _| async move { dispatched.store(true, Ordering::SeqCst); Ok(()) }).await;
            fixture::assert_division_by_zero(failed);
        }
        assert!(!flag.load(Ordering::SeqCst));
        assert_eq!(journal_count().await, 0);
        assert_eq!(characters::balance(&db, f.character).await.unwrap(), funded);
        assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), before);
        assert_eq!(history_fingerprint(&db, f.character).await, history_before);
        assert!(notifications.try_recv().is_err());
        let reply = super::super::super::dispatch(&mut c, &call).await.unwrap().unwrap();
        assert_eq!(reply, Value::list(skills.iter().copied().map(Value::Int)));
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), reply);
        let rows = db.query("SELECT j.journal_id,j.created_at,j.entry_type_id,j.actor_id,j.owner_id1,j.owner_id2,j.reference_id,j.amount::text AS amount,j.balance_after::text AS balance,j.market_transaction_id,j.order_id,j.side FROM game.wallet_journal j JOIN game.wallet_accounts a ON a.account_id=j.account_id WHERE a.owner_id=$1 ORDER BY j.journal_id", &[&f.character]).await.unwrap();
        assert_eq!(rows.len(), prices.len());
        let mut running = funded;
        let mut charged = 0_i128;
        for (row, (skill, price)) in rows.iter().zip(&prices) {
            let price = crate::isk::IskAmount::from_isk(*price).unwrap();
            running = running.checked_sub(price).unwrap();
            charged += fixture::isk_cents(price);
            assert_eq!(row.get::<_, i32>("entry_type_id"), REF_SKILL_PURCHASE);
            assert_eq!(row.get::<_, i64>("actor_id"), f.character);
            assert_eq!(row.get::<_, i64>("owner_id1"), f.character);
            assert_eq!(row.get::<_, i64>("owner_id2"), OWNER_UNOWNED);
            assert_eq!(row.get::<_, i64>("reference_id"), i64::from(*skill));
            assert_eq!(row.get::<_, String>("amount"), price.checked_neg().unwrap().decimal());
            assert_eq!(row.get::<_, String>("balance"), running.decimal());
            assert_eq!(row.get::<_, Option<i64>>("market_transaction_id"), None);
            assert_eq!(row.get::<_, Option<i64>>("order_id"), None);
            assert_eq!(row.get::<_, i16>("side"), 0);
        }
        assert_eq!(crate::isk::IskAmount::from_cents(charged).unwrap(), crate::isk::IskAmount::from_isk(total).unwrap());
        assert_eq!(characters::balance(&db, f.character).await.unwrap(), running);
        let account_args = [Value::Int(ACCOUNT_CASH.into()), Value::None, Value::None, Value::Bool(false)];
        let read = crate::services::Call { service: "account", method: "GetTransactions", args: &account_args, kwargs: &kwargs, bound: false, bind_params: None };
        let native = crate::services::dispatch(&mut c, &read).await.unwrap();
        let native = marshal::loads(&marshal::dumps(&native)).unwrap();
        let Value::List(native_rows) = native else { panic!("expected native journal list") };
        assert_eq!(native_rows.len(), rows.len());
        for (native, row) in native_rows.iter().zip(rows.iter().rev()) {
            for (field, expected) in [("transactionID", Value::Int(row.get("journal_id"))), ("transactionDate", Value::Int(row.get("created_at"))), ("entryTypeID", Value::Int(i64::from(REF_SKILL_PURCHASE))), ("ownerID1", Value::Int(f.character)), ("ownerID2", Value::Int(OWNER_UNOWNED)), ("referenceID", Value::Int(row.get("reference_id"))), ("amount", Value::Float(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("amount")).unwrap().client_isk(crate::isk::personal_client_cap()))), ("balance", Value::Float(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("balance")).unwrap().client_isk(crate::isk::personal_client_cap()))), ("description", Value::unicode(""))] {
                assert_eq!(crate::services::value_field(native, field), Some(&expected));
            }
        }
        assert_eq!(crate::services::dispatch(&mut stranger, &read).await.unwrap(), Value::list([]));
        let types = crate::services::Call { method: "GetEntryTypes", args: &[], ..read };
        let catalogue = crate::services::dispatch(&mut c, &types).await.unwrap();
        let entry = c.sde.accounting.iter().find(|e| e.id == REF_SKILL_PURCHASE).expect("skill purchase accounting type");
        assert!(entry.name_id > 0);
        assert!(catalogue.items().unwrap().iter().any(|v| crate::services::value_field(v, "entryTypeID") == Some(&Value::Int(i64::from(REF_SKILL_PURCHASE))) && crate::services::value_field(v, "entryTypeNameID") == Some(&Value::Int(entry.name_id.into()))));
        while notifications.try_recv().is_ok() {}
        let history_after = history_fingerprint(&db, f.character).await;
        let skills_after = fixture::skill_fingerprint(&db, f.character).await.unwrap();
        let error = super::super::super::dispatch(&mut c, &call).await.unwrap();
        assert!(matches!(error, Err(CallError::User { msg, dict }) if msg == messages::MSG_CHARACTER_ALREADY_KNOWS_SKILL && dict == vec![("skillName".into(), Value::Int(skills[0]))]));
        assert_eq!(journal_count().await, rows.len() as i64);
        assert_eq!(characters::balance(&db, f.character).await.unwrap(), running);
        let zero = stranger.sde.types.iter().find_map(|ty| {
            let id = i64::from(ty.type_id);
            sde_validate(&stranger.sde, &[id], &HashSet::new()).ok().filter(|(_, price)| *price == 0.0).map(|_| id)
        });
        if let Some(zero) = zero {
            let zero_args = [Value::list([Value::Int(zero)])];
            let zero_call = crate::services::Call { args: &zero_args, ..call };
            let response = super::super::super::dispatch(&mut stranger, &zero_call).await.unwrap().unwrap();
            assert_eq!(response, Value::list([Value::Int(zero)]));
            assert_eq!(crate::services::dispatch(&mut stranger, &read).await.unwrap(), Value::list([]));
        }
        assert_eq!(history_fingerprint(&db, f.character).await, history_after);
        assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), skills_after);
        assert!(notifications.try_recv().is_err());
    }).await;
    for fixture in [f, other] {
        let job = format!("{}:{}", super::super::completion::KIND, fixture.character);
        crate::scheduler::cancel(&db, &job).await.unwrap();
        db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&fixture.character]).await.unwrap();
        fixture::cleanup(&db, fixture).await.unwrap();
        assert_eq!(fixture::remaining(&db, fixture).await.unwrap(), 0);
    }
    result.unwrap();
}
