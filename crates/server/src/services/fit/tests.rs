use super::apply;

#[test]
fn operations_and_stacking() {
    // 3 x 5% damage skill bonus (PostPercent, unpenalized) on 2.0
    assert!((apply(2.0, &[(6, 15.0, false)]) - 2.3).abs() < 1e-9);
    // Two 10% penalized damage mods: 1.1 * (1 + 0.1 * e^-(1/2.67)^2)
    let two = apply(1.0, &[(4, 1.1, true), (4, 1.1, true)]);
    assert!((two - 1.1 * (1.0 + 0.1 * (-(1.0f64 / 2.67).powi(2)).exp())).abs() < 1e-9);
    assert_eq!(apply(5.0, &[(2, 3.0, false), (7, 1.0, false)]), 1.0);
}

#[test]
fn unnerfed_percent_precedes_assignment_and_ignores_penalty() {
    assert!((apply(100.0, &[(6, 10.0, false), (8, 20.0, true), (8, 20.0, true)]) - 158.4).abs() < 1e-9);
    assert_eq!(apply(100.0, &[(8, 20.0, true), (7, 3.0, false)]), 3.0);
}

#[test]
fn client_operation_precedence_and_sign_separated_stacking() {
    // Source: dogma/const.py:7-28,52-66. Deliberately reverse input order.
    let mods = [(8, 20.0, true), (6, 10.0, false), (5, 2.0, false),
        (4, 3.0, false), (3, 4.0, false), (2, 10.0, false),
        (1, 2.0, false), (0, 4.0, false), (-1, 5.0, false)];
    // (((5 * 4 / 2 + 10 - 4) * 3 / 2) * 1.1) * 1.2
    assert!((apply(999.0, &mods) - 31.68).abs() < 1e-9);
    let mut assigned = mods.to_vec();
    assigned.push((7, 42.0, false));
    assert_eq!(apply(999.0, &assigned), 42.0);

    // Two bonuses and two penalties each start their own penalty sequence.
    // Source: dogma/attributes/stackingNurfedAttribute.py:95-99.
    let second = (-(1.0f64 / 2.67).powi(2)).exp();
    let expected = 1.2 * (1.0 + 0.1 * second) * 0.7 * (1.0 - 0.2 * second);
    assert!((apply(1.0, &[(4, 1.1, true), (4, 0.8, true),
        (4, 1.2, true), (4, 0.7, true)]) - expected).abs() < 1e-9);
}

#[test]
fn synthetic_reference_fits_with_active_brain_modifiers() {
    // Synthetic fixture, not SDE values. Client operations: dogma/const.py:52-66.
    // Each row is frigate/cruiser/battleship base and independently calculated
    // expected value, with implant +10%, booster +20% and penalty -10%.
    let fixtures = [
        ([200.0, 150.0, 100.0], [237.6, 178.2, 118.8]),
        ([1000.0, 4000.0, 12000.0], [1188.0, 4752.0, 14256.0]),
        ([800.0, 3500.0, 11000.0], [950.4, 4158.0, 13068.0]),
        ([700.0, 3000.0, 10000.0], [831.6, 3564.0, 11880.0]),
        ([500.0, 2000.0, 8000.0], [594.0, 2376.0, 9504.0]),
        ([400.0, 250.0, 100.0], [475.2, 297.0, 118.8]),
        ([30000.0, 50000.0, 80000.0], [35640.0, 59400.0, 95040.0]),
        ([40.0, 120.0, 400.0], [47.52, 142.56, 475.2]),
        ([1200000.0, 11000000.0, 100000000.0], [1425600.0, 13068000.0, 118800000.0]),
        ([3.0, 0.6, 0.15], [3.564, 0.7128, 0.1782]),
        ([1000.0, 1400.0, 2000.0], [1188.0, 1663.2, 2376.0]),
        ([300.0, 600.0, 1200.0], [356.4, 712.8, 1425.6]),
        ([25.0, 50.0, 125.0], [29.7, 59.4, 148.5]),
        ([25.0, 75.0, 175.0], [29.7, 89.1, 207.9]),
        ([50.0, 900.0, 15000.0], [59.4, 1069.2, 17820.0]),
        ([200.0, 350.0, 700.0], [237.6, 415.8, 831.6]),
        ([150.0, 400.0, 800.0], [178.2, 475.2, 950.4]),
        ([5.0, 3.0, 2.0], [5.94, 3.564, 2.376]),
        ([10.0, 18.0, 28.0], [11.88, 21.384, 33.264]),
        ([4.0, 6.0, 8.0], [4.752, 7.128, 9.504]),
    ];
    for (bases, expected) in fixtures {
        for (base, expected) in bases.into_iter().zip(expected) {
            let meta = super::Attribute { attribute_id: 1, default_value: Some(base), stackable: Some(true) };
            let ship = fixture_node(super::Location::Space);
            let modifier = fixture_modifier("OwnerRequiredSkillModifier", "charID");
            let mut mods = Vec::new();
            for (key, percent) in [(super::Key::Implant(1), 10.0),
                (super::Key::Booster(2), 20.0), (super::Key::Booster(2), -10.0)] {
                let mut source = fixture_node(super::Location::Char);
                source.overrides = vec![(2, percent)];
                if super::effect_running(key, &source, 1, 0, &modifier.domain)
                    && super::modifier_applies(key, None, &modifier, super::Key::Ship, &ship, None, true) {
                    mods.push((modifier.op, source.base(None, None, modifier.modifying), false));
                }
            }
            assert_eq!(mods.len(), 3);
            let actual = apply(super::base_attribute(None, Some(&meta), 1), &mods);
            assert!((actual - expected).abs() < 1e-9 * expected.max(1.0));
        }
    }
}

