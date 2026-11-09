//! Paid store RPC integration against the dedicated test database and SDE.
use super::*;
use crate::{db::Db, services::tests::fixture};

pub(super) async fn set_balance(db: &Db, actor: fixture::Fixture, balance: i128) {
    use crate::{db::outbox::Outbox, services::plex_vault::{change_in, VaultAudience, VaultChange, VaultContext, VaultKind}};
    let _lock = crate::services::market::lock().await;
    let result: Result<(), CallError> = db.transaction(
        |tx, outbox: &mut Outbox| Box::pin(async move {
            assert!(change_in(tx, outbox, actor.account, Some(actor.character), VaultChange::Set(balance),
                VaultContext { kind: VaultKind::GmSet, item_id: None, reference: None }, VaultAudience::Account).await?.is_some());
            Ok(())
        }),
        |(), _outbox: Outbox| async { Ok(()) },
    ).await;
    result.unwrap();
}

async fn setup() -> (Db, fixture::Fixture, Client, tokio::net::TcpStream) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, peer) = crate::services::tests::client(db.clone(), actor.character).await;
    client.login.user_id = actor.account;
    (db, actor, client, peer)
}

async fn ordinary(client: &Client) -> (i64, i32, i64, i64) {
    let cat = catalogue(client).await.unwrap();
    let offer = cat.offers.iter().find(|o| !o.daily && client.sde.crates.get(o.type_id).is_none() && o.price > 0 && o.price < 1_000_000).expect("an affordable ordinary offer");
    (offer.id, offer.type_id, offer.quantity, offer.price)
}

