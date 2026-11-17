use super::*;

#[test]
fn wider_snapshots_validate_and_clamp_symmetrically() {
    let cap = personal_client_cap();
    let max = "99999999999999999999999999999999999.99";
    let min = "-99999999999999999999999999999999999.99";
    for value in [max, "+00010000000000000000000000000000000000.0000"] {
        assert_eq!(journal_balance_client_isk(value, cap).unwrap(), cap.wire_isk());
    }
    assert_eq!(journal_balance_client_isk(min, cap).unwrap(), -cap.wire_isk());
    for (value, expected) in [("-123.45", -123.45), ("+0001.2300", 1.23), ("-0.000", 0.0), ("1", 1.0), ("1.2", 1.2)] {
        assert_eq!(journal_balance_client_isk(value, cap).unwrap(), expected);
        assert_eq!(IskAmount::parse_decimal(value).unwrap().client_isk(cap), expected);
    }
    for value in ["", ".1", "NaN", "Infinity", "-inf", "1.001", "1.00.0", "1e2", "--1", "100000000000000000000000000000000000.00", "-100000000000000000000000000000000000.00", "1."] {
        assert!(journal_balance_client_isk(value, cap).is_err(), "{value}");
        assert!(IskAmount::parse_decimal(value).is_err(), "{value}");
    }
}

#[test]
fn negative_and_zero_are_distinct() {
    assert!(validate(IskAmount::from_cents(-1).unwrap()).is_err());
    assert!(validate(IskAmount::ZERO).is_ok());
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn large_personal_owner_uses_personal_wallet() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let location = crate::services::tests::fixture::default_location(crate::tests::shared_sde(&db).await.as_ref());
    let _economy = crate::services::market::lock().await;
    let character = 1_500_000_123_i64;
    let result: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        crate::services::tests::fixture::create_with_character_id(tx, character, location).await?;
        tx.execute("UPDATE game.characters SET balance=100 WHERE character_id=$1", &[&character]).await.map_err(db_err)?;
        let owner = ownership::FinancialOwner::cash(character);
        ownership::debit(tx, outbox, owner, IskAmount::from_cents(2_500).unwrap()).await?;
        let after_debit = balance(tx, character).await?;
        ownership::credit(tx, outbox, owner, IskAmount::from_cents(1_000).unwrap()).await?;
        let wallets: i64 = tx.query_one("SELECT count(*) AS count FROM game.corp_wallets WHERE corporation_id = $1", &[&character]).await.map_err(db_err)?.get("count");
        if after_debit != IskAmount::from_cents(7_500).unwrap() || balance(tx, character).await? != IskAmount::from_cents(8_500).unwrap() || wallets != 0 {
            return Err(CallError::Internal(anyhow::anyhow!("owner wallet facts differed")));
        }
        Err(CallError::Internal(anyhow::anyhow!("owner fixture rollback")))
    }), |(), _| async { unreachable!("rolled-back owner fixture must not dispatch") }).await;
    let Err(CallError::Internal(error)) = result else { panic!("owner fixture must roll back") };
    assert_eq!(error.to_string(), "owner fixture rollback");
    assert!(db.query_opt("SELECT character_id FROM game.characters WHERE character_id = $1", &[&character]).await.unwrap().is_none());
}
