//! Character-domain replacement and deadline semantics of the retained inputs.

use super::*;
use infinity_core::consts::dogma::ATTR_SKILL_LEVEL;

fn fit() -> Fit {
    Fit::for_space_type(crate::sde::types::tests::combat_fixture(), 1, vec![])
}

fn inputs(at: i64) -> CharacterInputs {
    CharacterInputs {
        at,
        attributes: Some(vec![(1, 10.0)]),
        trained: vec![Skill { type_id: 7, level: 5, skill_points: 8000 }],
        installations: vec![],
        brain: vec![
            BrainInput { item: BrainItem { item_id: 11, type_id: 2, flag_id: FLAG_IMPLANT,
                slot: None, activated_at: None, expires_at: None, effect_ids: None }, overrides: vec![] },
            BrainInput { item: BrainItem { item_id: 12, type_id: 3, flag_id: FLAG_BOOSTER,
                slot: None, activated_at: Some(10), expires_at: Some(20), effect_ids: Some(vec![3, 7]) }, overrides: vec![(9, 1.5)] },
        ],
    }
}



#[test]
fn replacement_half_open_expiry_and_late_publish_semantics() {
    let mut fit = fit();
    fit.apply_character_inputs(&inputs(10));
    assert_eq!(fit.value(Key::Skill(7), ATTR_SKILL_LEVEL), 5.0);
    assert!(fit.source_type(Key::Implant(11)).is_some());
    assert!(fit.source_type(Key::Booster(12)).is_some());
    assert_eq!(fit.selected_effects[&12].as_slice(), [3, 7]);
    // A real full publish replaces the whole domain: absent items are removed.
    let mut next = inputs(15);
    next.trained = vec![Skill { type_id: 9, level: 2, skill_points: 1 }];
    next.brain.remove(0);
    fit.apply_character_inputs(&next);
    assert!(fit.source_type(Key::Skill(7)).is_none() && fit.source_type(Key::Skill(9)).is_some());
    assert!(fit.source_type(Key::Implant(11)).is_none());
    assert_eq!(fit.selected_effects[&12].as_slice(), [3, 7]);
    assert!(!fit.selected_effects.contains_key(&11));
    // The booster deadline is half-open: still live at 19, gone at 20.
    assert_eq!(fit.next_character_expiry(), Some(20));
    assert!(!fit.expire_character_inputs(19));
    assert!(fit.source_type(Key::Booster(12)).is_some());
    assert!(fit.expire_character_inputs(20));
    assert!(fit.source_type(Key::Booster(12)).is_none());
    assert!(!fit.selected_effects.contains_key(&12));
    // A late publish of the same older rows cannot resurrect the deadline.
    fit.apply_character_inputs(&inputs(10));
    assert!(fit.source_type(Key::Booster(12)).is_none());
    assert!(!fit.expire_character_inputs(30));
    assert_eq!(fit.next_character_expiry(), None);
}