async fn buy(client: &mut Client, args: &[Value], kwargs: Value) -> Value {
    let call = Call { service: "storeManager", method: "buy_offer", args, kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::dispatch(client, &call).await.unwrap()
}

async fn state(db: &Db, payer: i64, recipient: i64) -> (String, i64, i64) {
    let row = db.query_one("SELECT plex_balance::text AS balance, (SELECT count(*) FROM game.store_purchases WHERE user_id=$1) AS purchases, (SELECT count(*) FROM game.redeem_tokens WHERE user_id=$2) AS tokens FROM game.accounts WHERE user_id=$1", &[&payer, &recipient]).await.unwrap();
    (row.get("balance"), row.get("purchases"), row.get("tokens"))
}

fn notifications(client: &mut Client) -> Vec<(String, Vec<Value>)> {
    std::mem::take(&mut client.outbox).into_iter().map(|packet| {
        let crate::macho::Address::Broadcast { broadcast_id, .. } = packet.destination else { panic!("expected broadcast notification") };
        let envelope = packet.body[0].items().unwrap();
        let decoded = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
        let fields = decoded.items().unwrap();
        (broadcast_id.as_text().unwrap().into_owned(), fields[1].items().unwrap().to_vec())
    }).collect()
}

async fn cleanup(db: &Db, actor: fixture::Fixture) {
    let tokens = crate::dao::store::purchases::tokens(db, actor.account).await.unwrap();
    let ids: Vec<_> = tokens.iter().map(|token| token.token_id).collect();
    crate::dao::store::purchases::trash(db, actor.account, &ids).await.unwrap();
    crate::dao::accounts::plex::tests::cleanup(db, actor.account).await;
    fixture::cleanup(db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn ordinary_purchase_authenticates_payer_and_delivers_quantity() {
    let (db, actor, mut client, _peer) = setup().await;
    let other = fixture::create(&db).await.unwrap();
    let (offer, ty, quantity, price) = ordinary(&client).await;
    set_balance(&db, actor, 10_000_000).await;
    let before_other = state(&db, other.account, other.account).await;
    let kwargs = Value::dict([("from_character_id", Value::Int(other.character))]);
    assert_eq!(buy(&mut client, &[Value::Int(offer), Value::str("PLX"), Value::Int(2)], kwargs).await, Value::Bool(true));
    assert_eq!(state(&db, actor.account, actor.account).await, ((10_000_000 - 2 * price).to_string(), 1, 1));
    assert_eq!(state(&db, other.account, other.account).await, before_other);
    let row = db.query_one("SELECT offer_id, quantity, price FROM game.store_purchases WHERE user_id=$1", &[&actor.account]).await.unwrap();
    assert_eq!((row.get::<_, i32>("offer_id"), row.get::<_, i64>("quantity"), row.get::<_, i64>("price")), (offer as i32, 2, 2 * price));
    let tokens = crate::dao::store::purchases::tokens(&db, actor.account).await.unwrap();
    assert_eq!((tokens[0].type_id, tokens[0].quantity), (ty, 2 * quantity));
    let notices = notifications(&mut client);
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0], ("OnAurumChangeFromVgs".into(), vec![Value::dict([("userid", Value::Int(actor.account)), ("balance", Value::Int(10_000_000 - 2 * price))])]));
    assert_eq!(notices[1], ("OnRedeemingQueueUpdated".into(), vec![]));
    cleanup(&db, other).await;
    cleanup(&db, actor).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn invalid_purchase_arguments_have_no_effect_and_none_targets_self() {
    let (db, actor, mut client, _peer) = setup().await;
    let (offer, _, _, price) = ordinary(&client).await;
    set_balance(&db, actor, 10_000_000).await;
    let before = state(&db, actor.account, actor.account).await;
    let empty = || Value::Dict(vec![]);
    let valid = || vec![Value::Int(offer), Value::str("PLX"), Value::Int(1)];
    let cases = vec![
        (vec![Value::Int(offer), Value::str("plx"), Value::Int(1)], empty()),
        (vec![Value::Int(offer)], empty()),
        (vec![Value::Int(offer), Value::str("PLX")], empty()),
        (vec![Value::str("bad"), Value::str("PLX"), Value::Int(1)], empty()),
        (vec![Value::Int(i64::MAX), Value::str("PLX"), Value::Int(1)], empty()),
        (vec![Value::Int(offer), Value::None, Value::Int(1)], empty()),
        (vec![Value::Int(DAILY_GIFT_OFFER), Value::str("PLX"), Value::Int(1)], empty()),
        (vec![Value::Int(offer), Value::str("PLX"), Value::Int(0)], empty()),
        (vec![Value::Int(offer), Value::str("PLX"), Value::Int(-1)], empty()),
        (vec![Value::Int(offer), Value::str("PLX"), Value::Int(i64::MAX)], empty()),
        (vec![], empty()),
        (vec![Value::Int(i64::from(i32::MAX)), Value::str("PLX"), Value::Int(1)], empty()),
        (vec![Value::Int(offer), Value::str("PLX"), Value::Bool(true)], empty()),
        (vec![Value::Int(offer), Value::str("PLX"), Value::Float(1.0)], empty()),
        (vec![Value::Int(offer), Value::str("PLX"), Value::str("1")], empty()),
        (valid(), Value::dict([("to_character_id", Value::str("bad"))])),
        (valid(), Value::dict([("to_character_id", Value::Int(0))])),
        (valid(), Value::dict([("to_character_id", Value::Int(-1))])),
        (valid(), Value::dict([("to_character_id", Value::Int(i64::MAX))])),
    ];
    for (args, kwargs) in cases {
        assert_eq!(buy(&mut client, &args, kwargs).await, Value::Bool(false));
        assert_eq!(state(&db, actor.account, actor.account).await, before);
        assert!(client.outbox.is_empty());
    }
    for kwargs in [empty(), Value::dict([("to_character_id", Value::None)])] {
        assert_eq!(buy(&mut client, &valid(), kwargs).await, Value::Bool(true));
    }
    assert_eq!(state(&db, actor.account, actor.account).await, ((10_000_000 - 2 * price).to_string(), 2, 2));
    cleanup(&db, actor).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn online_gift_routes_queue_to_account_transport_only() {
    let (db, actor, mut client, _peer) = setup().await;
    let recipient = fixture::create(&db).await.unwrap();
    let (offer, ty, quantity, price) = ordinary(&client).await;
    set_balance(&db, actor, 10_000_000).await;
    let registration = crate::live_sessions::register(recipient.account, false).unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let guard = registration.attach_sender(sender).unwrap();
    let kwargs = Value::dict([("to_character_id", Value::Int(recipient.character))]);
    assert_eq!(buy(&mut client, &[Value::Int(offer), Value::str("PLX"), Value::Int(2)], kwargs).await, Value::Bool(true));
    assert_eq!(receiver.try_recv().unwrap(), ("OnRedeemingQueueUpdated".into(), vec![]));
    assert!(receiver.try_recv().is_err());
    assert_eq!(notifications(&mut client), vec![("OnAurumChangeFromVgs".into(), vec![Value::dict([("userid", Value::Int(actor.account)), ("balance", Value::Int(10_000_000 - 2 * price))])])]);
    let tokens = crate::dao::store::purchases::tokens(&db, recipient.account).await.unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!((tokens[0].type_id, i64::from(tokens[0].quantity)), (ty, 2 * quantity));
    assert!(crate::dao::store::purchases::tokens(&db, actor.account).await.unwrap().is_empty());
    drop(guard);
    drop(registration);
    cleanup(&db, recipient).await;
    cleanup(&db, actor).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn deleted_gift_recipient_is_rejected_without_effects() {
    let (db, actor, mut client, _peer) = setup().await;
    let recipient = fixture::create(&db).await.unwrap();
    let (offer, _, _, _) = ordinary(&client).await;
    set_balance(&db, actor, 10_000_000).await;
    db.execute("UPDATE game.characters SET deleted = true WHERE character_id = $1", &[&recipient.character]).await.unwrap();
    let before_payer = state(&db, actor.account, actor.account).await;
    let before_recipient = state(&db, actor.account, recipient.account).await;
    let registration = crate::live_sessions::register(recipient.account, false).unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let guard = registration.attach_sender(sender).unwrap();
    let kwargs = Value::dict([("to_character_id", Value::Int(recipient.character))]);
    assert_eq!(buy(&mut client, &[Value::Int(offer), Value::str("PLX"), Value::Int(1)], kwargs).await, Value::Bool(false));
    assert_eq!(state(&db, actor.account, actor.account).await, before_payer);
    assert_eq!(state(&db, actor.account, recipient.account).await, before_recipient);
    assert!(client.outbox.is_empty());
    assert!(receiver.try_recv().is_err());
    drop(guard);
    drop(registration);
    cleanup(&db, recipient).await;
    cleanup(&db, actor).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn oversized_balance_debits_exactly_and_notifies_clamped_projection() {
    let (db, actor, mut client, _peer) = setup().await;
    let (offer, _, _, price) = ordinary(&client).await;
    let exact = "999999999999999999999999999999999999";
    set_balance(&db, actor, exact.parse().unwrap()).await;
    assert_eq!(buy(&mut client, &[Value::Int(offer), Value::str("PLX"), Value::Int(1)], Value::Dict(vec![])).await, Value::Bool(true));
    let expected = (exact.parse::<u128>().unwrap() - price as u128).to_string();
    assert_eq!(state(&db, actor.account, actor.account).await, (expected, 1, 1));
    let notices = notifications(&mut client);
    assert_eq!(notices[0], ("OnAurumChangeFromVgs".into(), vec![Value::dict([("userid", Value::Int(actor.account)), ("balance", Value::Int(infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT))])]));
    assert_eq!(notices[1], ("OnRedeemingQueueUpdated".into(), vec![]));
    assert_eq!(notices.len(), 2);
    cleanup(&db, actor).await;
}
