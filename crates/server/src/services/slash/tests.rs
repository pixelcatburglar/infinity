use super::*;
pub(in crate::services::slash) mod support;
mod feedback;
mod stack_quantities;

#[test]
fn list_limit_preserves_boundary_and_counts_omissions() {
    let rows = vec!["row"; LIST_ROW_LIMIT].join("\n");
    assert_eq!(limit_list(rows.clone(), false), rows);
    assert_eq!(limit_list(format!("{rows}\nrow"), false), format!("{rows}\n{}", messages::gm::gm_rows_truncated(1)));
    assert_eq!(limit_list(format!("{rows}\nrow\nrow\nrow"), false), format!("{rows}\n{}", messages::gm::gm_rows_truncated(3)));
    let with_header = format!("header\n{rows}");
    assert_eq!(limit_list(with_header.clone(), true), with_header);
    assert_eq!(limit_list(format!("{with_header}\nrow"), true), format!("{with_header}\n{}", messages::gm::gm_rows_truncated(1)));
    let rendered = vec!["row"; LIST_ROW_LIMIT].join("\n");
    assert_eq!(finish_list(rendered.clone(), 0), rendered);
    assert_eq!(finish_list(rendered.clone(), 2), format!("{rendered}\n{}", messages::gm::gm_rows_truncated(2)));
}

#[test]
fn isk_grammar_and_argument_count() {
    let parse = |args: &[&str]| parse_amount(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(), ISK_INTEGER_DIGITS, ISK_DECIMALS, false);
    for (input, expected) in [("0", "0"), ("1.2", "1.2"), (",1,,2,.3,4,", "12.34")] {
        assert_eq!(parse(&[input]).as_deref(), Some(expected));
    }
    for input in ["", ",,,", ".1", "1.", "1.234", "1.2.3", "-1", "+1", "1e2", "１２", "1a"] {
        assert_eq!(parse(&[input]), None, "{input}");
    }
    assert!(parse(&[]).is_none());
    assert!(parse(&["1", "2"]).is_none());
    assert!(parse(&[&"9".repeat(34)]).is_some());
    assert!(parse(&[&"9".repeat(35)]).is_none());
    assert!(parse(&["1.23"]).is_some());
    assert!(parse(&["1.234"]).is_none());
}

#[test]
fn isk_command_decimal_boundary_is_exact() {
    let maximum = "9999999999999999999999999999999999.99";
    let parsed = parse_amount(&[maximum.into()], ISK_INTEGER_DIGITS, ISK_DECIMALS, false).unwrap();
    assert_eq!(crate::isk::IskAmount::parse_decimal(&parsed).unwrap().decimal(), maximum);
    assert!(parse_amount(&["10000000000000000000000000000000000".into()], ISK_INTEGER_DIGITS, ISK_DECIMALS, false).is_none());
}

#[test]
fn addisk_signed_grammar_preserves_digit_limit() {
    let parse = |s: &str| parse_amount(&[s.into()], ISK_INTEGER_DIGITS, ISK_DECIMALS, true);
    assert_eq!(parse("-1,234.56").as_deref(), Some("-1234.56"));
    assert_eq!(parse("+0.01").as_deref(), Some("+0.01"));
    assert!(parse(&format!("-{}.99", "9".repeat(34))).is_some());
    assert!(parse(&format!("-{}", "9".repeat(35))).is_none());
    for input in ["-", "+", "--1", "+-1", "-.1", "-1.", "-1.234"] {
        assert!(parse(input).is_none(), "{input}");
    }
}

#[test]
fn plex_grammar_and_argument_count() {
    let parse = |args: &[&str]| parse_amount(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(), PLEX_DIGITS, 0, false);
    assert_eq!(parse(&[",1,,2,"]).as_deref(), Some("12"));
    assert!(parse(&[&"9".repeat(36)]).is_some());
    assert!(parse(&[&"9".repeat(37)]).is_none());
    for input in ["", ",", "1.0", "-1", "+1", "1e2", "１２"] {
        assert_eq!(parse(&[input]), None, "{input}");
    }
    assert!(parse(&[]).is_none());
    assert!(parse(&["1", "2"]).is_none());
}