#[test]
fn skill_level_uses_shared_points_formula_before_node_override() {
    // characterskills/util.py:20-25; incoming modifications to the derived
    // level are rejected by dogma/attributes/skillLevelAttribute.py:21-30.
    let level = crate::services::skills::formulas::level_from_points(8000, 1.0);
    assert_eq!(level, 3);
    let node = super::Node {
        type_id: 1, data_position: None, location: super::Location::Char,
        online: true, active: false, overloaded: false, projected_effect: None, receiver: None,
        charge_type: None, level: Some(level), overrides: vec![(super::ATTR_SKILL_LEVEL, 99.0)], charge_values: vec![],
    };
    assert_eq!(node.base(None, None, super::ATTR_SKILL_LEVEL), 3.0);
}

#[test]
fn supplied_skill_level_comes_from_points_not_stored_level() {
    let skill = crate::dao::skills::training::Skill { type_id: 42, level: 5, skill_points: 8000 };
    let rank = 1.0;
    let level = crate::services::skills::formulas::level_from_points(skill.skill_points, rank);
    assert_eq!(skill.level, 5);
    assert_eq!(level, 3);
}


fn fixture_node(location: super::Location) -> super::Node {
    super::Node { type_id: 1, data_position: None, location, online: true,
        active: false, overloaded: false, projected_effect: None, receiver: None,
        charge_type: None, level: None, overrides: vec![], charge_values: vec![] }
}

fn fixture_modifier(func: &str, domain: &str) -> super::Modifier {
    super::Modifier { func: func.into(), domain: domain.into(), op: 6,
        modified: 1, modifying: 2, group: Some(10), skill: Some(20), target_category: None }
}

#[test]
fn drone_node_overrides_are_independent_of_owner_bonus() {
    let mut drone = fixture_node(super::Location::Space);
    drone.overrides = vec![(1, 200.0)];
    let base = super::Attribute { attribute_id: 1, default_value: Some(100.0), stackable: Some(true) };
    let owner_bonus = [(super::OP_POST_PERCENT, 20.0, false)];
    assert_eq!(apply(drone.base(None, Some(&base), 1), &owner_bonus), 240.0);
    drone.overrides.clear();
    assert_eq!(apply(drone.base(None, Some(&base), 1), &owner_bonus), 120.0);
}

