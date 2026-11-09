use super::*;
use crate::services::{self, CallError};

const WRITES: [&str; 5] = ["AttemptLinkToMainBank", "AttemptLinkToReserveBank", "RequestMainBankUnlink", "RequestReserveBankUnlink", "RequestUnlockReserveBank"];

fn call<'a>(service: &'a str, method: &'a str, kwargs: &'a Value) -> Call<'a> {
    call_args(service, method, &[], kwargs, false)
}

fn call_args<'a>(service: &'a str, method: &'a str, args: &'a [Value], kwargs: &'a Value, bound: bool) -> Call<'a> {
    Call { service, method, args, kwargs, bound, bind_params: None }
}

#[test]
fn closed_ess_wire_shapes_and_fallthrough() {
    let kw = Value::Dict(vec![]);
    let solar_system = [Value::Int(30_000_142)];
    for (service, method, expected) in [
        ("dynamicResourceCacheMgr", "GetDBSMapData", Value::Dict(vec![])),
        ("dynamicResourceCacheMgr", "GetESSAgencyData", Value::Dict(vec![])),
        ("essMgr", "GetDataForClientSolarSystem", Value::None),
        ("essMgr", "GetMainBankTheftsForClientSolarSystem", Value::list([])),
        ("essMgr", "GetReserveBankTheftsForClientSolarSystem", Value::list([])),
        ("essMgr", "IsClientLinkedToReserveBank", Value::Bool(false)),
        ("dynamicBountyMgr", "GetOutputForClientSolarSystem", Value::tuple([Value::None, Value::Bool(false)])),
    ] {
        let c = call(service, method, &kw);
        let actual = universe(&c).or_else(|| station(&c)).or_else(|| social(&c)).unwrap().unwrap();
        assert_eq!(actual, expected);
        assert_eq!(marshal::loads(&marshal::dumps(&actual)).unwrap(), expected);
    }
    let keys_call = call_args("dynamicResourceCacheMgr", "GetESSReserveBankKeys", &solar_system, &kw, false);
    let keys = universe(&keys_call).unwrap().unwrap();
    assert_eq!(keys, Value::list([]));
    assert_eq!(marshal::loads(&marshal::dumps(&keys)).unwrap(), keys);
    let details_call = call_args("dynamicResourceCacheMgr", "GetESSSystemDetails", &solar_system, &kw, false);
    let details = universe(&details_call).unwrap().unwrap();
    assert_eq!(details, Value::dict([
        ("autopaymentWindowState", Value::Int(0)), ("activeLinks", Value::Int(0)),
        ("totalPulses", Value::Int(0)), ("reserveTimeWindowState", Value::Int(0)),
        ("reserveTimeChosen", Value::Int(0)),
    ]));
    assert_eq!(marshal::loads(&marshal::dumps(&details)).unwrap(), details);
    for method in WRITES { refusal(root(&call("essMgr", method, &kw)).unwrap().unwrap_err()); }
    for (service, method) in [("essMgr", "Unknown"), ("dynamicResourceCacheMgr", "GetESSSystemDetail"), ("other", "AttemptLinkToMainBank")] {
        let c = call(service, method, &kw);
        assert!(root(&c).is_none() && universe(&c).is_none() && station(&c).is_none() && social(&c).is_none());
    }
}


fn refusal(error: CallError) {
    let CallError::User { msg, dict } = error else { panic!("expected UserError") };
    assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
    assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::ess::UNAVAILABLE))]);
    let encoded = crate::client::user_error(&msg, dict);
    assert_eq!(marshal::loads(&marshal::dumps(&encoded)).unwrap(), encoded);
}


