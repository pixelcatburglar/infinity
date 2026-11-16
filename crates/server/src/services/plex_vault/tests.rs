//! Exact vault arithmetic at storage boundaries.
use super::*;

#[test]
fn exact_domain_and_checked_changes() {
    assert_eq!(next_balance(MAX_BALANCE, VaultChange::Add(-1)), Some(MAX_BALANCE - 1));
    assert_eq!(next_balance(MAX_BALANCE, VaultChange::Add(1)), None);
    assert_eq!(next_balance(0, VaultChange::Add(-1)), None);
    assert_eq!(next_balance(0, VaultChange::Set(MAX_BALANCE)), Some(MAX_BALANCE));
    assert_eq!(next_balance(0, VaultChange::Set(-1)), None);
    assert_eq!(next_balance(MAX_BALANCE, VaultChange::Add(i128::MAX)), None);
}

#[test]
fn market_kinds_carry_only_their_real_identities() {
    let fill = VaultKind::MarketFill { order_id: 7, trade_id: 9 };
    assert_eq!(fill.as_str(), "market_fill");
    assert_eq!(fill.market_ids(), (Some(7), Some(9)));
    let refund = VaultKind::MarketRefund { order_id: 7 };
    assert_eq!(refund.as_str(), "market_refund");
    assert_eq!(refund.market_ids(), (Some(7), None));
    assert_eq!(VaultKind::GmAdd.market_ids(), (None, None));
}