#[tokio::test]
#[ignore = "requires a dedicated database with the retail SDE"]
async fn real_sde_prospective_drone_and_projection_receivers() {
    use super::{Fit, Key};
    use crate::consts::{dogma::ATTR_AGILITY, dogma::ATTR_MASS, dogma::ATTR_MAX_VELOCITY, dogma::EFFECT_REMOTE_WEBIFIER_FALLOFF};
    use crate::services::effects::{projection::Projection, EffectKind};
    use std::sync::Arc;

    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    // Select the actual skill by SDE name; its node and passive index match an
    // owner fit loaded from the database.
    let skill_data = sde.types.iter().find(|t| t.name.as_deref() == Some("Drone Navigation"))
        .expect("Drone Navigation in SDE");
    let skill = skill_data.type_id;
    let drone = 2454; // Hobgoblin I
    let mut fit = Fit::for_space_type(sde.clone(), 587, vec![]);
    fit.include_drone(7, drone, vec![(ATTR_MAX_VELOCITY, 200.0), (1, 12.0)]);
    let unbonused = fit.value(Key::Drone(7), ATTR_MAX_VELOCITY);
    let drone_data = sde.types.get(drone).expect("Hobgoblin I in SDE");
    for attr in [ATTR_MASS, ATTR_AGILITY] {
        let actual = drone_data.fit_attribute(attr).filter(|value| *value > 0.0)
            .expect("combat drone must have positive SDE mass and agility");
        assert_eq!(fit.base(Key::Drone(7), attr), actual);
        assert_eq!(fit.value(Key::Drone(7), attr), actual);
    }
    let mut skill_node = fixture_node(super::Location::Char);
    skill_node.type_id = skill;
    skill_node.data_position = sde.types.position(skill);
    skill_node.level = Some(5);
    fit.nodes.insert(Key::Skill(skill), skill_node);
    assert!(skill_data.effects().iter().filter_map(|entry| sde.dogma.effect(entry.effect_id))
        .any(|effect| effect.effect_category.unwrap_or(0) == super::EFFECT_CATEGORY_PASSIVE
            && effect.modifiers().iter().any(|modifier| modifier.modified == ATTR_MAX_VELOCITY)),
        "Drone Navigation must have a passive velocity modifier in the SDE");
    for entry in skill_data.effects() {
        if entry.effect_id == super::EFFECT_SKILL { continue; }
        let Some(effect) = sde.dogma.effect(entry.effect_id) else { continue };
        if effect.effect_category.unwrap_or(0) != super::EFFECT_CATEGORY_PASSIVE { continue; }
        for (position, modifier) in effect.modifiers().iter().enumerate() {
            fit.passive_by_attribute.entry(modifier.modified).or_default().push(fit.passive_modifiers.len());
            fit.passive_modifiers.push((Key::Skill(skill), entry.effect_id, position));
        }
    }
    fit.memo.get_mut().unwrap().clear();
    let bonused = fit.value(Key::Drone(7), ATTR_MAX_VELOCITY);
    assert!(bonused > unbonused, "Drone Navigation must increase drone velocity");
    assert_eq!(fit.source_type(Key::Drone(7)), Some(drone));
    assert_eq!(fit.overrides(Key::Drone(7)), &[(1, 12.0), (ATTR_MAX_VELOCITY, 200.0)]);
    fit.include_drone(7, drone, vec![]);
    let fresh = fit.value(Key::Drone(7), ATTR_MAX_VELOCITY);
    assert_ne!(fresh, bonused);
    assert!(fresh > fit.base(Key::Drone(7), ATTR_MAX_VELOCITY));
    assert!(fit.overrides(Key::Drone(7)).is_empty());

    let web = sde.dogma.effect(EFFECT_REMOTE_WEBIFIER_FALLOFF).expect("web effect in SDE");
    let modifier = web.modifiers().iter().find(|m| m.domain == "targetID" && m.modified == ATTR_MAX_VELOCITY)
        .expect("web velocity modifier in SDE");
    let projection = Projection { source_ship: 1, kind: EffectKind::Modifiers, until_ms: i64::MAX,
        source_module: 42, source_type: 587, effect_id: EFFECT_REMOTE_WEBIFIER_FALLOFF,
        values: vec![(modifier.modifying, -60.0)], charge_values: vec![], charge_type: None };
    let ship_before = fit.value(Key::Ship, ATTR_MAX_VELOCITY);
    Fit::insert_projections(&sde, &mut fit.nodes, Key::Drone(7), vec![projection.clone()]);
    fit.memo.get_mut().unwrap().clear();
    assert!(fit.value(Key::Drone(7), ATTR_MAX_VELOCITY) < fresh);
    assert_eq!(fit.value(Key::Ship, ATTR_MAX_VELOCITY), ship_before);
    Fit::insert_projections(&sde, &mut fit.nodes, Key::Ship, vec![projection]);
    fit.memo.get_mut().unwrap().clear();
    assert_eq!(fit.nodes[&Key::Projected(42)].receiver, Some(Key::Drone(7)));
    assert_eq!(fit.nodes[&Key::Projected(43)].receiver, Some(Key::Ship));
    assert!(fit.value(Key::Ship, ATTR_MAX_VELOCITY) < ship_before);
    assert!(fit.value(Key::Drone(7), ATTR_MAX_VELOCITY) < fresh);
}