#[test]
fn owner_inputs_replace_ship_domain_and_preserve_drone_domain() {
    let node = |location| Node {
        type_id: 9, data_position: None, location, online: true, active: false,
        overloaded: false, projected_effect: None, receiver: None, charge_type: None,
        level: None, overrides: vec![], cargo_unlimited: false, charge_values: vec![],
    };
    // A type-only snapshot owns no ship item and produces no package.
    assert!(fit().owner_inputs().is_none());
    let mut source = fit();
    source.ship_id = Some(1);
    source.projection_revision = Some(4);
    source.nodes.insert(Key::Module(2), node(Location::Ship));
    source.nodes.insert(Key::Charge(2), node(Location::Ship));
    source.charge_incarnations.insert(2, crate::space::targeting::next_generation());
    let source_charge = source.charge_incarnation(2).unwrap();
    source.nodes.insert(Key::Projected(9), Node { receiver: Some(Key::Ship), ..node(Location::Space) });
    source.apply_character_inputs(&inputs(10));
    let owner = source.owner_inputs().unwrap();
    assert_eq!(owner.ship, 1);
    let mut target = fit();
    target.nodes.insert(Key::Module(7), node(Location::Ship));
    target.ship_id = Some(6);
    target.nodes.insert(Key::Charge(7), node(Location::Ship));
    target.charge_incarnations.insert(7, crate::space::targeting::next_generation());
    target.nodes.insert(Key::Projected(10), Node { receiver: Some(Key::Ship), ..node(Location::Space) });
    target.nodes.insert(Key::Drone(5), Node { overrides: vec![(5, 1.0)], ..node(Location::Space) });
    target.nodes.insert(Key::Projected(8), Node { receiver: Some(Key::Drone(5)), ..node(Location::Space) });
    target.apply_owner_inputs(&owner);
    // The old ship's keys disappear, the new package's arrive, and the
    // receiver's own drone node and drone-bound projection stay untouched.
    assert!(target.source_type(Key::Module(7)).is_none());
    assert!(target.source_type(Key::Module(2)).is_some() && target.source_type(Key::Charge(2)).is_some());
    assert!(target.source_type(Key::Projected(9)).is_some() && target.source_type(Key::Projected(8)).is_some());
    assert_eq!(target.nodes[&Key::Drone(5)].overrides, vec![(5, 1.0)]);
    assert_eq!(target.ship_id, Some(1));
    assert_eq!(target.projection_revision, Some(4));
    assert!(target.source_type(Key::Charge(7)).is_none());
    assert!(target.charge_incarnation(7).is_none());
    assert!(target.source_type(Key::Projected(10)).is_none());
    let target_charge = target.charge_incarnation(2).unwrap();
    assert_eq!(target_charge.1, source_charge.1);
    assert_ne!(target_charge.0, source_charge.0);
    assert!(!target.remove_charge(2, source_charge.0, source_charge.1));
    // The pure Fit expiry primitive removes the retained booster at its
    // half-open deadline; actor publication exercises its own captured time.
    assert!(target.source_type(Key::Booster(12)).is_some());
    assert!(target.expire_character_inputs(20));
    assert!(target.source_type(Key::Booster(12)).is_none());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn expired_expert_grant_falls_back_to_trained_level_in_place() {
    use std::sync::Arc;
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    // The grant must exceed the trained level, else expiry proves nothing.
    let definition = sde.expert_systems.iter()
        .find(|definition| definition.skills.iter().any(|&(_, virtual_level)| virtual_level > 1))
        .expect("expert definition granting above trained level");
    let (skill_type, virtual_level) = definition.skills.iter().copied()
        .find(|&(_, virtual_level)| virtual_level > 1).unwrap();
    let rank = sde.types.get(skill_type).and_then(|skill| skill.fit_attribute(ATTR_SKILL_TIME_CONSTANT)).unwrap();
    let points = crate::services::skills::formulas::sp_for_level(rank, 1);
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,1,$3)", &[&f.character, &skill_type, &points]).await.unwrap();
    db.execute("INSERT INTO game.character_expert_systems (character_id,type_id,installed_at,expires_at) VALUES ($1,$2,100,300)", &[&f.character, &definition.type_id]).await.unwrap();
    let mut fit = Fit::load_character_at(&db, sde.clone(), f.character, 200).await.unwrap();
    assert_eq!(fit.value(Key::Skill(skill_type), ATTR_SKILL_LEVEL), f64::from(virtual_level));
    assert_eq!(fit.next_character_expiry(), Some(300));
    assert!(fit.expire_character_inputs(300));
    assert_eq!(fit.value(Key::Skill(skill_type), ATTR_SKILL_LEVEL), 1.0);
    assert!(!fit.expire_character_inputs(400));
    // Expiry is in-actor evaluation only; the persisted rows are untouched.
    let stored: i64 = db.query_one("SELECT count(*) AS count FROM game.character_expert_systems WHERE character_id=$1 AND type_id=$2", &[&f.character, &definition.type_id]).await.unwrap().get("count");
    assert_eq!(stored, 1);
    fixture::cleanup(&db, f).await.unwrap();
}