#[test]
fn exact_balance_clamp_and_echo() {
    let isk_cap = format!("{CLIENT_MAX_ISK:.0}");
    assert!(!exceeds_cap(&format!("000{isk_cap}.00"), &isk_cap));
    assert!(exceeds_cap(&format!("{isk_cap}.01"), &isk_cap));
    assert!(!exceeds_cap("0000.00", &isk_cap));
    assert!(exceeds_cap("1000", "999"));
    assert!(!exceeds_cap("998.99", "999"));
    assert!(exceeds_cap("999.01", "999"));
    let plex_cap = CLIENT_MAX_SHORT_AMOUNT.to_string();
    assert!(!exceeds_cap(&format!("000{plex_cap}"), &plex_cap));
    assert!(exceeds_cap(&format!("{plex_cap}0"), &plex_cap));
    assert_eq!(messages::gm::gm_balance("1.23", None), "balance 1.23 ISK");
    assert_eq!(messages::gm::gm_balance("999.01", Some(999.0)), "balance 999.01 ISK (client shows 999)");
    assert_eq!(messages::gm::gm_plex_balance("12", None), "vault 12 PLEX");
    assert_eq!(messages::gm::gm_plex_balance("1000", Some(999)), "vault 1000 PLEX (client shows 999)");
}

#[test]
fn dogma_grammar() {
    use dogma::{Action, Target};
    for (args, expected) in [
        (vec!["ship", "list"], (Target::Ship, Action::List)),
        (vec!["flag:27", "damageMultiplier", "3"], (Target::Flag(27), Action::Set("damageMultiplier", 3.0))),
        (vec!["123", "37"], (Target::Item(123), Action::Query("37"))),
        (vec!["ship", "37", "reset"], (Target::Ship, Action::Reset("37"))),
    ] {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(dogma::parse(&owned), Some(expected));
    }
    for args in [&[][..], &["ship"][..], &["flag:x", "37"][..], &["flag:-1", "37"][..],
        &["0", "37"][..], &["ship", "", "1"][..], &["ship", "37", "NaN"][..],
        &["ship", "37", "inf"][..], &["ship", "37", "-inf"][..],
        &["ship", "37", "x"][..], &["ship", "list", "1"][..], &["ship", "37", "1", "2"][..]] {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(dogma::parse(&owned), None, "{args:?}");
    }
}

#[test]
fn dogma_runtime_and_character_attributes_are_denied_for_all_actions() {
    use dogma::{denied, Action};
    for args in [&["ship", "skillPoints"][..], &["ship", "skillPoints", "1"][..], &["ship", "skillPoints", "reset"][..]] {
        let owned: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        let (_, action) = dogma::parse(&owned).unwrap();
        assert_eq!(match action { Action::Query(a) | Action::Set(a, _) | Action::Reset(a) => a, Action::List => panic!("expected attribute action") }, "skillPoints");
        assert_eq!(denied(9999, |name, id| name == "skillPoints" && id == 9999), Some(messages::gm::GM_DOGMA_RUNTIME));
        for id in [164, 165, 166, 167, 168] {
            assert_eq!(denied(id, |_, _| false), Some(messages::gm::GM_DOGMA_CHARACTER));
        }
        for id in [163, 169] {
            assert_eq!(denied(id, |_, _| false), None);
        }
        assert_eq!(denied(9999, |_, _| false), None);
    }
}