#[test]
fn projected_category_gate_uses_receiving_hull() {
    use crate::consts::{CATEGORY_DRONE, CATEGORY_ENTITY, CATEGORY_SHIP};
    for actual in [Some(CATEGORY_SHIP), Some(CATEGORY_ENTITY), Some(CATEGORY_DRONE), None] {
        assert!(super::category_matches(None, actual));
        assert_eq!(super::category_matches(Some(CATEGORY_SHIP), actual), actual == Some(CATEGORY_SHIP));
        assert_eq!(super::category_matches(Some(CATEGORY_ENTITY), actual), actual == Some(CATEGORY_ENTITY));
        assert_eq!(super::category_matches(Some(CATEGORY_DRONE), actual), actual == Some(CATEGORY_DRONE));
    }
}

#[test]
fn drone_and_ship_projections_keep_receivers_and_owner_bonus_separate() {
    use super::{category_matches, modifier_applies, Key, Location};
    use crate::consts::{CATEGORY_DRONE, CATEGORY_SHIP};
    let ship = fixture_node(Location::Space);
    let drone = fixture_node(Location::Space);
    let owner = fixture_modifier("OwnerRequiredSkillModifier", "charID");
    let projected = fixture_modifier("ItemModifier", "targetID");
    let drone_key = Key::Drone(7);
    assert!(modifier_applies(Key::Skill(1), None, &owner, drone_key, &drone, None, true));
    let owner_bonus = (owner.op, 20.0, false);
    for penalty in [-60.0, -30.0] {
        assert!(modifier_applies(Key::Projected(2), Some(drone_key), &projected, drone_key, &drone, None, false));
        assert!(!modifier_applies(Key::Projected(2), Some(drone_key), &projected, Key::Ship, &ship, None, false));
        assert!(modifier_applies(Key::Projected(3), Some(Key::Ship), &projected, Key::Ship, &ship, None, false));
        assert!(!modifier_applies(Key::Projected(3), Some(Key::Ship), &projected, drone_key, &drone, None, false));
        assert!((apply(100.0, &[owner_bonus, (projected.op, penalty, false)]) - 120.0 * (1.0 + penalty / 100.0)).abs() < 1e-9);
    }
    assert!(category_matches(Some(CATEGORY_DRONE), Some(CATEGORY_DRONE)));
    assert!(!category_matches(Some(CATEGORY_SHIP), Some(CATEGORY_DRONE)));
}

#[test]
fn item_and_location_domains_select_correct_nodes() {
    use super::{modifier_applies as applies, Key, Location};
    let ship = fixture_node(Location::Space);
    let module = fixture_node(Location::Ship);
    let brain = fixture_node(Location::Char);
    let drone = fixture_node(Location::Space);
    let source = Key::Module(1);
    for (domain, target, expected) in [
        ("itemID", source, true), ("itemID", Key::Ship, false),
        ("shipID", Key::Ship, true), ("charID", Key::Char, true),
        ("otherID", Key::Charge(1), true), ("otherID", Key::Charge(2), false),
        ("targetID", Key::Ship, false), ("structureID", Key::Ship, false),
    ] {
        assert_eq!(applies(source, None, &fixture_modifier("ItemModifier", domain), target, &ship, None, false), expected, "{domain}");
    }
    assert!(applies(Key::Charge(1), None, &fixture_modifier("ItemModifier", "otherID"), source, &module, None, false));
    assert!(applies(Key::Projected(9), Some(Key::Ship), &fixture_modifier("ItemModifier", "targetID"), Key::Ship, &ship, None, false));
    for (domain, target, node, expected) in [
        ("shipID", Key::Module(1), &module, true),
        ("shipID", Key::Implant(1), &brain, false),
        ("charID", Key::Implant(1), &brain, true),
        ("charID", Key::Booster(1), &brain, true),
        ("charID", Key::Module(1), &module, false),
        ("charID", Key::Drone(1), &drone, true),
        ("shipID", Key::Drone(1), &drone, true),
        ("structureID", Key::Drone(1), &drone, false),
    ] {
        assert_eq!(applies(source, None, &fixture_modifier("LocationModifier", domain), target, node, None, false), expected, "{domain}");
    }
    for domain in ["charID", "shipID"] {
        let group = fixture_modifier("LocationGroupModifier", domain);
        assert!(applies(source, None, &group, Key::Drone(1), &drone, Some(10), false));
        assert!(!applies(source, None, &group, Key::Drone(1), &drone, Some(11), false));
        let required = fixture_modifier("LocationRequiredSkillModifier", domain);
        assert!(applies(source, None, &required, Key::Drone(1), &drone, None, true));
        assert!(!applies(source, None, &required, Key::Drone(1), &drone, None, false));
    }
    let owner = fixture_modifier("OwnerRequiredSkillModifier", "charID");
    assert!(applies(source, None, &owner, Key::Ship, &ship, None, true));
    assert!(applies(source, None, &owner, Key::Module(1), &module, None, true));
    assert!(applies(source, None, &owner, Key::Drone(1), &drone, None, true));
    assert!(!applies(source, None, &owner, Key::Implant(1), &brain, None, true));
    assert!(!applies(source, None, &owner, Key::Ship, &ship, None, false));
}

