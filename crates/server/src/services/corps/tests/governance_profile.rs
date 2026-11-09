//! Corporation tax input validation shared by founding and updates.
use crate::services::corps::profile;
use marshal::Value;

#[test]
fn tax_requires_finite_unit_interval() {
    for value in [0.0, 0.25, 1.0] { assert_eq!(profile::tax(Some(&Value::Float(value))).unwrap(), value); }
    for value in [-0.01, 1.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] { assert!(profile::tax(Some(&Value::Float(value))).is_err()); }
}

#[test]
fn logo_requires_seven_optional_int32_values() {
    let mut values = vec![Value::Int(1); 6];
    values.push(Value::None);
    assert_eq!(profile::logo(&values).unwrap(), [Some(1), Some(1), Some(1), Some(1), Some(1), Some(1), None]);
    values[0] = Value::Int(i64::from(i32::MAX) + 1);
    assert!(profile::logo(&values).is_err());
    assert!(profile::logo(&values[..6]).is_err());
}

async fn update(client: &mut crate::client::Client, corp: i64, method: &str, args: &[Value]) -> crate::services::CallResult {
    let kwargs = Value::Dict(vec![]);
    let binding = Value::Int(corp);
    let call = crate::services::Call { service: "corpRegistry", method, args, kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
    super::super::dispatch(client, &call).await.unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn profile_rpc_is_atomic_and_notifies_actual_changes() {
    use infinity_core::consts::ROLE_DIRECTOR;
    use crate::{db::Db, services::tests::{self, fixture}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=$1,corp_roles=$2 WHERE character_id=$3", &[&corp, &ROLE_DIRECTOR, &ceo.character]).await.unwrap();
    db.execute("INSERT INTO game.corp_wallets(corporation_id,account_key,balance) VALUES ($1,1000,250)", &[&corp]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), ceo.character).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(ceo.character, tx);
        let args = [Value::unicode("new description"), Value::unicode("url"), Value::Float(0.25), Value::Bool(true), Value::Float(0.5)];
        update(&mut client, corp, "UpdateCorporation", &args).await.unwrap();
        let row = crate::dao::corporations::registry::corporation(&test_db, corp).await.unwrap().unwrap();
        assert_eq!((row.description.as_str(), row.tax_rate, row.lp_tax_rate), ("new description", 0.25, 0.5));
        assert!(rx.try_recv().is_ok_and(|(event, _)| event == "OnCorporationChanged"));
        let bad = [Value::unicode("discard"), Value::unicode("url"), Value::Float(f64::NAN), Value::Bool(false), Value::Float(0.0)];
        assert!(update(&mut client, corp, "UpdateCorporation", &bad).await.is_err());
        assert_eq!(crate::dao::corporations::registry::corporation(&test_db, corp).await.unwrap().unwrap().description, "new description");
        let mut founding = vec![Value::None; 15];
        founding[0] = Value::unicode("invalid-tax-corporation");
        founding[1] = Value::unicode("ITC");
        founding[4] = Value::Float(f64::INFINITY);
        founding[14] = Value::Float(0.0);
        assert!(update(&mut client, corp, "AddCorporation", &founding).await.is_err());
        founding[4] = Value::Float(0.0);
        founding[14] = Value::Float(f64::NAN);
        assert!(update(&mut client, corp, "AddCorporation", &founding).await.is_err());
        let names: Vec<Value> = (0..14).map(|index| Value::unicode(format!("division-{index}"))).collect();
        update(&mut client, corp, "UpdateDivisionNames", &names).await.unwrap();
        let row = crate::dao::corporations::registry::corporation(&test_db, corp).await.unwrap().unwrap();
        assert_eq!(row.division_names.unwrap()[13].as_deref(), Some("division-13"));
        assert!(rx.try_recv().is_ok_and(|(event, _)| event == "OnCorporationChanged"));
        let logo = [Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4), Value::Int(5), Value::Int(6), Value::None];
        update(&mut client, corp, "UpdateLogo", &logo).await.unwrap();
        let row = crate::dao::corporations::registry::corporation(&test_db, corp).await.unwrap().unwrap();
        assert_eq!(row.logo, [Some(1), Some(2), Some(3), Some(4), Some(5), Some(6), None]);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|(event, _)| event == "OnAccountChange"));
        assert!(events.iter().any(|(event, _)| event == "OnCorporationChanged"));
        let balance = crate::dao::wallets::corporations::balance(&test_db, corp, infinity_core::consts::ACCOUNT_CASH).await.unwrap();
        assert_eq!(balance, crate::isk::IskAmount::from_cents(15_000).unwrap());
        update(&mut client, corp, "UpdateLogo", &logo).await.unwrap();
        let balance = crate::dao::wallets::corporations::balance(&test_db, corp, infinity_core::consts::ACCOUNT_CASH).await.unwrap();
        assert_eq!(balance, crate::isk::IskAmount::from_cents(15_000).unwrap());
        assert!(rx.try_recv().is_err());
        let mut bad_logo = logo;
        bad_logo[0] = Value::Int(i64::from(i32::MAX) + 1);
        assert!(update(&mut client, corp, "UpdateLogo", &bad_logo).await.is_err());
        client.session.apply(vec![("charid", Value::Int(outsider.character))]);
        assert!(update(&mut client, corp, "UpdateCorporation", &args).await.is_err());
        test_db.execute("UPDATE game.characters SET corporation_id=$1,corp_roles=$2 WHERE character_id=$3", &[&corp, &ROLE_DIRECTOR, &outsider.character]).await.unwrap();
        assert!(update(&mut client, corp, "UpdateDivisionNames", &names).await.is_err());
        let director_logo = [Value::Int(7), Value::Int(2), Value::Int(3), Value::Int(4), Value::Int(5), Value::Int(6), Value::None];
        update(&mut client, corp, "UpdateLogo", &director_logo).await.unwrap();
        let balance = crate::dao::wallets::corporations::balance(&test_db, corp, infinity_core::consts::ACCOUNT_CASH).await.unwrap();
        assert_eq!(balance, crate::isk::IskAmount::from_cents(5_000).unwrap());
        while rx.try_recv().is_ok() {}
        let too_expensive = [Value::Int(8), Value::Int(2), Value::Int(3), Value::Int(4), Value::Int(5), Value::Int(6), Value::None];
        assert!(update(&mut client, corp, "UpdateLogo", &too_expensive).await.is_err());
        let row = crate::dao::corporations::registry::corporation(&test_db, corp).await.unwrap().unwrap();
        assert_eq!(row.logo[0], Some(7));
        assert!(rx.try_recv().is_err());
        let balance = crate::dao::wallets::corporations::balance(&test_db, corp, infinity_core::consts::ACCOUNT_CASH).await.unwrap();
        assert_eq!(balance, crate::isk::IskAmount::from_cents(5_000).unwrap());
        test_db.execute("UPDATE game.corporations SET ceo_id=$2 WHERE corporation_id=$1", &[&corp, &outsider.character]).await.unwrap();
        test_db.execute("UPDATE game.characters SET corp_roles=0 WHERE character_id=$1", &[&ceo.character]).await.unwrap();
        client.session.apply(vec![("charid", Value::Int(ceo.character))]);
        assert!(update(&mut client, corp, "UpdateLogo", &director_logo).await.is_err());
    }).await;
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0 WHERE character_id=ANY($1)", &[&[ceo.character, outsider.character].as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, outsider).await.unwrap();
    result.unwrap();
}