async fn isk_command(client: &mut Client, command: &str, amount: &str) -> CallResult {
    let args = [Value::unicode(format!("/{command} {amount}"))];
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "slash", method: "SlashCmd", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    execute(client, &call).await.unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn gm_isk_preserves_cents_above_cap_and_rolls_back_overflow() {
    use crate::isk::IskAmount;
    use crate::lua::events::GameEvent;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), fixture.character).await;
        client.login.role = GM_ROLES;
        let maximum = "9999999999999999999999999999999999.99";
        let initial = crate::services::wallets::balance(&test_db, fixture.character).await.unwrap();
        let before = crate::db::outbox::tests::wallet_events(&test_db, fixture.character).await.len();
        isk_command(&mut client, "setisk", maximum).await.unwrap();
        let events = crate::db::outbox::tests::wallet_events(&test_db, fixture.character).await;
        let balance = IskAmount::parse_decimal(maximum).unwrap();
        assert_eq!(crate::services::wallets::balance(&test_db, fixture.character).await.unwrap(), balance);
        assert!(matches!(events.last(), Some(GameEvent::WalletChanged { balance: actual, delta, .. }) if *actual == balance && *delta == balance.checked_sub(initial).unwrap()));
        assert_eq!(events.len(), before + 1);
        assert_eq!(client.outbox.len(), 1);
        client.outbox.clear();
        assert!(isk_command(&mut client, "addisk", "0.01").await.is_err());
        assert_eq!(crate::services::wallets::balance(&test_db, fixture.character).await.unwrap(), balance);
        assert_eq!(crate::db::outbox::tests::wallet_events(&test_db, fixture.character).await.len(), events.len());
        assert!(client.outbox.is_empty());
        for input in ["0", "+0", "-0.00"] {
            let row = test_db.query_one("SELECT ctid::text AS tuple_id FROM game.characters WHERE character_id = $1", &[&fixture.character]).await.unwrap();
            let before_tuple: String = row.get("tuple_id");
            isk_command(&mut client, "addisk", input).await.unwrap();
            let row = test_db.query_one("SELECT ctid::text AS tuple_id FROM game.characters WHERE character_id = $1", &[&fixture.character]).await.unwrap();
            assert_eq!(row.get::<_, String>("tuple_id"), before_tuple);
            assert_eq!(crate::services::wallets::balance(&test_db, fixture.character).await.unwrap(), balance);
            assert_eq!(crate::db::outbox::tests::wallet_events(&test_db, fixture.character).await.len(), events.len());
            assert!(client.outbox.is_empty());
        }
        for (input, expected, delta) in [
            ("-0.01", "9999999999999999999999999999999999.98", "-0.01"),
            ("+0.01", maximum, "0.01"),
        ] {
            let before_count = crate::db::outbox::tests::wallet_events(&test_db, fixture.character).await.len();
            let reply = isk_command(&mut client, "addisk", input).await.unwrap();
            let expected = IskAmount::parse_decimal(expected).unwrap();
            assert_eq!(reply, Value::unicode(messages::gm::gm_balance(&expected.decimal(), Some(expected.client_isk(crate::isk::personal_client_cap())))));
            assert_eq!(crate::services::wallets::balance(&test_db, fixture.character).await.unwrap(), expected);
            let events = crate::db::outbox::tests::wallet_events(&test_db, fixture.character).await;
            assert_eq!(events.len(), before_count + 1);
            assert!(matches!(events.last(), Some(GameEvent::WalletChanged { balance, delta: actual, .. }) if *balance == expected && *actual == IskAmount::parse_decimal(delta).unwrap()));
            assert_eq!(client.outbox.len(), 1);
            assert!(matches!(&client.outbox[0].destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnAccountChange")));
            client.outbox.clear();
        }
        assert!(isk_command(&mut client, "setisk", "-1").await.is_err());
        assert!(client.outbox.is_empty());
        isk_command(&mut client, "setisk", "1.23").await.unwrap();
        client.outbox.clear();
        let count = crate::db::outbox::tests::wallet_events(&test_db, fixture.character).await.len();
        let Err(CallError::User { msg, dict }) = isk_command(&mut client, "addisk", "-1.24").await else { panic!("expected insufficient balance") };
        assert_eq!(msg, messages::MSG_NOT_ENOUGH_MONEY);
        assert_eq!(dict, vec![("balance".into(), Value::Float(1.23)), ("amount".into(), Value::Float(1.24))]);
        assert_eq!(crate::services::wallets::balance(&test_db, fixture.character).await.unwrap(), IskAmount::parse_decimal("1.23").unwrap());
        assert_eq!(crate::db::outbox::tests::wallet_events(&test_db, fixture.character).await.len(), count);
        assert!(client.outbox.is_empty());
        isk_command(&mut client, "addisk", "-1.23").await.unwrap();
        assert_eq!(crate::services::wallets::balance(&test_db, fixture.character).await.unwrap(), IskAmount::ZERO);
        let events = crate::db::outbox::tests::wallet_events(&test_db, fixture.character).await;
        assert_eq!(events.len(), count + 1);
        assert!(matches!(events.last(), Some(GameEvent::WalletChanged { balance, delta, .. }) if *balance == IskAmount::ZERO && *delta == IskAmount::parse_decimal("-1.23").unwrap()));
        assert_eq!(client.outbox.len(), 1);
        client.outbox.clear();
        let row = test_db.query_one("SELECT ctid::text AS tuple_id FROM game.characters WHERE character_id = $1", &[&fixture.character]).await.unwrap();
        let before_tuple: String = row.get("tuple_id");
        isk_command(&mut client, "addisk", "-0.00").await.unwrap();
        let row = test_db.query_one("SELECT ctid::text AS tuple_id FROM game.characters WHERE character_id = $1", &[&fixture.character]).await.unwrap();
        assert_eq!(row.get::<_, String>("tuple_id"), before_tuple);
        assert_eq!(crate::services::wallets::balance(&test_db, fixture.character).await.unwrap(), IskAmount::ZERO);
        assert_eq!(crate::db::outbox::tests::wallet_events(&test_db, fixture.character).await.len(), events.len());
        assert!(client.outbox.is_empty());
    }).await;
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn gm_plex_wallet_uses_exact_ledger_and_refuses_overflow_atomically() {
    use crate::dao::accounts::plex::{self, tests::{ConversionFixture, ledger_counts, ledger_references}};

    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), 0).await;
    let station = client.sde.map.stations().next().expect("SDE station").station_id;
    let fixture = ConversionFixture::create(&db, station, "0").await.unwrap();
    let other_character = fixture.another_character(&db).await.unwrap();
    client.login.user_id = fixture.user;
    client.session.apply(vec![("charid", Value::Int(fixture.character))]);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    client.world.register_pilot(other_character, sender);

    let maximum = "9".repeat(36);
    let almost_maximum = format!("{}8", "9".repeat(35));
    for (command, amount, exact, projected, count) in [
        ("setplex", almost_maximum.as_str(), almost_maximum.as_str(), CLIENT_MAX_SHORT_AMOUNT, 2),
        ("addplex", "1", maximum.as_str(), CLIENT_MAX_SHORT_AMOUNT, 3),
    ] {
        let reply = wallet(&mut client, &[amount.to_owned()], command).await.unwrap();
        assert_eq!(reply, messages::gm::gm_plex_balance(exact, Some(projected)));
        assert_eq!(plex::balance(&db, fixture.user).await.unwrap(), exact);
        assert_eq!(ledger_counts(&db, fixture.user).await, (count, 1));
        let notice = Value::dict([("userid", Value::Int(fixture.user)), ("balance", Value::Int(projected))]);
        assert_eq!(receiver.try_recv().unwrap(), ("OnAurumChangeFromVgs".into(), vec![notice.clone()]));
        assert!(receiver.try_recv().is_err());
        assert_eq!(client.outbox.len(), 1);
        let packet = &client.outbox[0];
        assert!(matches!(&packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnAurumChangeFromVgs")));
        let envelope = packet.body[0].items().unwrap();
        assert_eq!(marshal::loads(envelope[1].as_bytes().unwrap()).unwrap(), Value::tuple([Value::Int(1), Value::tuple([notice])]));
        client.outbox.clear();
    }

    assert!(wallet(&mut client, &["1".into()], "addplex").await.is_err());
    assert_eq!(plex::balance(&db, fixture.user).await.unwrap(), maximum);
    assert_eq!(ledger_counts(&db, fixture.user).await, (3, 1));
    assert_eq!(ledger_references(&db, fixture.user).await.unwrap(), vec![None; 3]);
    assert!(client.outbox.is_empty());
    assert!(receiver.try_recv().is_err());

    for (command, amount, exact, projected, count) in [
        ("setplex", "0", "0", 0, 4),
        ("addplex", "12", "12", 12, 5),
    ] {
        let reply = wallet(&mut client, &[amount.to_owned()], command).await.unwrap();
        assert_eq!(reply, messages::gm::gm_plex_balance(exact, None));
        assert_eq!(plex::balance(&db, fixture.user).await.unwrap(), exact);
        assert_eq!(ledger_counts(&db, fixture.user).await, (count, 1));
        let notice = Value::dict([("userid", Value::Int(fixture.user)), ("balance", Value::Int(projected))]);
        assert_eq!(receiver.try_recv().unwrap(), ("OnAurumChangeFromVgs".into(), vec![notice.clone()]));
        assert!(receiver.try_recv().is_err());
        assert_eq!(client.outbox.len(), 1);
        let packet = &client.outbox[0];
        assert!(matches!(&packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnAurumChangeFromVgs")));
        let envelope = packet.body[0].items().unwrap();
        assert_eq!(marshal::loads(envelope[1].as_bytes().unwrap()).unwrap(), Value::tuple([Value::Int(1), Value::tuple([notice])]));
        client.outbox.clear();
    }
    assert_eq!(ledger_references(&db, fixture.user).await.unwrap(), vec![None; 5]);
    client.world.unregister_pilot_generation(other_character, None);
    fixture.cleanup(&db).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn gm_loyalty_adjustment_does_not_emit_earnings() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), fixture.character).await;
        let issuer = client.sde.npc_corporations.iter().next().expect("SDE NPC corporation").corporation_id;
        social::command(&mut client, "setlp", &[issuer.to_string(), "7".into()]).await.unwrap();
        social::command(&mut client, "addlp", &[issuer.to_string(), "2".into()]).await.unwrap();
        assert_eq!(crate::services::standings::lp_balance(&test_db, fixture.character, issuer).await.unwrap(), 9);
        assert!(crate::db::outbox::tests::events(&test_db, "LoyaltyPointsEarned", fixture.character).await.is_empty());
    }).await;
    db.execute("DELETE FROM game.lp_balances WHERE char_id=$1", &[&fixture.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