#[test]
fn online_effect_uses_online_state_despite_active_category() {
    use super::{effect_running, Key, Location, EFFECT_ONLINE};
    let mut module = fixture_node(Location::Ship);
    let online = EFFECT_ONLINE as i32;
    assert!(effect_running(Key::Module(1), &module, online, 1, "shipID"));
    assert!(!effect_running(Key::Module(1), &module, 42, 1, "itemID"));
    module.online = false;
    module.active = true;
    assert!(!effect_running(Key::Module(1), &module, online, 1, "shipID"));
    assert!(effect_running(Key::Module(1), &module, 42, 1, "itemID"));

    module.projected_effect = Some(online);
    assert!(effect_running(Key::Projected(1), &module, online, 1, "targetID"));
    assert!(!effect_running(Key::Projected(1), &module, online, 1, "shipID"));
}

#[test]
fn projected_web_and_overload_gate_use_production_selection() {
    use super::{effect_running, modifier_applies, Key, Location};
    let mut module = fixture_node(Location::Ship);
    assert!(!effect_running(Key::Module(1), &module, 42, 2, "targetID"));
    assert!(!effect_running(Key::Module(1), &module, 42, 1, "itemID"));
    assert!(!effect_running(Key::Module(1), &module, 42, 5, "shipID"));
    module.overloaded = true;
    assert!(effect_running(Key::Module(1), &module, 42, 5, "shipID"));
    module.online = false;
    assert!(!effect_running(Key::Module(1), &module, 42, 5, "shipID"));
    module.online = true;
    module.overloaded = false;
    module.active = true;
    assert!(effect_running(Key::Module(1), &module, 42, 1, "itemID"));
    assert!(!effect_running(Key::Module(1), &module, 42, 2, "targetID"));
    assert!(!effect_running(Key::Module(1), &module, 42, 5, "shipID"));

    let mut projection = fixture_node(Location::Space);
    projection.projected_effect = Some(42);
    projection.overrides = vec![(2, -60.0)];
    let target = fixture_node(Location::Space);
    let modifier = fixture_modifier("ItemModifier", "targetID");
    assert!(effect_running(Key::Projected(1), &projection, 42, 2, "targetID"));
    assert!(!effect_running(Key::Projected(1), &projection, 41, 2, "targetID"));
    assert!(!effect_running(Key::Projected(1), &projection, 42, 0, "shipID"));
    assert!(modifier_applies(Key::Projected(1), Some(Key::Ship), &modifier, Key::Ship, &target, None, false));
    let web = projection.base(None, None, modifier.modifying);
    assert!((apply(1.0, &[(6, web, true)]) - 0.4).abs() < 1e-9);
    let expected = 0.4 * (1.0 - 0.6 * (-(1.0f64 / 2.67).powi(2)).exp());
    assert!((apply(1.0, &[(6, web, true), (6, web, true)]) - expected).abs() < 1e-9);
}