/// Evaluate both branches of dynamicresources/common/ess/formulas.py:GetPayout.
#[test]
fn closed_settings_satisfy_client_tooltips_and_payout_arithmetic() {
    let kw = Value::Dict(vec![]);
    let settings = universe(&call("dynamicResourceCacheMgr", "GetDynamicResourceSettings", &kw)).unwrap().unwrap();
    assert_eq!(marshal::loads(&marshal::dumps(&settings)).unwrap(), settings);
    let field = |name| services::value_field(&settings, name).unwrap().as_float().unwrap();
    for name in ["timeIntervalSeconds", "autopaymentIntervalSeconds", "bufferTimeSeconds", "reservePulseIntervalSeconds", "reserveWindowAccessFactor", "reserveBankTimeAccessFactor"] {
        assert!(field(name) > 0.0);
    }
    for name in ["bountiesCut", "mainBankPayRatio", "reserveBankMaxPayout", "reservePayoutPaddingValue", "earlyPayoutThresholdFactor", "latePayoutThresholdFactor"] {
        assert_eq!(field(name), 0.0);
    }
    assert!(field("maxOutput") > field("minOutput"));
    assert!(field("equilibriumValue") >= field("minOutput"));
    let total = 4.0;
    let window = total / field("reserveWindowAccessFactor");
    assert!(window.is_finite() && window > 0.0);
    for pulse in [1.0_f64, 4.0] {
        let payout = if pulse < window {
            field("reserveBankMaxPayout") * (pulse / window).powf(field("reservePayoutExponentLead")) + field("reservePayoutPaddingValue")
        } else {
            field("reserveBankMaxPayout") * (window / pulse).powf(field("reservePayoutExponentTail")) + field("reservePayoutPaddingValue")
        };
        assert_eq!(payout, 0.0);
    }
}

async fn snapshot(db: &crate::db::Db) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for table in ["characters", "items", "wallet_accounts", "wallet_journal", "jobs"] {
        rows.push(services::drones::tests::fixture::table_fingerprint(db, table).await.unwrap());
    }
    rows
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn real_dispatch_refusals_preserve_owner_other_and_connection() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = services::tests::fixture::create(&db).await.unwrap();
    let other = services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        // SDE essreservekeys keyTypeID fixture.
        let key_type = 57714;
        crate::dao::items::transfers::insert(&test_db, key_type, owner.character, owner.character, infinity_core::consts::FLAG_HANGAR, 3).await.unwrap();
        crate::dao::items::transfers::insert(&test_db, key_type, other.character, other.character, infinity_core::consts::FLAG_HANGAR, 3).await.unwrap();
        let (mut client, peer) = services::tests::client(test_db.clone(), owner.character).await;
        client.login.user_id = owner.account;
        let empty = Value::Dict(vec![]);
        let kwargs = Value::dict([("unexpected", Value::Bool(true))]);
        let valid_unlock_args = [Value::Int(i64::from(key_type))];
        let malformed_args = [Value::Int(i64::from(key_type)), Value::Int(other.character)];
        for role in [0, i64::MAX] {
            client.login.role = role;
            client.session.apply(vec![("role", Value::Int(role))]);
            for method in WRITES {
                let authentic_args: &[Value] = if method == "RequestUnlockReserveBank" { &valid_unlock_args } else { &[] };
                for (arguments, kw, bound) in [(authentic_args, &empty, false), (&malformed_args[..], &kwargs, true)] {
                    let before = snapshot(&test_db).await;
                    let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
                    let outbox = client.outbox.len();
                    let c = Call { service: "essMgr", method, args: arguments, kwargs: kw, bound, bind_params: None };
                    refusal(services::dispatch(&mut client, &c).await.unwrap_err());
                    assert_eq!(snapshot(&test_db).await, before);
                    assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
                    assert_eq!(client.outbox.len(), outbox);
                    let mut byte = [0];
                    assert_eq!(peer.try_read(&mut byte).unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
                }
            }
        }
    }).await;
    services::tests::fixture::cleanup(&db, owner).await.unwrap();
    services::tests::fixture::cleanup(&db, other).await.unwrap();
    result.unwrap();
}
