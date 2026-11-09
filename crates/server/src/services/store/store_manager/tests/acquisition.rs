//! Expert catalogue and physical redemption coverage using the initialized SDE.
use super::*;
use crate::{db::Db, services::tests::fixture};

async fn call(client: &mut Client, service: &str, method: &str, args: &[Value], kwargs: &Value) -> Result<Value, CallError> {
    crate::services::dispatch(client, &Call { service, method, args, kwargs, bound: false, bind_params: None }).await
}

#[test]
fn hidden_and_retired_experts_are_not_saleable() {
    let mut definition = crate::sde::expert_systems::ExpertSystem { type_id: 1, duration_days: 1, duration_filetime: 1, skills: vec![], associated_ship_types: vec![], hidden: false, retired: false };
    assert!(expert_saleable(&definition));
    definition.hidden = true;
    assert!(!expert_saleable(&definition));
    definition.hidden = false;
    definition.retired = true;
    assert!(!expert_saleable(&definition));
}

#[test]
fn expert_offer_wire_shape_roundtrips() {
    for (type_id, price) in [(57207_i32, 15), (86227_i32, 50)] {
        let id = i64::from(type_id);
        let offer = Offer { id, type_id, quantity: 1, name: "expert".into(), image: "icon".into(), category: 202, price, daily: false };
        let expected_product = Value::dict([
            ("id", Value::Int(id)), ("name", Value::unicode("expert")), ("href", Value::unicode("")),
            ("typeId", Value::Int(id)), ("quantity", Value::Int(1)), ("productName", Value::unicode("expert")),
            ("imageUrl", Value::unicode("icon")),
        ]);
        let product = product_json(&offer);
        assert_eq!(product, expected_product);
        let json = offer_json(&offer, true);
        assert_eq!(json, Value::dict([
            ("id", Value::Int(id)), ("name", Value::unicode("expert")), ("description", Value::unicode("")),
            ("href", Value::unicode("")),
            ("offerPricings", Value::list([Value::dict([("currency", Value::str("PLX")), ("price", Value::Int(price)), ("basePrice", Value::Int(price))])])),
            ("imageUrl", Value::unicode("icon")), ("products", Value::list([expected_product])),
            ("categories", Value::list([Value::dict([("id", Value::Int(202))])])),
            ("label", Value::None), ("canPurchase", Value::Bool(true)), ("singlePurchase", Value::Bool(false)),
            ("sortPriority", Value::Int(id)),
        ]));
        assert_eq!(marshal::loads(&marshal::dumps(&product)).unwrap(), product);
        assert_eq!(marshal::loads(&marshal::dumps(&json)).unwrap(), json);
    }
    let category = category_json(&Category { id: 202, name: "expert".into(), parent: Some(2), home: false });
    assert_eq!(category, Value::dict([
        ("id", Value::Int(202)), ("name", Value::unicode("expert")), ("href", Value::unicode("")),
        ("parent", Value::dict([("id", Value::Int(2))])), ("tags", Value::list([Value::unicode("expert system")])),
        ("isHomePage", Value::Bool(false)), ("sortPriority", Value::Int(202)),
    ]));
    assert_eq!(marshal::loads(&marshal::dumps(&category)).unwrap(), category);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn expert_catalogue_routes_through_dispatch() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    client.login.user_id = actor.account;
    let kwargs = Value::Dict(vec![]);
    for method in ["get_categories", "get_products", "get_offers", "get_offer_states"] {
        let reply = call(&mut client, "storeManager", method, &[], &kwargs).await.unwrap();
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), reply);
        let rows = reply.items().unwrap();
        if method == "get_categories" {
            let cat = rows.iter().find(|r| r.get("id") == Some(&Value::Int(EXPERT_CATEGORY))).unwrap();
            assert_eq!(cat.get("tags"), Some(&Value::list([s(EXPERT_CATEGORY_TAG)])));
            assert_eq!(cat.get("parent").unwrap().get("id"), Some(&Value::Int(2)));
        } else {
            for (id, price) in [(57207, 15), (86227, 50)] {
                let key = if method == "get_offer_states" { "offerId" } else { "id" };
                let row = rows.iter().find(|r| r.get(key) == Some(&Value::Int(id))).unwrap();
                if method == "get_products" { assert_eq!(row.get("quantity"), Some(&Value::Int(1))); }
                if method == "get_offers" {
                    assert_eq!(row.get("label"), Some(&Value::None));
                    assert_eq!(row.get("singlePurchase"), Some(&Value::Bool(false)));
                    assert!(row.get("periodPurchase").is_none());
                    assert_eq!(row.get("offerPricings").unwrap().items().unwrap()[0].get("price"), Some(&Value::Int(price)));
                }
                if method == "get_offer_states" { assert_eq!(row.get("canPurchase"), Some(&Value::Bool(true))); }
            }
        }
    }
    for id in [57207, 86227] { assert_eq!(call(&mut client, "storeManager", "is_offer_purchasable", &[Value::Int(id)], &kwargs).await.unwrap(), Value::Bool(true)); }
    let unlisted = client.sde.expert_systems.iter().find(|d| !client.sde.store.expert_offers().iter().any(|o| o.type_id == d.type_id)).unwrap().type_id;
    assert_eq!(call(&mut client, "storeManager", "is_offer_purchasable", &[Value::Int(i64::from(unlisted))], &kwargs).await.unwrap(), Value::Bool(false));
    let excluded: Vec<_> = client.sde.expert_systems.iter().filter(|d| d.hidden || d.retired).map(|d| i64::from(d.type_id)).collect();
    let balance = crate::dao::accounts::balances::plex_balance(&db, actor.account).await.unwrap();
    for id in excluded {
        assert_eq!(call(&mut client, "storeManager", "is_offer_purchasable", &[Value::Int(id)], &kwargs).await.unwrap(), Value::Bool(false));
        assert_eq!(call(&mut client, "storeManager", "buy_offer", &[Value::Int(id), Value::str(PLX), Value::Int(1)], &kwargs).await.unwrap(), Value::Bool(false));
    }
    assert_eq!(crate::dao::accounts::balances::plex_balance(&db, actor.account).await.unwrap(), balance);
    assert!(crate::dao::store::purchases::tokens(&db, actor.account).await.unwrap().is_empty());
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn alpha_purchase_claim_consume_and_renew() {
    use crate::dao::{accounts::subscriptions, skills::expert_systems as installed, store::purchases};
    use crate::services::subscriptions::rules;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let grade = subscriptions::by_user(&db, actor.account).await.unwrap();
    assert!(subscriptions::set_grade(&db, actor.account, rules::ALPHA, None, None, grade.revision).await.unwrap());
    assert_eq!(rules::current(&db, actor.account, crate::client::filetime_now()).await.unwrap(), rules::ALPHA);
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    client.login.user_id = actor.account;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = sender;
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap();
    let (station_id, system) = (station.station_id, station.solar_system_id.unwrap());
    db.execute("UPDATE game.characters SET station_id=$2, home_station_id=$2, solar_system_id=$3 WHERE character_id=$1", &[&actor.character, &station_id, &system]).await.unwrap();
    client.session.apply(vec![("stationid", Value::Int(station_id)), ("solarsystemid2", Value::Int(system))]);
    super::purchasing::set_balance(&db, actor, 100).await;
    let definition = client.sde.expert_systems.get(57207).unwrap();
    let duration = definition.duration_filetime;
    let skills = definition.skills.clone();
    let before = fixture::skill_fingerprint(&db, actor.character).await.unwrap();
    assert_eq!(crate::dao::characters::attributes::free_points(&db, actor.character).await.unwrap(), 0);
    assert!(crate::dao::skills::history::list(&db, actor.character, 1).await.unwrap().is_empty());
    let kwargs = Value::Dict(vec![]);
    for index in 0..2 {
        assert_eq!(call(&mut client, "storeManager", "buy_offer", &[Value::Int(57207), Value::str(PLX), Value::Int(1)], &kwargs).await.unwrap(), Value::Bool(true));
        assert_eq!(crate::dao::accounts::balances::plex_balance(&db, actor.account).await.unwrap(), 100 - 15 * (index + 1));
        assert_eq!(fixture::store_purchase_count(&db, actor.account, 57207).await.unwrap(), index + 1);
        let tokens = purchases::tokens(&db, actor.account).await.unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!((tokens[0].type_id, tokens[0].quantity), (57207, 1));
        let token = tokens[0].token_id;
        let wire = call(&mut client, "userSvc", "GetRedeemTokens", &[], &kwargs).await.unwrap();
        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
        let token_value = &wire.items().unwrap()[0].items().unwrap()[0];
        let Value::Instance(_, state) = token_value else { panic!("expected token KeyVal") };
        assert_eq!(state.get("dateTime"), Some(&Value::Int(tokens[0].created)));
        assert_eq!(state.get("soulbound"), Some(&Value::Bool(false)));
        if index == 0 { assert!(installed::all(&db, actor.character).await.unwrap().is_empty()); }
        while let Ok((name, _)) = receiver.try_recv() { assert_ne!(name, "OnExpertSystemsUpdated"); }
        let args = [Value::list([Value::tuple([Value::Int(token), Value::None])]), Value::Int(actor.character)];
        let claim = call(&mut client, "userSvc", "ClaimRedeemTokens", &args, &kwargs).await.unwrap();
        assert_eq!(claim.get("errors").unwrap().items().unwrap().len(), 0);
        let item = claim.get("itemsCreated").unwrap().items().unwrap()[0].get("itemID").unwrap().as_int().unwrap();
        assert!(purchases::tokens(&db, actor.account).await.unwrap().is_empty());
        let replay = call(&mut client, "userSvc", "ClaimRedeemTokens", &args, &kwargs).await.unwrap();
        assert!(replay.get("itemsCreated").unwrap().items().unwrap().is_empty());
        if index == 0 { assert!(installed::all(&db, actor.character).await.unwrap().is_empty()); }
        while let Ok((name, _)) = receiver.try_recv() { assert_ne!(name, "OnExpertSystemsUpdated"); }
        let previous = installed::all(&db, actor.character).await.unwrap();
        assert_eq!(call(&mut client, "expertSystemMgr", "ConsumeExpertSystem", &[Value::Int(item)], &kwargs).await.unwrap(), Value::None);
        let after = installed::all(&db, actor.character).await.unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].type_id, 57207);
        let updates: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok())
            .filter(|(name, _)| name == "OnExpertSystemsUpdated").collect();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].1, vec![
            Value::Dict(vec![(Value::Int(57207), Value::tuple([Value::Int(after[0].installed_at), Value::Int(after[0].expires_at)]))]),
            Value::Bool(index == 0), Value::Int(57207),
        ]);
        if index == 1 {
            assert_eq!(after[0].installed_at, previous[0].installed_at);
            assert_eq!(after[0].expires_at, previous[0].expires_at + duration);
        }
        assert_eq!(fixture::skill_fingerprint(&db, actor.character).await.unwrap(), before);
        let snapshot = crate::services::skills::effective::snapshot_at(&db, &client.sde, actor.character, crate::client::filetime_now()).await.unwrap();
        for &(skill, level) in &skills { assert!(snapshot.level(skill) >= level); }
        for &(skill, level) in &skills { assert!(snapshot.get(skill).unwrap().virtual_level.is_some_and(|grant| grant >= level)); }
    }
    let persisted = subscriptions::by_user(&db, actor.account).await.unwrap();
    assert_eq!((persisted.grade, persisted.omega_until, persisted.grace_until), (rules::ALPHA, None, None));
    assert_eq!(rules::current(&db, actor.account, crate::client::filetime_now()).await.unwrap(), rules::ALPHA);
    assert_eq!(crate::dao::characters::attributes::free_points(&db, actor.character).await.unwrap(), 0);
    assert!(crate::dao::skills::history::list(&db, actor.character, 1).await.unwrap().is_empty());
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", crate::services::skills::completion::KIND, actor.character)]).await.unwrap();
    crate::dao::accounts::plex::tests::cleanup(&db, actor.account).await;
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn expert_gift_unlisted_redemption_and_deleted_character() {
    use crate::dao::store::purchases;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let payer = fixture::create(&db).await.unwrap();
    let recipient = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), payer.character).await;
    client.login.user_id = payer.account;
    super::purchasing::set_balance(&db, payer, 100).await;
    let registration = crate::live_sessions::register(recipient.account, false).unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let guard = registration.attach_sender(sender).unwrap();
    let gift = Value::dict([("to_character_id", Value::Int(recipient.character))]);
    assert_eq!(call(&mut client, "storeManager", "buy_offer", &[Value::Int(86227), Value::str(PLX), Value::Int(1)], &gift).await.unwrap(), Value::Bool(true));
    assert_eq!(crate::dao::accounts::balances::plex_balance(&db, payer.account).await.unwrap(), 50);
    assert_eq!(receiver.try_recv().unwrap(), ("OnRedeemingQueueUpdated".into(), vec![]));
    assert!(purchases::tokens(&db, payer.account).await.unwrap().is_empty());
    let gifted = purchases::tokens(&db, recipient.account).await.unwrap();
    assert_eq!(gifted.len(), 1);
    assert_eq!((gifted[0].type_id, gifted[0].quantity), (86227, 1));
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap();
    let (station_id, system) = (station.station_id, station.solar_system_id.unwrap());
    db.execute("UPDATE game.characters SET station_id=$2, home_station_id=$2, solar_system_id=$3 WHERE character_id=$1", &[&recipient.character, &station_id, &system]).await.unwrap();
    let unlisted = client.sde.expert_systems.iter().find(|d| d.type_id == 57208).unwrap().type_id;
    assert!(!client.sde.store.expert_offers().iter().any(|o| o.type_id == unlisted));
    purchases::grant(&db, recipient.account, unlisted, 1, TOKEN_LABEL, filetime_now()).await.unwrap();
    let tokens = purchases::tokens(&db, recipient.account).await.unwrap();
    let id = tokens.iter().find(|t| t.type_id == unlisted).unwrap().token_id;
    let (mut recipient_client, _peer) = crate::services::tests::client(db.clone(), recipient.character).await;
    recipient_client.login.user_id = recipient.account;
    recipient_client.session.apply(vec![("stationid", Value::Int(station_id)), ("solarsystemid2", Value::Int(system))]);
    let kwargs = Value::Dict(vec![]);
    let args = [Value::list([Value::tuple([Value::Int(id), Value::None])]), Value::Int(recipient.character)];
    let result = call(&mut recipient_client, "userSvc", "ClaimRedeemTokens", &args, &kwargs).await.unwrap();
    assert_eq!(result.get("itemsCreated").unwrap().items().unwrap().len(), 1);
    let remaining = purchases::tokens(&db, recipient.account).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!((remaining[0].type_id, remaining[0].quantity), (86227, 1));
    let token_id = remaining[0].token_id;
    db.execute("UPDATE game.characters SET deleted=true WHERE character_id=$1", &[&recipient.character]).await.unwrap();
    recipient_client.outbox.clear();
    let args = [Value::list([Value::tuple([Value::Int(token_id), Value::None])]), Value::Int(recipient.character)];
    assert!(matches!(call(&mut recipient_client, "userSvc", "ClaimRedeemTokens", &args, &kwargs).await, Err(CallError::User { msg, .. }) if msg == messages::MSG_CHARACTER_NOT_FOUND));
    assert_eq!(purchases::tokens(&db, recipient.account).await.unwrap().len(), 1);
    assert!(recipient_client.outbox.is_empty());
    let count: i64 = db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&recipient.character, &86227_i32]).await.unwrap().get("n");
    assert_eq!(count, 0);
    purchases::trash(&db, recipient.account, &[token_id]).await.unwrap();
    drop(guard);
    drop(registration);
    crate::dao::accounts::plex::tests::cleanup(&db, payer.account).await;
    fixture::cleanup(&db, recipient).await.unwrap();
    fixture::cleanup(&db, payer).await.unwrap();
}