#[test]
fn active_brain_sources_change_ship_and_booster_has_penalty() {
    use super::{effect_running, modifier_applies, Key, Location};
    let ship = fixture_node(Location::Space);
    let modifier = fixture_modifier("OwnerRequiredSkillModifier", "charID");
    let mut collected = Vec::new();
    for (source, percent) in [(Key::Implant(1), 10.0), (Key::Booster(2), 20.0),
        (Key::Booster(2), -10.0)] {
        let mut node = fixture_node(Location::Char);
        node.overrides = vec![(2, percent)];
        if effect_running(source, &node, 1, 0, &modifier.domain)
            && modifier_applies(source, None, &modifier, Key::Ship, &ship, None, true) {
            collected.push((modifier.op, node.base(None, None, modifier.modifying), false));
        }
    }
    assert_eq!(collected.len(), 3);
    assert!((apply(100.0, &collected) - 118.8).abs() < 1e-9);
    assert!(!modifier_applies(Key::Implant(1), None, &modifier, Key::Ship, &ship, None, false));
}

#[test]
fn source_categories_and_client_eight_modifier_limit() {
    use super::stacking_exempt;
    for category in [16, 20, 6, 8, 32] {
        assert!(stacking_exempt(category));
        let actual = apply(100.0, &[(6, 10.0, !stacking_exempt(category)),
            (6, 10.0, !stacking_exempt(category))]);
        assert!((actual - 121.0).abs() < 1e-9);
    }
    assert!(!stacking_exempt(7));
    let seven = apply(1.0, &vec![(4, 2.0, true); 7]);
    let eight = apply(1.0, &vec![(4, 2.0, true); 8]);
    let nine = apply(1.0, &vec![(4, 2.0, true); 9]);
    let eighth_factor = 1.0 + (-(7.0f64 / 2.67).powi(2)).exp();
    assert!((eight / seven - eighth_factor).abs() < 1e-12);
    assert!(eight > seven * 1.0001);
    assert!((nine - eight).abs() < 1e-12);
}

#[test]
fn projection_script_uses_injected_value_and_target_domain() {
    use super::{effect_running, modifier_applies, projected_value, Key, Location};
    let mut source = fixture_node(Location::Space);
    source.projected_effect = Some(42);
    source.overrides = vec![(2, -30.0)];
    let victim = fixture_node(Location::Space);
    let script = fixture_modifier("ItemModifier", "targetID");
    let default = super::Attribute { attribute_id: 2, default_value: Some(-10.0), stackable: Some(true) };
    assert!(effect_running(Key::Projected(5), &source, 42, 2, &script.domain));
    assert!(modifier_applies(Key::Projected(5), Some(Key::Ship), &script, Key::Ship, &victim, None, false));
    let injected = projected_value(&source, None, Some(&default), script.modifying);
    // A hypothetical victim bonus affects the victim, not the already effective
    // injected source value. The script is a target row with its own attribute.
    assert!((apply(100.0, &[(6, injected, false), (6, 20.0, false)]) - 84.0).abs() < 1e-9);
    source.overrides.clear();
    assert_eq!(projected_value(&source, None, Some(&default), script.modifying), -10.0);
}

#[test]
fn projected_module_and_charge_keep_distinct_operands() {
    let mut source = fixture_node(super::Location::Space);
    source.overrides = vec![(2, -60.0)];
    source.charge_values = vec![(2, 20.0)];
    let meta = super::Attribute { attribute_id: 2, default_value: Some(5.0), stackable: Some(true) };
    let module_value = super::projected_value(&source, None, Some(&meta), 2);
    let charge_value = source.charge_base(None, Some(&meta), 2);
    assert_eq!(module_value, -60.0);
    assert_eq!(charge_value, 20.0);
    assert!((apply(100.0, &[(6, module_value, false), (6, charge_value, false)]) - 48.0).abs() < 1e-9);
    source.charge_values.clear();
    assert_eq!(source.charge_base(None, Some(&meta), 2), 5.0);
}

#[test]
fn unknown_type_uses_attribute_default() {
    let meta = super::Attribute { attribute_id: 1, default_value: Some(7.5), stackable: None };
    assert_eq!(super::base_attribute(None, Some(&meta), 1), 7.5);
    assert_eq!(super::base_attribute(None, None, 1), 0.0);
    let null_default = super::Attribute { default_value: None, ..meta };
    assert_eq!(super::base_attribute(None, Some(&null_default), 1), 0.0);
}

