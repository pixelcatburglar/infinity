use super::super::*;
use super::fixtures::*;
use super::fixtures::activate;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn implant_stack_install_destroy_and_legacy_slot_refusals() {
    let _market = crate::services::market::lock().await;
    let (db, f, c, _peer) = setup().await;
    let ty = implant(&c.sde);
    let stack = item(&db, f.character, ty, STATION, 3).await;
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,$3)", &[&stack, &ATTR_IMPLANTNESS, &7.0_f64]).await.unwrap();
    let now = filetime_now();
    let (value, effects) = activate(&db, &c.sde, f.character, stack, None, now, false, vec![]).await.unwrap_or_else(|_| panic!("implant activation failed"));
    assert!(value == Value::None);
    assert_eq!(notices(&effects, crate::client::BRAIN_REFRESH), 1);
    assert_eq!(notices(&effects, "OnServerImplantsChanged"), 1);
    let installed = brain::list(&db, f.character).await.unwrap();
    assert_eq!(installed.len(), 1);
    assert_ne!(installed[0].item_id, stack);
    assert_eq!(installed[0].flag_id, FLAG_IMPLANT);
    assert_eq!(db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&stack]).await.unwrap().get::<_, i64>("quantity"), 2);
    let wire = super::super::super::brain_reads::get_implants(&c, f.character).await.unwrap_or_else(|_| panic!("implant read failed"));
    let Value::Dict(records) = wire else { panic!("implant read must be a dictionary") };
    assert_eq!(records.len(), 1);
    assert!(records[0].0 == Value::Int(installed[0].item_id));
    assert_eq!(records[0].1, crate::services::keyval([
        ("itemID", Value::Int(installed[0].item_id)),
        ("typeID", Value::Int(i64::from(ty))),
    ]));
    for id in [stack, installed[0].item_id] {
        assert_eq!(dogma::overrides(&db, id).await.unwrap().iter().find(|a| a.attribute_id == ATTR_IMPLANTNESS).map(|a| a.value), Some(7.0));
    }
    let before = fingerprint(&db, f.character).await;
    assert!(activate(&db, &c.sde, f.character, stack, None, now, false, vec![]).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, before);
    db.execute("UPDATE game.items SET brain_slot=NULL WHERE item_id=$1", &[&installed[0].item_id]).await.unwrap();
    let legacy = fingerprint(&db, f.character).await;
    assert!(activate(&db, &c.sde, f.character, stack, None, now, false, vec![]).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, legacy);
    assert!(destroy(&db, &c.sde, f.character, stack, now).await.is_err());
    assert_eq!(fingerprint(&db, f.character).await, legacy);
    let (value, effects) = destroy(&db, &c.sde, f.character, installed[0].item_id, now).await.unwrap_or_else(|_| panic!("implant removal failed"));
    assert!(value == Value::None);
    assert_eq!(notices(&effects, crate::client::BRAIN_REFRESH), 1);
    assert_eq!(notices(&effects, "OnServerImplantsChanged"), 1);
    assert!(brain::list(&db, f.character).await.unwrap().is_empty());
    assert!(dogma::overrides(&db, installed[0].item_id).await.unwrap().is_empty());
    cleanup(&db, f).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn wrong_source_owner_type_and_prerequisites_leave_state_unchanged() {
    let _market = crate::services::market::lock().await;
    let (db, f, c, _peer) = setup().await;
    let other = crate::services::tests::fixture::create(&db).await.unwrap();
    let ty = implant(&c.sde);
    let remote = item(&db, f.character, ty, STATION + 1, 1).await;
    let wrong_owner = item(&db, other.character, ty, STATION, 1).await;
    let wrong_type = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id != Some(CATEGORY_IMPLANT)).expect("non-implant type required").type_id;
    let invalid = item(&db, f.character, wrong_type, STATION, 1).await;
    let dependent = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER) && t.attribute(ATTR_IMPLANTNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0) && requirements(&c.sde, t.type_id).next().is_some()).expect("implant with prerequisite required").type_id;
    let needs_skill = item(&db, f.character, dependent, STATION, 1).await;
    let now = filetime_now();
    let before = fingerprint(&db, f.character).await;
    let other_before = fingerprint(&db, other.character).await;
    for id in [remote, wrong_owner, invalid, needs_skill] {
        assert!(activate(&db, &c.sde, f.character, id, None, now, false, vec![]).await.is_err());
        assert_eq!(fingerprint(&db, f.character).await, before);
        assert_eq!(fingerprint(&db, other.character).await, other_before);
    }
    cleanup(&db, other).await;
    cleanup(&db, f).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn current_owned_ship_cargo_is_a_valid_booster_source() {
    let _market = crate::services::market::lock().await;
    let (db, f, c, _peer) = setup().await;
    let ship_type = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).expect("ship type required").type_id;
    let ship = item(&db, f.character, ship_type, STATION, 1).await;
    let ty = booster(&c.sde);
    let source = crate::dao::items::transfers::insert(&db, ty, f.character, ship, infinity_core::consts::FLAG_CARGO, 1).await.unwrap();
    let now = filetime_now();
    let draws: Vec<_> = candidates(&c.sde, ty).into_iter().map(|(id, _)| (id, 0.5)).collect();
    let before = fingerprint(&db, f.character).await;
    let world = crate::space::World::new(db.clone(), c.sde.clone(), 1);
    let fences = crate::services::effects::online::capture_online_fences(&world, &[f.character]).await;
    let refused: Result<Value, CallError> = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = c.sde.clone(); let draws = draws.clone(); let fences = fences.clone();
        Box::pin(async move { activate_tx(tx, outbox, &sde, &fences, f.character, Some(ship + 1), source, Some(ship), now, &draws, true).await })
    }, |value, _: Outbox<OnlineChanged>| async move { Ok(value) }).await;
    assert!(refused.is_err());
    assert_eq!(fingerprint(&db, f.character).await, before);
    let fences = crate::services::effects::online::capture_online_fences(&world, &[f.character]).await;
    let result: Result<Value, CallError> = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = c.sde.clone(); let draws = draws.clone(); let fences = fences.clone();
        Box::pin(async move { activate_tx(tx, outbox, &sde, &fences, f.character, Some(ship), source, Some(ship), now, &draws, true).await })
    }, |value, _: Outbox<OnlineChanged>| async move { Ok(value) }).await;
    assert!(result.unwrap_or_else(|_| panic!("owned cargo activation failed")) == Value::None);
    assert_eq!(brain::get(&db, f.character, source).await.unwrap().unwrap().flag_id, FLAG_BOOSTER);
    cleanup(&db, f).await;
}
