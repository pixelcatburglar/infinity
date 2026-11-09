//! Committed and rolled-back inventory departures of primed modules.
use super::*;
use infinity_core::{consts::{FLAG_CARGO, FLAG_HI_SLOT_FIRST}};
use crate::{services::inventory::{self, access::Container}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn primed_unfit_clears_only_after_commit() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(MEDIUM_CAPACITY).is_some_and(|v| v > 0.0)).expect("heat-capable hull").type_id;
    let module = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v > 0.0)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_MICROWARPDRIVE)
        && t.effects().iter().any(|e| e.effect_id == OVERLOAD_SPEED)).expect("overloadable propulsion module").type_id;
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, module, FLAG_MED_SLOT_FIRST).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let w = world.clone();
    let d = db.clone();
    let result = tokio::spawn(async move {
        fixture::position(&d, f.ship, SYSTEM, [0.0; 3]).await.unwrap();
        dogma::set_override(&d, f.module, REQUIRED_THERMODYNAMICS, 0.0).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, addr) = listener.accept().await.unwrap();
        let login = Login { user_id: f.account, user_name: f.account.to_string(), role: 0, client_id: f.character, session_id: f.character, language_id: "EN".into() };
        let mut c = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), d.clone(), sde, crate::chat::Hub::new("localhost", d.clone(), w.clone()), w.clone(), login);
        c.session.apply(vec![("charid", Value::Int(f.character)), ("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(SYSTEM))]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = tx.clone();
        w.register_pilot(f.character, tx);
        w.add_ship(SYSTEM, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await.unwrap();
        let args = [Value::Int(f.module), Value::Int(i64::from(OVERLOAD_SPEED))];
        call(&mut c, "dogmaIM", "Overload", &args, None).await.ok().expect("prime idle module");
        assert!(heat::overloaded_modules().contains(&f.module));
        assert!(heat::ship_state(&w, SYSTEM, f.ship).await.is_none_or(|r| r.iter().all(|r| r.incoming == 0.0)));
        while rx.try_recv().is_ok() {}
        c.outbox.clear();
        let destination = Container::Item(f.ship);
        assert!(inventory::movement::move_item(&crate::services::identity::Identity::of_client(&c), &c.db, &c.world, &c.sde, c.session.int("solarsystemid"), c.notify_tx.clone(), &destination, f.module, Some(f.ship), Some(FLAG_HI_SLOT_FIRST), None, |n| crate::services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await.is_err());
        assert!(heat::overloaded_modules().contains(&f.module));
        assert!(rx.try_recv().is_err());
        assert!(c.outbox.is_empty());
        assert_eq!(inventory::movement::move_item(&crate::services::identity::Identity::of_client(&c), &c.db, &c.world, &c.sde, c.session.int("solarsystemid"), c.notify_tx.clone(), &destination, f.module, Some(f.ship), Some(FLAG_CARGO), None, |n| crate::services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await.ok().unwrap(), Some(f.module));
        assert!(!heat::overloaded_modules().contains(&f.module));
        assert!(heat::ship_state(&w, SYSTEM, f.ship).await.is_none_or(|r| r.iter().all(|r| r.incoming == 0.0)));
    }).await;
    heat::clear_character(f.character);
    world.remove_ball(SYSTEM, f.ship).await;
    world.unregister_pilot_generation(f.character, None);
    combat::forget_ship(f.ship);
    let flush = world.flush_positions().await;
    drop(world);
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn unsupported_overload_does_not_change_state() {
    use infinity_core::consts::dogma::EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(MEDIUM_CAPACITY).is_some_and(|v| v > 0.0)).expect("heat-capable hull").type_id;
    let (module, overload) = sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER))
        .find_map(|t| t.effects().iter().find(|e| sde.dogma.effect(e.effect_id).is_some_and(|row| row.effect_category == Some(5)))
            .map(|e| (t.type_id, e.effect_id))).expect("unsupported mutadaptive module with overload effect");
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, module, FLAG_MED_SLOT_FIRST).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let w = world.clone();
    let d = db.clone();
    let result = tokio::spawn(async move {
        fixture::position(&d, f.ship, SYSTEM, [0.0; 3]).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, addr) = listener.accept().await.unwrap();
        let login = Login { user_id: f.account, user_name: f.account.to_string(), role: 0, client_id: f.character, session_id: f.character, language_id: "EN".into() };
        let mut c = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), d.clone(), sde, crate::chat::Hub::new("localhost", d.clone(), w.clone()), w.clone(), login);
        c.session.apply(vec![("charid", Value::Int(f.character)), ("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(SYSTEM))]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = tx.clone();
        w.register_pilot(f.character, tx);
        w.add_ship(SYSTEM, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await.unwrap();
        let cap = combat::groups_health::capacitor_snapshot(&d, &w, SYSTEM, f.ship).await.ok().unwrap().unwrap().level;
        let args = [Value::Int(f.module), Value::Int(i64::from(overload))];
        assert!(call(&mut c, "dogmaIM", "Overload", &args, None).await.is_err());
        assert!(!heat::overloaded_modules().contains(&f.module));
        assert!(heat::ship_state(&w, SYSTEM, f.ship).await.is_none_or(|r| r.iter().all(|r| r.heat == 0.0 && r.incoming == 0.0)));
        assert_eq!(combat::groups_health::capacitor_snapshot(&d, &w, SYSTEM, f.ship).await.ok().unwrap().unwrap().level, cap);
        assert!(rx.try_recv().is_err());
        assert!(c.outbox.is_empty());
    }).await;
    heat::clear_character(f.character);
    world.remove_ball(SYSTEM, f.ship).await;
    world.unregister_pilot_generation(f.character, None);
    combat::forget_ship(f.ship);
    let flush = world.flush_positions().await;
    drop(world);
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}