#[test]
fn drone_physical_base_corrects_only_missing_or_nonpositive_inputs() {
    use crate::consts::{dogma::ATTR_AGILITY, dogma::ATTR_MASS, dogma::ATTR_MAX_VELOCITY, CATEGORY_DRONE, CATEGORY_SHIP, DRONE_FALLBACK_AGILITY, DRONE_FALLBACK_MASS};
    let drone = Some(CATEGORY_DRONE);
    for value in [None, Some(0.0), Some(-1.0)] {
        assert_eq!(super::physical_base(drone, ATTR_MASS, value), Some(DRONE_FALLBACK_MASS));
        assert_eq!(super::physical_base(drone, ATTR_AGILITY, value), Some(DRONE_FALLBACK_AGILITY));
    }
    // Each attribute is independent, including positive combat-drone inputs.
    for (mass, agility, expected_mass, expected_agility) in [
        (None, Some(42.0), DRONE_FALLBACK_MASS, 42.0),
        (Some(4500.0), Some(0.0), 4500.0, DRONE_FALLBACK_AGILITY),
        (Some(4500.0), Some(42.0), 4500.0, 42.0),
    ] {
        assert_eq!(super::physical_base(drone, ATTR_MASS, mass), Some(expected_mass));
        assert_eq!(super::physical_base(drone, ATTR_AGILITY, agility), Some(expected_agility));
    }
    for category in [Some(CATEGORY_SHIP), None] {
        for attr in [ATTR_MASS, ATTR_AGILITY] {
            assert_eq!(super::physical_base(category, attr, Some(0.0)), Some(0.0));
            assert_eq!(super::physical_base(category, attr, None), None);
        }
    }
    assert_eq!(super::physical_base(drone, ATTR_MAX_VELOCITY, Some(0.0)), Some(0.0));
    assert_eq!(super::physical_base(drone, ATTR_MAX_VELOCITY, None), None);
}

#[test]
fn node_override_precedence_and_modifier() {
    use super::{Node, Location, ATTR_SKILL_LEVEL};
    let mut node = Node { type_id: 1, data_position: None, location: Location::Char, online: true, active: false, overloaded: false, projected_effect: None, receiver: None, charge_type: None, level: None, overrides: vec![(1, 10.0), (ATTR_SKILL_LEVEL, 99.0)], charge_values: vec![] };
    let meta = super::Attribute { attribute_id: 1, default_value: Some(7.5), stackable: None };
    assert_eq!(node.base(None, Some(&meta), 1), 10.0);
    assert_eq!(node.base(None, None, 2), 0.0);
    assert_eq!(apply(node.base(None, Some(&meta), 1), &[(6, 25.0, false)]), 12.5);
    node.level = Some(4);
    assert_eq!(node.base(None, None, ATTR_SKILL_LEVEL), 4.0);
    node.overrides.clear();
    assert_eq!(node.base(None, Some(&meta), 1), 7.5);
}



