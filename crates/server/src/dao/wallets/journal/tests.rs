use super::*;
use crate::dao::wallets::accounts;
use crate::db::{Db, Error, outbox::{Effect, Notification, Outbox}};
use crate::services::{CallError, db_err, tests::fixture, wallets::{journal as service_journal, ownership::FinancialOwner}};
use crate::lua::events::GameEvent;

fn amount(s:&str)->IskAmount { IskAmount::parse_decimal(s).unwrap() }
fn entry(owner:i64, date:i64)->Entry<'static> { Entry { actor_id:owner, entry_type_id:REF_MARKET_TRANSACTION, owner_id1:owner, owner_id2:123, reference_id:9, created_at:date, description:"", market_transaction_id:None, order_id:None, side:1 } }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn exact_order_components_and_month_boundaries() {
    let db=Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture=fixture::create(&db).await.unwrap();
    let owner=fixture.character;
    let result:Result<(),CallError>=db.transaction(|tx, outbox: &mut Outbox<()>| Box::pin(async move {
        tx.execute("UPDATE game.characters SET balance=9999999999999999999999999999999999.99 WHERE character_id=$1", &[&owner]).await.map_err(Error::from)?;
        let escrow_cents=9_223_372_036_854_i128 * i128::from(i32::MAX) * 100;
        let escrow=IskAmount::from_cents(escrow_cents).unwrap();
        let fee=amount("0.01");
        let total=IskAmount::from_cents(escrow_cents + 1).unwrap();
        let date:i64=tx.query_one("SELECT extract(epoch from '2024-02-29'::timestamp AT TIME ZONE 'UTC')::bigint * $1::bigint + $2::bigint AS date", &[&FILETIME_SECOND,&FILETIME_UNIX_EPOCH]).await.map_err(Error::from)?.get("date");
        service_journal::debit_order_charge(tx,outbox,FinancialOwner::cash(owner),total,escrow,&entry(owner,date)).await?;
        let rows=list(tx,owner,Some(ACCOUNT_CASH),Period::Month { year:2024,month:2 }).await.map_err(db_err)?;
        assert_eq!(rows.len(),2);
        assert_eq!(rows[0].entry_type_id,REF_BROKER_FEE);
        assert_eq!(rows[0].amount,fee.checked_neg().unwrap());
        assert_eq!(rows[1].entry_type_id,REF_MARKET_ESCROW);
        assert_eq!(rows[1].amount,escrow.checked_neg().unwrap());
        assert_eq!(amount(&rows[1].balance).checked_sub(amount(&rows[0].balance)),Some(fee));
        assert_eq!(fixture::isk_cents(rows[1].amount) + fixture::isk_cents(rows[0].amount), -fixture::isk_cents(total));
        assert!(list(tx,owner,None,Period::Month { year:2024,month:3 }).await.map_err(db_err)?.is_empty());
        Ok(())
    }), |(), _| async { Ok(()) }).await;
    fixture::cleanup(&db,fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn sale_preserves_wider_snapshot_and_zero_net_notification() {
    let db=Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture=fixture::create(&db).await.unwrap();
    let owner=fixture.character;
    let result:Result<(),CallError>=db.transaction(|tx, outbox: &mut Outbox<()>| Box::pin(async move {
        tx.execute("UPDATE game.characters SET balance=9999999999999999999999999999999999.99 WHERE character_id=$1", &[&owner]).await.map_err(Error::from)?;
        let account=accounts::ensure(tx,owner,ACCOUNT_CASH).await?;
        let gross=amount("9999999999999999999999999999999999.99");
        service_journal::credit_sale(tx,outbox,FinancialOwner::cash(owner),account.id,gross,IskAmount::ZERO,&entry(owner,1)).await?;
        let rows=list(tx,owner,Some(ACCOUNT_CASH),Period::Since(0)).await.map_err(db_err)?;
        assert_eq!(rows.len(),2);
        assert_eq!(rows[0].amount,gross.checked_neg().unwrap());
        assert_eq!(rows[0].balance,gross.decimal());
        assert_eq!(rows[1].amount,gross);
        assert_eq!(rows[1].balance,"19999999999999999999999999999999999.98");
        assert_eq!(rows[1].entry_type_id,REF_MARKET_TRANSACTION);
        assert_eq!(rows[0].entry_type_id,REF_TRANSACTION_TAX);
        Ok(())
    }), |(), outbox| async move {
        assert_eq!(outbox.into_entries().iter().filter(|e| matches!(e,Effect::Notification(Notification::Pilot { char_id, event, .. }) if *char_id == owner && event == "OnAccountChange")).count(),1);
        Ok(())
    }).await;
    assert_eq!(crate::db::outbox::tests::wallet_events(&db,owner).await.iter().filter(|e| matches!(e,GameEvent::WalletChanged { .. })).count(),0);
    fixture::cleanup(&db,fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn invalid_and_zero_components_have_no_effects() {
    let db=Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture=fixture::create(&db).await.unwrap();
    let owner=fixture.character;
    let result:Result<(),CallError>=db.transaction(|tx, outbox: &mut Outbox<()>| Box::pin(async move {
        let before:String=tx.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.map_err(Error::from)?.get("balance");
        let financial=FinancialOwner::cash(owner);
        let one=amount("1.00"); let negative=amount("-0.01");
        assert!(service_journal::credit(tx,outbox,financial,negative,&entry(owner,1)).await.is_err());
        assert!(service_journal::debit(tx,outbox,financial,negative,&entry(owner,1)).await.is_err());
        assert!(service_journal::debit_order_charge(tx,outbox,financial,one,amount("1.01"),&entry(owner,1)).await.is_err());
        assert!(service_journal::debit_order_charge(tx,outbox,financial,one,negative,&entry(owner,1)).await.is_err());
        assert!(service_journal::credit_sale(tx,outbox,financial,0,one,amount("1.01"),&entry(owner,1)).await.is_err());
        assert!(service_journal::credit_sale(tx,outbox,financial,0,one,negative,&entry(owner,1)).await.is_err());
        service_journal::debit_order_charge(tx,outbox,financial,IskAmount::ZERO,IskAmount::ZERO,&entry(owner,1)).await?;
        service_journal::credit_sale(tx,outbox,financial,0,IskAmount::ZERO,IskAmount::ZERO,&entry(owner,1)).await?;
        assert!(tx.query_opt("SELECT account_id FROM game.wallet_accounts WHERE owner_id = $1 AND account_key = $2", &[&owner, &ACCOUNT_CASH]).await.map_err(Error::from)?.is_none());
        let after:String=tx.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.map_err(Error::from)?.get("balance");
        assert_eq!(before,after);
        Ok(())
    }), |(), outbox| async move { assert!(outbox.into_entries().is_empty()); Ok(()) }).await;
    assert!(crate::db::outbox::tests::wallet_events(&db,owner).await.is_empty());
    fixture::cleanup(&db,fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn overflow_and_rollback_leave_no_cash_journal_or_events() {
    let db=Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture=fixture::create(&db).await.unwrap();
    let owner=fixture.character;
    let setup:Result<(),CallError>=db.transaction(|tx, _: &mut Outbox<()>| Box::pin(async move {
        tx.execute("UPDATE game.characters SET balance=9999999999999999999999999999999999.99 WHERE character_id=$1", &[&owner]).await.map_err(Error::from)?; Ok(())
    }), |(), _| async { Ok(()) }).await;
    setup.unwrap();
    let result:Result<(),CallError>=db.transaction(|tx, outbox: &mut Outbox<()>| Box::pin(async move {
        service_journal::debit_order_charge(tx,outbox,FinancialOwner::cash(owner),amount("9999999999999999999999999999999999.99"),IskAmount::ZERO,&entry(owner,1)).await?;
        service_journal::debit(tx,outbox,FinancialOwner::cash(owner),amount("0.01"),&entry(owner,2)).await
    }), |(), _| async { panic!("insufficient funds must not dispatch") }).await;
    assert!(result.is_err());
    let result:Result<(),CallError>=db.transaction(|tx, outbox: &mut Outbox<()>| Box::pin(async move {
        service_journal::credit(tx,outbox,FinancialOwner::cash(owner),amount("0.01"),&entry(owner,1)).await

    }), |(), _| async { panic!("overflow must not dispatch") }).await;
    assert!(result.is_err());
    let result:Result<(),CallError>=db.transaction(|tx, outbox: &mut Outbox<()>| Box::pin(async move {
        service_journal::debit(tx,outbox,FinancialOwner::cash(owner),amount("0.01"),&entry(owner,1)).await?;
        Err(CallError::Internal(anyhow::anyhow!("rollback test")))
    }), |(), _| async { panic!("rollback must not dispatch") }).await;
    assert!(result.is_err());
    let balance:String=db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("balance");
    assert_eq!(balance,"9999999999999999999999999999999999.99");
    assert!(db.query_opt("SELECT account_id FROM game.wallet_accounts WHERE owner_id = $1 AND account_key = $2", &[&owner, &ACCOUNT_CASH]).await.unwrap().is_none());
    assert!(crate::db::outbox::tests::wallet_events(&db,owner).await.is_empty());
    fixture::cleanup(&db,fixture).await.unwrap();
}
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn zero_tax_and_zero_escrow_produce_one_row_each() {
    let db=Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture=fixture::create(&db).await.unwrap();
    let owner=fixture.character;
    let result:Result<(),CallError>=db.transaction(|tx, outbox: &mut Outbox<()>| Box::pin(async move {
        tx.execute("UPDATE game.characters SET balance=123.45 WHERE character_id=$1", &[&owner]).await.map_err(Error::from)?;
        let account=accounts::ensure(tx,owner,ACCOUNT_CASH).await?;
        let financial=FinancialOwner::cash(owner);
        service_journal::credit_sale(tx,outbox,financial,account.id,amount("100.00"),amount("100.00"),&entry(owner,1)).await?;
        service_journal::debit_order_charge(tx,outbox,financial,amount("10.01"),IskAmount::ZERO,&entry(owner,2)).await?;
        let rows=list(tx,owner,Some(ACCOUNT_CASH),Period::Since(0)).await.map_err(db_err)?;
        assert_eq!(rows.len(),2);
        assert_eq!(rows[0].entry_type_id,REF_BROKER_FEE);
        assert_eq!(rows[0].amount,amount("-10.01"));
        assert_eq!(rows[0].balance,"213.44");
        assert_eq!(rows[1].entry_type_id,REF_MARKET_TRANSACTION);
        assert_eq!(rows[1].amount,amount("100.00"));
        assert_eq!(rows[1].balance,"223.45");
        Ok(())
    }), |(), outbox| async move {
        assert_eq!(outbox.into_entries().iter().filter(|e| matches!(e,Effect::Notification(_))).count(),2);
        Ok(())
    }).await;
    assert_eq!(crate::db::outbox::tests::wallet_events(&db,owner).await.len(),2);
    fixture::cleanup(&db,fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn duplicate_fill_rolls_back_cash_and_journal() {
    let db=Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture=fixture::create(&db).await.unwrap();
    let owner=fixture.character;
    let result:Result<(),CallError>=db.transaction(|tx, outbox: &mut Outbox<()>| Box::pin(async move {
        let fill:i64=tx.query_one("INSERT INTO game.market_transactions (type_id,quantity,price,buyer_id,seller_id,station_id,region_id,date) VALUES (1,1,1,$1,$1,1,1,1) RETURNING transaction_id", &[&owner]).await.map_err(Error::from)?.get("transaction_id");
        let mut e=entry(owner,1);
        e.market_transaction_id=Some(fill);
        service_journal::credit(tx,outbox,FinancialOwner::cash(owner),amount("1.00"),&e).await?;
        service_journal::credit(tx,outbox,FinancialOwner::cash(owner),amount("1.00"),&e).await
    }), |(), _| async { panic!("duplicate must not dispatch") }).await;
    assert!(result.is_err());
    assert!(db.query_opt("SELECT account_id FROM game.wallet_accounts WHERE owner_id = $1 AND account_key = $2", &[&owner, &ACCOUNT_CASH]).await.unwrap().is_none());
    assert!(crate::db::outbox::tests::wallet_events(&db,owner).await.is_empty());
    fixture::cleanup(&db,fixture).await.unwrap();
}
