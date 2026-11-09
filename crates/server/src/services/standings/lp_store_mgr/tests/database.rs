//! Persistent LP settlement regressions against isolated game rows and loaded SDE data.
use super::super::*;
use std::sync::Arc;
use infinity_core::{consts::{ACCOUNT_CASH, FLAG_HANGAR}};
use crate::db::Db;
use tokio::sync::mpsc;

#[derive(Clone, Copy)]
struct Fixture { account: i64, buyer: i64, member: i64, corp: i64, issuer: i64, station: i64, offer: i64, cost_type: i32, output_type: i32 }

async fn create(db: &Db, issuer: i64, station: i64, cost_type: i32, output_type: i32) -> Result<Fixture, crate::db::Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('lp-test-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let buyer: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, 'lp-test-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 1000) RETURNING character_id", &[&account, &station]).await?.get("character_id");
        let member: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, 'lp-test-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 1000) RETURNING character_id", &[&account, &station]).await?.get("character_id");
        let corp: i64 = tx.query_one("INSERT INTO game.corporations (name, ticker, ceo_id, creator_id, station_id, created) VALUES ('lp-test-' || nextval('game.corporations_corporation_id_seq')::text, 'L' || nextval('game.corporations_corporation_id_seq')::text, $1, $1, $2, 0) RETURNING corporation_id", &[&buyer, &station]).await?.get("corporation_id");
        tx.execute("UPDATE game.characters SET corporation_id = $1 WHERE character_id = ANY($2)", &[&corp, &vec![buyer, member]]).await?;
        tx.execute("UPDATE game.characters SET corp_roles = $1 WHERE character_id = $2", &[&infinity_core::consts::ROLE_DIRECTOR, &buyer]).await?;
        // The member may read wallet changes but cannot spend from the division.
        tx.execute("UPDATE game.characters SET corp_roles = $1 WHERE character_id = $2", &[&infinity_core::consts::ROLE_JUNIOR_ACCOUNTANT, &member]).await?;
        tx.execute("INSERT INTO game.corp_wallets (corporation_id, account_key, balance) VALUES ($1, $2, 1000)", &[&corp, &ACCOUNT_CASH]).await?;
        tx.execute("INSERT INTO game.lp_balances (char_id, corp_id, amount) VALUES ($1, $3, 100), ($2, $3, 100)", &[&buyer, &corp, &issuer]).await?;
        let offer = -buyer;
        let required = format!("[{{\"type_id\":{cost_type},\"quantity\":3}}]");
        tx.execute("INSERT INTO game.lp_offers (corporation_id, offer_id, type_id, quantity, lp_cost, isk_cost, ak_cost, required_items) VALUES ($1, $2, $3, 3, 10, 20, 0, $4::text::jsonb)", &[&issuer, &offer, &output_type, &required]).await?;
        for quantity in [4_i64, 10] {
            tx.execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5, false)", &[&cost_type, &buyer, &station, &FLAG_HANGAR, &quantity]).await?;
        }
        Ok(Fixture { account, buyer, member, corp, issuer, station, offer, cost_type, output_type })
    }), |fixture, ()| async move { Ok(fixture) }).await
}

async fn cleanup(db: &Db, f: Fixture) -> Result<(), crate::db::Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        crate::services::tests::fixture::cleanup_progress(tx, &[f.buyer, f.member, f.corp], &[f.buyer, f.member, f.corp]).await?;
        tx.execute("DELETE FROM game.lp_offers WHERE corporation_id = $1 AND offer_id = $2", &[&f.issuer, &f.offer]).await?;
        tx.execute("DELETE FROM game.lp_balances WHERE char_id = ANY($1) AND corp_id = $2", &[&vec![f.buyer, f.corp], &f.issuer]).await?;
        tx.execute("DELETE FROM game.items WHERE owner_id = ANY($1)", &[&vec![f.buyer, f.member, f.corp]]).await?;
        tx.execute("DELETE FROM game.corp_wallets WHERE corporation_id = $1", &[&f.corp]).await?;
        tx.execute("DELETE FROM game.corporations WHERE corporation_id = $1", &[&f.corp]).await?;
        tx.execute("DELETE FROM game.characters WHERE user_id = $1", &[&f.account]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&f.account]).await?;
        Ok(())
    }), |(), ()| async move { Ok(()) }).await
}

