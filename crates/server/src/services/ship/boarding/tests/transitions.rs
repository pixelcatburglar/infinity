//! Real in-space Board transitions and persistent hull identity.
//! Uses published SDE ships and game items; foreign custody is not modelled.
use super::*;
use infinity_core::consts::{FLAG_CARGO, TYPE_CAPSULE, dogma::ATTR_ARMOR_DAMAGE};
use crate::dao::items::{creation, space_ships::FreeShip};
use crate::services::ship::balls;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn capsule_and_ship_transitions_preserve_independent_hulls() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().expect("SDE system").solar_system_id;
    let capsule_group = sde.types.get(TYPE_CAPSULE).and_then(|t| t.group_id).expect("capsule group");
    let capsule = sde.types.iter().filter(|t| t.group_id == Some(capsule_group) && t.published == Some(true))
        .filter(|t| balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("published capsule");
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.group_id != Some(capsule_group) && t.published == Some(true))
        .filter(|t| balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("published ship");
    let before = fingerprint(&db).await;
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [100_000_000.0; 3];
        let old = creation::positioned(&test_db, capsule, owner, system, pos).await.unwrap();
        let first = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let second = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
        train(&test_db, &sde, owner, kind).await;
        // inventoryrestrictions/common/checks.py identifies type list 142 as soulbound.
        let cargo = sde.types.iter().find(|t| t.category_id.is_some() && t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true)
            && sde.typelists.contains(142, t.type_id, t.group_id, t.category_id))
            .expect("initialized SDE must contain a published nonship soulbound type");
        assert!(sde.typelists.contains(142, cargo.type_id, cargo.group_id, cargo.category_id));
        let cargo_type = cargo.type_id;
        let mut cargo = Vec::new();
        for ship in [old, first, second] {
            let id: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&cargo_type, &owner, &ship, &FLAG_CARGO]).await.unwrap().get("item_id");
            cargo.push(id);
        }
        assert!(crate::dao::items::dogma::set_override(&test_db, first, ATTR_ARMOR_DAMAGE, 1.0).await.unwrap());
        assert!(crate::dao::items::dogma::set_override(&test_db, second, ATTR_ARMOR_DAMAGE, 2.0).await.unwrap());
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let row = |id, type_id| FreeShip { item_id: id, type_id, owner_id: owner, corporation_id: 1, alliance_id: 0, pos };
        let pilot = balls::piloted(&test_db, sde.clone(), &row(old, capsule), owner, 0.0).await.unwrap();
        world.system(system).await.unwrap();
        world.with(system, move |s| pilot.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        for id in [first, second] {
            assert!(world.ball(system, id).await.is_some(), "cold restore must admit free hull");
        }
        let armor_damage = world.with(system, move |s| {
            assert!(s.set_condition_fraction_mask(old, 0b010, 0.5));
            let state = s.combat_condition_snapshot(old).unwrap();
            state.condition.stats.max[1] - state.condition.now[1]
        }).await.unwrap();
        let frame = world.with(system, move |s| {
            let generation = s.combat_membership_generation(old).unwrap();
            let facts = s.prepare_combat_source(old, None, None, None,
                crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
            let hit = s.apply_damage(old, [1.0, 0.0, 0.0, 0.0],
                crate::services::combat::damage::DamageSource { facts: Some(facts), ..Default::default() }, false).unwrap().unwrap();
            assert!(hit.dealt > 0.0 && !hit.lethal);
            crate::space::snapshot::capture_fact_checkpoint(s).unwrap().unwrap()
        }).await.unwrap();
        assert!(!frame.is_committed(&test_db).await.unwrap());
        let mut c = client(test_db.clone(), sde.clone(), world.clone(), account).await;
        c.session.apply(vec![("userid", Value::Int(account)), ("charid", Value::Int(owner)), ("shipid", Value::Int(old)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        world.register_pilot(owner, c.notify_tx.clone());
        world.set_online_session(owner, infinity_core::session::OnlineSession::from_session(&c.session));
        world.observe(system, c.login.client_id, old, c.destiny_tx.clone()).await.unwrap();
        for (from, target) in [(old, first), (first, second)] {
            fourtuple(board_call(&mut c, target, Value::Int(from)).await.ok().expect("space Board"));
            assert_eq!(c.session.int("shipid"), Some(target));
            assert_eq!(crate::dao::characters::selection::ship_id(&test_db, owner).await.unwrap(), Some(target));
            if from == old {
                assert!(frame.is_committed(&test_db).await.unwrap());
                let saved = crate::dao::items::dogma::overrides(&test_db, old).await.unwrap();
                assert_eq!(saved.iter().find(|row| row.attribute_id == ATTR_ARMOR_DAMAGE).unwrap().value, armor_damage);
            }
            world.with(system, move |s| {
                assert_eq!(s.pilot_ball(owner).unwrap().id, target);
                assert_eq!(s.pilots().filter(|b| b.char_id == Some(owner)).count(), 1);
                assert_eq!(s.ball(from).unwrap().char_id, None);
                assert!(!s.ball(from).unwrap().interactive);
                assert!(s.persistent_generation(from).is_some());
                assert_eq!(s.ball(target).unwrap().char_id, Some(owner));
            }).await.unwrap();
            let state = fingerprint(&test_db).await;
            fourtuple(board_call(&mut c, target, Value::Int(target)).await.ok().expect("current ship refresh"));
            assert_eq!(fingerprint(&test_db).await, state);
        }
        for (ship, item) in [old, first, second].into_iter().zip(cargo) {
            let row = test_db.query_one("SELECT owner_id, location_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
            assert_eq!(row.get::<_, i64>("owner_id"), owner);
            assert_eq!(row.get::<_, i64>("location_id"), ship);
        }
        for (ship, value) in [(first, 1.0_f64), (second, 2.0_f64)] {
            let damage: f64 = test_db.query_one("SELECT value FROM game.item_attributes WHERE item_id=$1 AND attribute_id=$2", &[&ship, &ATTR_ARMOR_DAMAGE]).await.unwrap().get("value");
            assert_eq!(damage, value);
        }
        let restored = World::new(test_db.clone(), sde, 1);
        restored.system(system).await.unwrap();
        for ship in [old, first] {
            assert_eq!(restored.ball(system, ship).await.unwrap().char_id, None);
            assert_eq!(restored.with(system, move |s| s.persistent_balls().filter(|b| b.id == ship).count()).await, Some(1));
        }
    }).await;
    cleanup(&db, account, owner).await;
    assert_eq!(fingerprint(&db).await, before);
    result.unwrap();
}