#[test]
fn signed_projection_clamps_whole_then_multiplies() {
    let cap = i128::from(CLIENT_MAX_SHORT_AMOUNT);
    for (whole, expected) in [(0, 0), (1, 1), (-1, -1), (cap, 99_999_999_999_999), (-cap, -99_999_999_999_999),
        (cap + 1, 99_999_999_999_999), (-(cap + 1), -99_999_999_999_999), (i128::MAX, 99_999_999_999_999), (i128::MIN, -99_999_999_999_999)] {
        assert_eq!(projected_whole(whole), expected, "{whole}");
        assert_eq!(projected_cents(whole), expected * PLEX_HUNDREDTHS, "{whole}");
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE"]
async fn committed_change_notifies_bound_apps_and_rollback_stays_silent() {
    use crate::dao::accounts::plex::tests::{ledger_counts, ConversionFixture};
    use crate::db::outbox::{dispatch_world, Outbox};
    use crate::gateway::{protocol::target_group::Target, tests::{decode_notice, notice_stream}};
    use infinity_core::session::OnlineSession;
    use prost_reflect::DynamicMessage;
    fn cents(attrs: &DynamicMessage, field: &str) -> Option<i64> {
        attrs.get_field_by_name(field)?.as_message()?.get_field_by_name("total_in_cents")?.as_i64()
    }
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = ConversionFixture::create(&db, 60000001, "0").await.unwrap();
    let foreign = ConversionFixture::create(&db, 60000001, "0").await.unwrap();
    let (user, character) = (fixture.user, fixture.character);
    let world = crate::space::World::new(db.clone(), crate::tests::shared_sde(&db).await, 8);
    let register = |character: i64, user: i64| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        world.register_pilot(character, tx);
        world.set_online_session(character, Some(OnlineSession { character_id: character, user_id: Some(user), session_id: 1, ship_id: None, station_id: None, system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
        rx
    };
    let mut macho = register(character, user);
    let mut foreign_macho = register(foreign.character, foreign.user);
    let mut app_a = notice_stream(&world, [8; 16], character, 8);
    let mut app_b = notice_stream(&world, [9; 16], character, 8);
    let mut foreign_app = notice_stream(&world, [10; 16], foreign.character, 8);
    let context = VaultContext { kind: VaultKind::GmAdd, item_id: None, reference: None };
    let commit = |change: VaultChange| db.transaction(move |tx, outbox: &mut Outbox| Box::pin(async move {
        change_in(tx, outbox, user, Some(character), change, context, VaultAudience::Account).await
    }), |value, outbox| { let world = world.clone(); async move { dispatch_world(value, outbox, &world) } });
    let entry = commit(VaultChange::Add(5)).await.unwrap().unwrap();
    assert_eq!((entry.balance, entry.projected), (5, 5));
    assert_eq!(ledger_counts(&db, user).await, (2, 1));
    let id = plex::logged_ids(&db, user).await.unwrap()[0];
    let row = plex::logged_transaction(&db, user, id).await.unwrap().unwrap();
    assert_eq!((row.delta.as_str(), row.balance_after.as_str()), ("5", "5"));
    let (seconds, nanos) = crate::client::unix_parts_from_filetime(row.created);
    for app in [&mut app_a.rx, &mut app_b.rx] {
        let (envelope, payload) = decode_notice(&app.try_recv().expect("committed notice"));
        assert_eq!(envelope.target_group.unwrap().target, Some(Target::User(i32::try_from(user).unwrap())));
        assert_eq!(payload.get_field_by_name("identifier").unwrap().as_message().unwrap().get_field_by_name("sequential").unwrap().as_i64(), Some(id));
        let attrs = payload.get_field_by_name("attributes").unwrap().as_message().unwrap().clone();
        let ts = attrs.get_field_by_name("timestamp").unwrap().as_message().unwrap().clone();
        assert_eq!((ts.get_field_by_name("seconds").unwrap().as_i64(), ts.get_field_by_name("nanos").unwrap().as_i32()), (Some(seconds), Some(nanos)));
        assert_eq!(cents(&attrs, "amount_transferred"), Some(500));
        assert_eq!(cents(&attrs, "resulting_balance"), Some(500));
        assert!(app.try_recv().is_err());
    }
    assert_eq!(macho.try_recv().unwrap(), ("OnAurumChangeFromVgs".into(), vec![Value::dict([("userid", Value::Int(user)), ("balance", Value::Int(5))])]));
    assert!(foreign_app.rx.try_recv().is_err() && foreign_macho.try_recv().is_err());
    let rebound = fixture.another_character(&db).await.unwrap();
    let mut rebound_macho = register(rebound, user);
    assert!(world.unregister_pilot_generation(character, Some(1)));
    assert!(world.online_session(character).is_none());
    let mut rebound_app = notice_stream(&world, [8; 16], rebound, 8);
    let entry = commit(VaultChange::Set(100_000_000_000_004)).await.unwrap().unwrap();
    assert_eq!((entry.balance, entry.projected), (100_000_000_000_004_i128, 99_999_999_999_999));
    let row = plex::logged_transaction(&db, user, plex::logged_ids(&db, user).await.unwrap()[0]).await.unwrap().unwrap();
    assert_eq!((row.delta.as_str(), row.balance_after.as_str()), ("99999999999999", "100000000000004"));
    let (envelope, payload) = decode_notice(&rebound_app.rx.try_recv().expect("rebound app notice"));
    assert_eq!(envelope.target_group.unwrap().target, Some(Target::User(i32::try_from(user).unwrap())));
    let attrs = payload.get_field_by_name("attributes").unwrap().as_message().unwrap().clone();
    assert_eq!(cents(&attrs, "amount_transferred"), Some(9_999_999_999_999_900));
    assert_eq!(cents(&attrs, "resulting_balance"), Some(9_999_999_999_999_900));
    assert!(rebound_app.rx.try_recv().is_err() && app_a.rx.try_recv().is_err() && app_b.rx.try_recv().is_err());
    assert!(macho.try_recv().is_err());
    assert!(rebound_macho.try_recv().is_ok());
    assert!(foreign_app.rx.try_recv().is_err() && foreign_macho.try_recv().is_err());
    let entry = commit(VaultChange::Set(0)).await.unwrap().unwrap();
    assert_eq!(entry.balance, 0);
    let row = plex::logged_transaction(&db, user, plex::logged_ids(&db, user).await.unwrap()[0]).await.unwrap().unwrap();
    assert_eq!(row.delta, "-100000000000004");
    let (_, payload) = decode_notice(&rebound_app.rx.try_recv().expect("negative cap notice"));
    let attrs = payload.get_field_by_name("attributes").unwrap().as_message().unwrap().clone();
    assert_eq!(cents(&attrs, "amount_transferred"), Some(-9_999_999_999_999_900));
    assert_eq!(cents(&attrs, "resulting_balance"), Some(0));
    assert!(app_b.rx.try_recv().is_err() && macho.try_recv().is_err() && rebound_macho.try_recv().is_ok());
    let counts = ledger_counts(&db, user).await;
    let rolled: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        change_in(tx, outbox, user, Some(character), VaultChange::Add(1), context, VaultAudience::Account).await?;
        Err(CallError::Internal(anyhow::anyhow!("rollback")))
    }), |value, outbox| { let world = world.clone(); async move { dispatch_world(value, outbox, &world) } }).await;
    assert!(rolled.is_err());
    assert_eq!(ledger_counts(&db, user).await, counts);
    assert!(rebound_app.rx.try_recv().is_err() && app_b.rx.try_recv().is_err() && macho.try_recv().is_err());
    fixture.cleanup(&db).await.unwrap();
    foreign.cleanup(&db).await.unwrap();
}