async fn snapshot(db: &Db, f: Fixture) -> String {
    db.query_one("SELECT json_build_object('characters', (SELECT json_agg(row_to_json(c) ORDER BY character_id) FROM game.characters c WHERE user_id = $1), 'wallets', (SELECT json_agg(row_to_json(w) ORDER BY account_key) FROM game.corp_wallets w WHERE corporation_id = $2), 'lp', (SELECT json_agg(row_to_json(l) ORDER BY char_id) FROM game.lp_balances l WHERE char_id = ANY($3) AND corp_id = $4), 'items', (SELECT json_agg(row_to_json(i) ORDER BY item_id) FROM game.items i WHERE owner_id = ANY($3)))::text AS state", &[&f.account, &f.corp, &vec![f.buyer, f.member, f.corp], &f.issuer]).await.unwrap().get("state")
}

async fn quantities(db: &Db, f: Fixture, kind: i32) -> Vec<i64> {
    db.query("SELECT quantity FROM game.items WHERE owner_id = $1 AND location_id = $2 AND flag_id = $3 AND type_id = $4 ORDER BY item_id", &[&f.buyer, &f.station, &FLAG_HANGAR, &kind]).await.unwrap().iter().map(|r| r.get("quantity")).collect()
}

fn drain(rx: &mut mpsc::UnboundedReceiver<(String, Vec<Value>)>) -> Vec<String> {
    let mut names = Vec::new();
    while let Ok((name, _)) = rx.try_recv() { names.push(name); }
    names
}
async fn direct_item_notices(client: &mut Client, peer: &mut tokio::net::TcpStream) -> usize {
    assert!(!client.wait_pumping(-1, 0).await.unwrap());
    let mut count = 0;
    while let Ok(Ok(Some(frame))) = tokio::time::timeout(std::time::Duration::from_millis(50), crate::framing::read_frame(peer)).await {
        let packet = crate::macho::Packet::from_value(&marshal::loads(&frame.payload).unwrap()).unwrap();
        if let crate::macho::Address::Broadcast { broadcast_id, .. } = packet.destination {
            if broadcast_id.as_text().as_deref() == Some("OnItemChange") { count += 1; }
        }
    }
    count
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn committed_settlement_and_refusals_are_atomic() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let issuer = sde.npc_corporations.iter().next().expect("SDE NPC corporation").corporation_id;
    let station = sde.map.stations().next().expect("SDE station").station_id;
    let (cost_type, output_type) = {
        let mut types = sde.types.iter().filter(|t| t.type_id > 0);
        (types.next().expect("SDE cost type").type_id, types.next().expect("second SDE type").type_id)
    };
    let f = create(&db, issuer, station, cost_type, output_type).await.unwrap();
    let test_db = db.clone();
    // The join boundary lets cleanup run even when an assertion panics.
    let result = tokio::spawn(async move {
        let world = crate::space::World::new(test_db.clone(), sde.clone(), 1);
        let (buyer_tx, mut buyer_notices) = mpsc::unbounded_channel();
        let (tx, mut member_notices) = mpsc::unbounded_channel();
        world.register_pilot(f.member, tx);
        let cfg = Arc::new(crate::config::Config::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let login = crate::handshake::Login { user_id: f.account, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), world.clone());
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, test_db.clone(), sde, chat, world.clone(), login);
        client.change_session(vec![("charid", Value::Int(f.buyer)), ("corpid", Value::Int(f.corp)), ("stationid", Value::Int(f.station)), ("corpAccountKey", Value::Int(i64::from(ACCOUNT_CASH)))]);
        world.register_pilot(f.buyer, buyer_tx);
        world.set_online_session(f.buyer, infinity_core::session::OnlineSession::from_session(&client.session));
        let mut member_session = infinity_core::session::OnlineSession::from_session(&client.session).expect("buyer online session");
        member_session.character_id = f.member;
        world.set_online_session(f.member, Some(member_session));
        assert!(take_offer(&mut client, f.issuer, f.offer, 2, false).await.is_ok());
        assert_eq!(quantities(&test_db, f, f.cost_type).await, vec![8]);
        assert_eq!(quantities(&test_db, f, f.output_type).await, vec![6]);
        assert_eq!(crate::services::wallets::balance(&test_db, f.buyer).await.unwrap_or_else(|_| panic!("character wallet")), crate::isk::IskAmount::from_cents(96_000).unwrap());
        assert_eq!(crate::services::standings::lp_balance(&test_db, f.buyer, f.issuer).await.unwrap_or_else(|_| panic!("character LP")), 80);
        assert!(crate::db::outbox::tests::events(&test_db, "LoyaltyPointsEarned", f.buyer).await.is_empty());
        let notices = drain(&mut buyer_notices);
        assert_eq!(notices.iter().filter(|n| *n == "OnItemChange").count(), 1);
        assert_eq!(direct_item_notices(&mut client, &mut peer).await, 2);
        assert!(notices.iter().any(|n| n == "OnAccountChange"));
        assert!(notices.iter().any(|n| n == "OnLPChange"));
        assert!(drain(&mut member_notices).is_empty());

        // Each refusal starts from a deliberately insufficient fixture and compares every owned row.
        for (index, (sql, restore)) in [
            ("UPDATE game.lp_balances SET amount = 0 WHERE char_id = $1 AND corp_id = $2", "UPDATE game.lp_balances SET amount = 80 WHERE char_id = $1 AND corp_id = $2"),
            ("UPDATE game.characters SET balance = 0 WHERE character_id = $1 AND $2::bigint IS NOT NULL", "UPDATE game.characters SET balance = 960 WHERE character_id = $1 AND $2::bigint IS NOT NULL"),
            ("UPDATE game.items SET quantity = 1 WHERE owner_id = $1 AND type_id = $2::bigint", "UPDATE game.items SET quantity = 8 WHERE owner_id = $1 AND type_id = $2::bigint"),
            ("UPDATE game.lp_offers SET ak_cost = 1 WHERE offer_id = $1 AND corporation_id = $2", "UPDATE game.lp_offers SET ak_cost = 0 WHERE offer_id = $1 AND corporation_id = $2"),
        ].into_iter().enumerate() {
            let a = if index == 3 { f.offer } else { f.buyer };
            let b = if index == 2 { i64::from(f.cost_type) } else { f.issuer };
            test_db.execute(sql, &[&a, &b]).await.unwrap();
            let before = snapshot(&test_db, f).await;
            assert!(take_offer(&mut client, f.issuer, f.offer, 2, false).await.is_err(), "refusal {index}");
            assert_eq!(snapshot(&test_db, f).await, before, "refusal {index} mutated rows");
            assert!(drain(&mut buyer_notices).is_empty());
            assert!(drain(&mut member_notices).is_empty());
            assert_eq!(direct_item_notices(&mut client, &mut peer).await, 0);
            test_db.execute(restore, &[&a, &b]).await.unwrap();
        }
        assert!(take_offer(&mut client, f.issuer, f.offer, 1, true).await.is_ok());
        assert_eq!(quantities(&test_db, f, f.cost_type).await, vec![5]);
        assert_eq!(quantities(&test_db, f, f.output_type).await, vec![9]);
        assert_eq!(crate::services::wallets::balance(&test_db, f.buyer).await.unwrap_or_else(|_| panic!("character wallet")), crate::isk::IskAmount::from_cents(96_000).unwrap());
        assert_eq!(crate::services::standings::lp_balance(&test_db, f.buyer, f.issuer).await.unwrap_or_else(|_| panic!("character LP")), 80);
        assert_eq!(crate::services::wallets::corp_balance(&test_db, f.corp, ACCOUNT_CASH).await.unwrap_or_else(|_| panic!("corporation wallet")), crate::isk::IskAmount::from_cents(98_000).unwrap());
        assert_eq!(crate::services::standings::lp_balance(&test_db, f.corp, f.issuer).await.unwrap_or_else(|_| panic!("corporation LP")), 90);
        assert!(crate::db::outbox::tests::events(&test_db, "LoyaltyPointsEarned", f.corp).await.is_empty());
        assert!(crate::db::outbox::tests::events(&test_db, "LoyaltyPointsEarned", f.buyer).await.is_empty());
        let notices = drain(&mut buyer_notices);
        assert_eq!(notices.iter().filter(|n| *n == "OnItemChange").count(), 1);
        assert_eq!(direct_item_notices(&mut client, &mut peer).await, 1);
        assert!(notices.iter().any(|n| n == "OnAccountChange"));
        assert!(notices.iter().any(|n| n == "OnCorpLPChange"));
        let notices = drain(&mut member_notices);
        assert!(notices.iter().any(|n| n == "OnAccountChange"));
        assert!(notices.iter().any(|n| n == "OnCorpLPChange"));
        // An odd whole-ISK price above f64's exact integer range must debit exactly.
        test_db.execute("UPDATE game.lp_offers SET isk_cost = 9007199254740993 WHERE corporation_id = $1 AND offer_id = $2", &[&f.issuer, &f.offer]).await.unwrap();
        test_db.execute("UPDATE game.characters SET balance = 9007199254740994.01 WHERE character_id = $1", &[&f.buyer]).await.unwrap();
        test_db.execute("UPDATE game.lp_balances SET amount = 0 WHERE char_id = $1 AND corp_id = $2", &[&f.buyer, &f.issuer]).await.unwrap();
        let before = snapshot(&test_db, f).await;
        assert!(take_offer(&mut client, f.issuer, f.offer, 1, false).await.is_err());
        assert_eq!(snapshot(&test_db, f).await, before);
        assert!(drain(&mut buyer_notices).is_empty());
        assert!(drain(&mut member_notices).is_empty());
        assert_eq!(direct_item_notices(&mut client, &mut peer).await, 0);
        test_db.execute("UPDATE game.lp_balances SET amount = 80 WHERE char_id = $1 AND corp_id = $2", &[&f.buyer, &f.issuer]).await.unwrap();
        assert!(take_offer(&mut client, f.issuer, f.offer, 1, false).await.is_ok());
        assert_eq!(crate::services::wallets::balance(&test_db, f.buyer).await.unwrap(), crate::isk::IskAmount::from_cents(101).unwrap());
        assert_eq!(crate::services::standings::lp_balance(&test_db, f.buyer, f.issuer).await.unwrap(), 70);
        assert_eq!(quantities(&test_db, f, f.cost_type).await, vec![2]);
        assert_eq!(quantities(&test_db, f, f.output_type).await, vec![12]);
        drain(&mut buyer_notices);
        assert_eq!(direct_item_notices(&mut client, &mut peer).await, 1);
        assert!(drain(&mut member_notices).is_empty());

        // Multiplication beyond the offer's integer domain must still refuse atomically.
        test_db.execute("UPDATE game.lp_offers SET isk_cost = 9223372036854775807 WHERE corporation_id = $1 AND offer_id = $2", &[&f.issuer, &f.offer]).await.unwrap();
        let before = snapshot(&test_db, f).await;
        assert!(take_offer(&mut client, f.issuer, f.offer, 2, false).await.is_err());
        assert_eq!(snapshot(&test_db, f).await, before);
        assert!(drain(&mut buyer_notices).is_empty());
        assert!(drain(&mut member_notices).is_empty());
        assert_eq!(direct_item_notices(&mut client, &mut peer).await, 0);
        test_db.execute("UPDATE game.lp_offers SET isk_cost = 0 WHERE corporation_id = $1 AND offer_id = $2", &[&f.issuer, &f.offer]).await.unwrap();
        test_db.execute("UPDATE game.items SET quantity = $3 WHERE owner_id = $1 AND type_id = $2", &[&f.buyer, &f.cost_type, &(i64::MAX - 1)]).await.unwrap();
        test_db.execute("UPDATE game.items SET quantity = $3 WHERE owner_id = $1 AND type_id = $2", &[&f.buyer, &f.output_type, &(i64::MAX - 1)]).await.unwrap();
        assert!(take_offer(&mut client, f.issuer, f.offer, 1, false).await.is_ok());
        assert_eq!(quantities(&test_db, f, f.cost_type).await, vec![i64::MAX - 4]);
        assert_eq!(quantities(&test_db, f, f.output_type).await, vec![i64::MAX, 2]);
        assert_eq!(crate::services::wallets::balance(&test_db, f.buyer).await.unwrap(), crate::isk::IskAmount::from_cents(101).unwrap());
        assert_eq!(crate::services::standings::lp_balance(&test_db, f.buyer, f.issuer).await.unwrap(), 60);
        let notices = drain(&mut buyer_notices);
        assert_eq!(notices.iter().filter(|n| *n == "OnItemChange").count(), 2);
        assert!(notices.iter().any(|n| n == "OnLPChange"));
        assert_eq!(direct_item_notices(&mut client, &mut peer).await, 1);
        assert!(drain(&mut member_notices).is_empty());
        client.change_session(vec![("charid", Value::Int(f.member))]);
        assert_eq!(drain(&mut member_notices), vec!["OnCharNoLongerInStation"]);
        let before = snapshot(&test_db, f).await;
        assert!(take_offer(&mut client, f.issuer, f.offer, 1, true).await.is_err());
        assert_eq!(snapshot(&test_db, f).await, before);
        assert!(drain(&mut buyer_notices).is_empty());
        let drained = drain(&mut member_notices);
        assert_eq!(drained, Vec::<String>::new());
        assert_eq!(direct_item_notices(&mut client, &mut peer).await, 0);
    }).await;
    cleanup(&db, f).await.expect("cleaning LP settlement fixture");
    result.expect("LP settlement regression failed");
}
