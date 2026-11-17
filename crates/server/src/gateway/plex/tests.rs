use super::*;
use crate::dao::accounts::plex::tests::{set_balance, opening_id, ConversionFixture};

/// CLIENT_MAX_SHORT_AMOUNT * PLEX_HUNDREDTHS.
const CENTS_CAP: i64 = 9_999_999_999_999_900;

fn cents(msg: &DynamicMessage, field: &str) -> Option<i64> {
    msg_field(msg, field)?.get_field_by_name("total_in_cents")?.as_i64()
}

#[test]
fn amounts_and_attributes_apply_the_shared_signed_projection() {
    validate().unwrap();
    let cap = i128::from(infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT);
    assert_eq!(exact_whole("0").unwrap(), 0);
    assert_eq!(exact_whole("-12345").unwrap(), -12345);
    assert_eq!(exact_whole(&i128::MAX.to_string()).unwrap(), i128::MAX);
    assert_eq!(exact_whole("170141183460469231731687303715884105728").unwrap(), i128::MAX);
    assert_eq!(exact_whole("-170141183460469231731687303715884105729").unwrap(), i128::MIN);
    assert!(exact_whole("12x").is_err() && exact_whole("").is_err() && exact_whole("-").is_err());
    let row = attributes(116_444_736_000_000_007, -(cap + 1), cap + 5).unwrap();
    let ts = msg_field(&row, "timestamp").unwrap();
    assert_eq!(ts.get_field_by_name("seconds").unwrap().as_i64(), Some(0));
    assert_eq!(ts.get_field_by_name("nanos").unwrap().as_i32(), Some(700));
    assert_eq!(cents(&row, "amount_transferred"), Some(-CENTS_CAP));
    assert_eq!(cents(&row, "resulting_balance"), Some(CENTS_CAP));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE"]
async fn claimed_reads_projection_authorization_and_notice_consistency() {
    let gw = crate::gateway::tests::gateway().await;
    let db = gw.db.clone();
    let fixture = ConversionFixture::create(&db, 60000001, "0").await.unwrap();
    let foreign = ConversionFixture::create(&db, 60000001, "0").await.unwrap();
    let empty = ConversionFixture::create(&db, 60000001, "0").await.unwrap();
    let user = fixture.user;
    let ctx = |user: Option<i64>| HandlerContext::for_request(&gw, crate::gateway::Caller { user_id: user, character_id: None });
    let request = |name: &str| DynamicMessage::new(message(name).unwrap());
    let refusal = |result: &Result<DynamicMessage, GatewayError>| match result {
        Err(GatewayError::Refused { code, .. }) => code.status(),
        _ => panic!("expected refusal"),
    };
    let anonymous = ctx(None).await.unwrap();
    for name in [BALANCE, ALL_LOGGED, LOG, STATISTICS] {
        assert_eq!(refusal(&handle(&anonymous, name, &request(name)).await), 401, "{name}");
    }
    plex::opening(&db, user, "10", 50).await.unwrap();
    plex::opening(&db, empty.user, "10", 50).await.unwrap();
    let gain = plex::append(&db, user, None, "5", "15", "gm_add", 200, None, None, None, None).await.unwrap();
    let loss = plex::append(&db, user, None, "-3", "12", "gm_remove", 200, None, None, None, None).await.unwrap();
    let zero = plex::append(&db, user, None, "0", "12", "gm_add", 100, None, None, None, None).await.unwrap();
    let over = plex::append(&db, user, None, "-999999999999999999999999999999999999", "0", "gm_remove", 300, None, None, None, None).await.unwrap();
    let theirs = plex::append(&db, foreign.user, None, "7", "7", "gm_add", 400, None, None, None, None).await.unwrap();
    let authed = ctx(Some(user)).await.unwrap();
    let opening = opening_id(&db, user).await.unwrap();
    set_balance(&db, user, "12345").await.unwrap();
    let balance = handle(&authed, BALANCE, &request(BALANCE)).await.unwrap();
    assert_eq!(cents(&balance, "balance"), Some(1_234_500));
    set_balance(&db, user, "100000000000004").await.unwrap();
    let balance = handle(&authed, BALANCE, &request(BALANCE)).await.unwrap();
    assert_eq!(cents(&balance, "balance"), Some(CENTS_CAP));
    let all = handle(&authed, ALL_LOGGED, &request(ALL_LOGGED)).await.unwrap();
    let ids: Vec<i64> = all.get_field_by_name("transactions").unwrap().as_list().unwrap().iter()
        .map(|v| v.as_message().unwrap().get_field_by_name("sequential").unwrap().as_i64().unwrap()).collect();
    assert_eq!(ids, plex::logged_ids(&db, user).await.unwrap());
    assert_eq!(ids, vec![over, loss, gain, zero]);
    let mut getlog = request(LOG);
    let id = identifier(&getlog.descriptor(), "identifier", PV::I64(gain)).unwrap();
    set(&mut getlog, "identifier", PV::Message(id)).unwrap();
    let log = handle(&authed, LOG, &getlog).await.unwrap();
    let transaction = msg_field(&log, "transaction").unwrap();
    let ts = msg_field(&transaction, "timestamp").unwrap();
    let (seconds, nanos) = crate::client::unix_parts_from_filetime(200);
    assert_eq!((ts.get_field_by_name("seconds").unwrap().as_i64(), ts.get_field_by_name("nanos").unwrap().as_i32()), (Some(seconds), Some(nanos)));
    assert_eq!(cents(&transaction, "amount_transferred"), Some(500));
    assert_eq!(cents(&transaction, "resulting_balance"), Some(1_500));
    assert_eq!(log.get_field_by_name("unavailable").unwrap().as_bool(), Some(true));
    assert!(!log.has_field_by_name("invoice_entry"));
    assert_eq!(refusal(&handle(&authed, LOG, &request(LOG)).await), 400);
    for bad in [0, -7, i64::MAX, theirs, opening] {
        let mut req = request(LOG);
        let id = identifier(&req.descriptor(), "identifier", PV::I64(bad)).unwrap();
        set(&mut req, "identifier", PV::Message(id)).unwrap();
        assert_eq!(refusal(&handle(&authed, LOG, &req).await), if bad <= 0 { 400 } else { 404 }, "{bad}");
    }
    let stats = handle(&authed, STATISTICS, &request(STATISTICS)).await.unwrap();
    let entries_value = stats.get_field_by_name("entries").unwrap().into_owned();
    let entries = entries_value.as_list().unwrap();
    assert_eq!(entries.len(), 1);
    let entry = entries[0].as_message().unwrap();
    assert_eq!(entry.get_field_by_name("no_classification").unwrap().as_bool(), Some(true));
    assert!(!entry.has_field_by_name("category"));
    assert_eq!(cents(entry, "incomes"), Some(500));
    assert_eq!(cents(entry, "expenses"), Some(-CENTS_CAP));
    assert_eq!(entry.get_field_by_name("transactions_count").unwrap().as_u32(), Some(4));
    let earliest = msg_field(&stats, "earliest_transaction").unwrap();
    let (seconds, nanos) = crate::client::unix_parts_from_filetime(100);
    assert_eq!((earliest.get_field_by_name("seconds").unwrap().as_i64(), earliest.get_field_by_name("nanos").unwrap().as_i32()), (Some(seconds), Some(nanos)));
    let stats = handle(&ctx(Some(empty.user)).await.unwrap(), STATISTICS, &request(STATISTICS)).await.unwrap();
    assert!(stats.get_field_by_name("entries").unwrap().as_list().unwrap().is_empty());
    let earliest = msg_field(&stats, "earliest_transaction").unwrap();
    assert_eq!((earliest.get_field_by_name("seconds").unwrap().as_i64(), earliest.get_field_by_name("nanos").unwrap().as_i32()), (Some(0), Some(0)));
    for _ in 0..172 { // beyond i128::MAX only through the exact SUM, not any single row.
        plex::append(&db, foreign.user, None, "999999999999999999999999999999999999", "0", "gm_add", 1, None, None, None, None).await.unwrap();
    }
    let stats = handle(&ctx(Some(foreign.user)).await.unwrap(), STATISTICS, &request(STATISTICS)).await.unwrap();
    let entries_value = stats.get_field_by_name("entries").unwrap().into_owned();
    let entries = entries_value.as_list().unwrap();
    assert_eq!(cents(entries[0].as_message().unwrap(), "incomes"), Some(CENTS_CAP));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    gw.world.register_pilot(fixture.character, tx);
    gw.world.set_online_session(fixture.character, Some(infinity_core::session::OnlineSession { character_id: fixture.character, user_id: Some(user), session_id: 1, ship_id: None, station_id: None, system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
    let mut stream = crate::gateway::tests::notice_stream(&gw.world, [7; 16], fixture.character, 8);
    gw.notice_hub.dispatch(balance_changed_notice(user, gain, 200, 5, 15).unwrap(), |id| gw.world.online_session(id));
    let (envelope, payload) = crate::gateway::tests::decode_notice(&stream.rx.try_recv().unwrap());
    assert_eq!(envelope.target_group.unwrap().target, Some(Target::User(i32::try_from(user).unwrap())));
    assert_eq!(msg_field(&payload, "identifier").unwrap().get_field_by_name("sequential").unwrap().as_i64(), Some(gain));
    assert_eq!(format!("{}", msg_field(&payload, "attributes").unwrap()), format!("{transaction}"));
    assert!(stream.rx.try_recv().is_err());
    fixture.cleanup(&db).await.unwrap();
    foreign.cleanup(&db).await.unwrap();
    empty.cleanup(&db).await.unwrap();
}

