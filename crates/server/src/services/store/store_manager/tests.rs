use super::*;

mod purchasing;
mod acquisition;

#[test]
fn service_prices_use_only_positive_retail_prices() {
    assert_eq!(service_price(None, 100.0, 50), 50);
    assert_eq!(service_price(Some(0.0), 100.0, 50), 50);
    assert_eq!(service_price(Some(-1.0), 100.0, 50), 50);
    assert_eq!(service_price(Some(240.0), 100.0, 50), 2);
    assert_eq!(service_price(Some(1.0), 100.0, 50), 1);
}

#[test]
fn retail_price_filter_matches_postgres_float_ordering() {
    use crate::dao::store::catalogue::positive_price;
    assert!(positive_price(f64::NAN));
    assert!(positive_price(f64::INFINITY));
    assert!(positive_price(1.0));
    assert!(!positive_price(0.0));
    assert!(!positive_price(-1.0));
    assert!(!positive_price(f64::NEG_INFINITY));
    assert_eq!(service_price(Some(f64::NAN), 100.0, 50), 1);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn unavailable_crates_are_not_listed_or_purchasable() {
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    client.login.user_id = actor.account;
    purchasing::set_balance(&db, actor, 1_000_000).await;
    let type_id = client.sde.crates.iter().find(|d| !d.sale_supported()).expect("catalogue requires an unavailable crate").type_id;
    let balance = crate::dao::accounts::balances::plex_balance(&db, actor.account).await.unwrap();
    let tokens = crate::dao::store::purchases::tokens(&db, actor.account).await.unwrap();
    let purchases = fixture::store_purchase_count(&db, actor.account, type_id).await.unwrap();
    let token_ids: Vec<_> = tokens.iter().map(|t| t.token_id).collect();
    let kwargs = Value::Dict(vec![]);
    for method in ["get_products", "get_offers"] {
        let call = Call { service: "storeManager", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let reply = crate::services::dispatch(&mut client, &call).await.unwrap();
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), reply);
        for row in reply.items().unwrap() {
            assert_ne!(row.get("id").and_then(|v| v.as_int()), Some(i64::from(type_id)));
        }
    }
    for (method, id, expected) in [
        ("can_purchase_offer", Value::str(format!("VEVC-{type_id}")), Value::dict([("canPurchase", Value::Bool(false))])),
        ("is_offer_purchasable", Value::Int(i64::from(type_id)), Value::Bool(false)),
    ] {
        for keyword in [false, true] {
            let args = if keyword { vec![] } else { vec![id.clone(), Value::str("period")] };
            let kwargs = if keyword { Value::dict([("offer_id", id.clone())]) } else { Value::Dict(vec![]) };
            let call = Call { service: "storeManager", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let reply = crate::services::dispatch(&mut client, &call).await.unwrap();
            assert_eq!(reply, expected);
            assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), expected);
        }
    }
    for (method, id, expected) in [
        ("can_purchase_offer", Value::str("VEVC-1"), Value::dict([("canPurchase", Value::Bool(true))])),
        ("is_offer_purchasable", Value::Int(DAILY_GIFT_OFFER), Value::Bool(true)),
        ("can_purchase_offer", Value::str("VEVC--1"), Value::dict([("canPurchase", Value::Bool(false))])),
        ("is_offer_purchasable", Value::Int(-1), Value::Bool(false)),
    ] {
        let args = [id];
        let call = Call { service: "storeManager", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let reply = crate::services::dispatch(&mut client, &call).await.unwrap();
        assert_eq!(reply, expected);
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), expected);
    }
    let cat = catalogue(&client).await.unwrap();
    let ordinary = cat.offers.iter().find(|o| !o.daily && client.sde.crates.get(o.type_id).is_none()).unwrap();
    for (method, id, expected) in [
        ("can_purchase_offer", Value::str(format!("VEVC-{}", ordinary.id)), Value::dict([("canPurchase", Value::Bool(true))])),
        ("is_offer_purchasable", Value::Int(ordinary.id), Value::Bool(true)),
    ] {
        let args = [id];
        let call = Call { service: "storeManager", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), expected);
    }
    let args = [Value::Int(i64::from(type_id)), Value::str("PLX"), Value::Int(1)];
    let call = Call { service: "storeManager", method: "buy_offer", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    let reply = crate::services::dispatch(&mut client, &call).await.unwrap();
    assert_eq!(reply, Value::Bool(false));
    assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), reply);
    assert_eq!(crate::dao::accounts::balances::plex_balance(&db, actor.account).await.unwrap(), balance);
    assert_eq!(fixture::store_purchase_count(&db, actor.account, type_id).await.unwrap(), purchases);
    assert_eq!(crate::dao::store::purchases::tokens(&db, actor.account).await.unwrap().iter().map(|t| t.token_id).collect::<Vec<_>>(), token_ids);
    assert!(client.outbox.is_empty());
    let args = [Value::Int(DAILY_GIFT_OFFER)];
    let call = Call { service: "storeManager", method: "free_to_get", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::Bool(true));
    for (method, id, expected) in [
        ("can_purchase_offer", Value::str("VEVC-1"), Value::dict([("canPurchase", Value::Bool(false))])),
        ("is_offer_purchasable", Value::Int(DAILY_GIFT_OFFER), Value::Bool(false)),
    ] {
        let args = [id];
        let call = Call { service: "storeManager", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), expected);
    }
    let token_ids: Vec<_> = crate::dao::store::purchases::tokens(&db, actor.account).await.unwrap().iter().map(|t| t.token_id).collect();
    crate::dao::store::purchases::trash(&db, actor.account, &token_ids).await.unwrap();
    crate::dao::accounts::plex::tests::cleanup(&db, actor.account).await;
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn external_period_marking_is_refused_without_side_effects() {
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    client.login.user_id = actor.account;
    purchasing::set_balance(&db, actor, 1_000_000).await;
    let balance = crate::dao::accounts::balances::plex_balance(&db, actor.account).await.unwrap();
    let purchases = fixture::store_purchase_count(&db, actor.account, DAILY_GIFT_OFFER as i32).await.unwrap();
    let token_ids: Vec<_> = crate::dao::store::purchases::tokens(&db, actor.account).await.unwrap().iter().map(|t| t.token_id).collect();
    assert!(client.outbox.is_empty());
    let args = [Value::Int(DAILY_GIFT_OFFER), Value::str("2026-01-01T00:00:00Z")];
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "storeManager", method: "set_offer_purchased", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    match crate::services::dispatch(&mut client, &call).await {
        Err(CallError::User { msg, dict }) => {
            assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
            assert_eq!(dict, vec![("notify".into(), Value::unicode(messages::store::EXTERNAL_PAYMENT_UNSUPPORTED))]);
        }
        other => panic!("expected external period marking refusal: {other:?}"),
    }
    assert_eq!(crate::dao::accounts::balances::plex_balance(&db, actor.account).await.unwrap(), balance);
    assert_eq!(fixture::store_purchase_count(&db, actor.account, DAILY_GIFT_OFFER as i32).await.unwrap(), purchases);
    assert_eq!(crate::dao::store::purchases::tokens(&db, actor.account).await.unwrap().iter().map(|t| t.token_id).collect::<Vec<_>>(), token_ids);
    assert!(client.outbox.is_empty());
    crate::dao::accounts::plex::tests::cleanup(&db, actor.account).await;
    fixture::cleanup(&db, actor).await.unwrap();
}