#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn brain_snapshot_respects_activation_and_expiry() {
    use crate::consts::{FLAG_BOOSTER, FLAG_IMPLANT};
    use crate::dao::items::fit::brain_items;
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let row = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_activated_at,brain_expires_at,brain_effect_ids) VALUES (1,$1,$1,$2,true,10,20,ARRAY[7,3]) RETURNING item_id", &[&f.character, &FLAG_BOOSTER]).await.unwrap();
    let booster: i64 = row.get("item_id");
    let row = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_activated_at,brain_expires_at) VALUES (1,$1,$1,$2,true,10,20) RETURNING item_id", &[&f.character, &FLAG_IMPLANT]).await.unwrap();
    let implant: i64 = row.get("item_id");
    for (time, present) in [(9, false), (10, true), (19, true), (20, false)] {
        let items = brain_items(&db, f.character, time).await.unwrap();
        assert_eq!(items.iter().any(|item| item.item_id == booster), present);
        assert!(items.iter().any(|item| item.item_id == implant));
    }
    db.execute("UPDATE game.items SET brain_activated_at=NULL,brain_expires_at=NULL WHERE item_id=$1", &[&booster]).await.unwrap();
    assert!(brain_items(&db, f.character, 100).await.unwrap().iter().any(|item| item.item_id == booster));
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn selected_booster_effect_changes_fit_and_wire_brain() {
    use crate::consts::FLAG_BOOSTER;
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let mut exercised = false;
    'types: for data in c.sde.types.iter() {
        if data.attribute(crate::consts::dogma::ATTR_BOOSTERNESS).is_none() { continue; }
        for entry in data.effects() {
            let Some(effect) = c.sde.dogma.effect(entry.effect_id) else { continue };
            if effect.effect_category.unwrap_or(0) != 0 || !effect.fitting_usage_chance_attribute_id.is_some_and(|id| data.attribute(id).is_some()) { continue; }
            for modifier in effect.modifiers() {
                if modifier.modified == modifier.modifying || !matches!(modifier.op, 2 | 3 | 4 | 6) { continue; }
                let row = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_effect_ids) VALUES ($1,$2,$2,$3,true,ARRAY[]::integer[]) RETURNING item_id", &[&data.type_id, &f.character, &FLAG_BOOSTER]).await.unwrap();
                let item: i64 = row.get("item_id");
                db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,10)", &[&item, &modifier.modifying]).await.unwrap();
                let before = super::Fit::load_character_at(&db, c.sde.clone(), f.character, 10).await.unwrap_or_else(|_| panic!("loading unselected fit"));
                let source = super::Key::Booster(item);
                let mut absent = true;
                before.brain_modifiers(|key, m| {
                    if key == source && std::ptr::eq(m, modifier) { absent = false; }
                });
                db.execute("UPDATE game.items SET brain_effect_ids=ARRAY[$2::integer] WHERE item_id=$1", &[&item, &effect.effect_id]).await.unwrap();
                let after = super::Fit::load_character_at(&db, c.sde.clone(), f.character, 10).await.unwrap_or_else(|_| panic!("loading selected fit"));
                let mut present = false;
                after.brain_modifiers(|key, m| {
                    if key == source && std::ptr::eq(m, modifier) { present = true; }
                });
                for target in [super::Key::Char, super::Key::Ship, source] {
                    if before.value(target, modifier.modified) != after.value(target, modifier.modified) {
                        assert!(absent && present);
                        exercised = true;
                        break;
                    }
                }
                db.execute("DELETE FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
                if exercised { break 'types; }
            }
        }
    }
    fixture::cleanup(&db, f).await.unwrap();
    assert!(exercised, "no effective chance modifier was found in the test SDE");
}
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn expert_fit_projects_virtual_level_and_dogma_sp_without_mutating_training() {
    use std::sync::Arc;
    use crate::consts::dogma::{ATTR_SKILL_LEVEL, ATTR_SKILL_POINTS};
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let definition = sde.expert_systems.iter().find(|definition| definition.skills.first().is_some()).expect("expert definition with skills");
    let (skill_type, virtual_level) = definition.skills[0];
    let expert_type = definition.type_id;
    let rank = sde.types.get(skill_type).and_then(|skill| skill.fit_attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT)).expect("skill rank in SDE");
    let trained_points = crate::services::skills::formulas::sp_for_level(rank, 1);
    db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,1,$3)", &[&f.character, &skill_type, &trained_points]).await.unwrap();
    db.execute("INSERT INTO game.character_expert_systems (character_id,type_id,installed_at,expires_at) VALUES ($1,$2,100,300)", &[&f.character, &expert_type]).await.unwrap();
    let before = super::Fit::load_character_at(&db, sde.clone(), f.character, 99).await.unwrap();
    let after = super::Fit::load_character_at(&db, sde.clone(), f.character, 200).await.unwrap();
    assert_eq!(before.value(super::Key::Skill(skill_type), ATTR_SKILL_LEVEL), 1.0);
    assert_eq!(after.value(super::Key::Skill(skill_type), ATTR_SKILL_LEVEL), f64::from(virtual_level));
    assert_eq!(before.value(super::Key::Skill(skill_type), ATTR_SKILL_POINTS), trained_points as f64);
    assert_eq!(after.value(super::Key::Skill(skill_type), ATTR_SKILL_POINTS), crate::services::skills::formulas::sp_for_level(rank, i64::from(virtual_level)).max(trained_points) as f64);
    let stored: i64 = db.query_one("SELECT skill_points FROM game.skills WHERE character_id=$1 AND type_id=$2", &[&f.character, &skill_type]).await.unwrap().get("skill_points");
    assert_eq!(stored, trained_points);
    fixture::cleanup(&db, f).await.unwrap();
}
