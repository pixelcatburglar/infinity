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
                    && super::modifier_applies(key, &modifier, super::Key::Ship, &ship, None, true) {
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
        online: true, active: false, overloaded: false, projected_effect: None,
        charge_type: None, level: Some(level), overrides: vec![(super::ATTR_SKILL_LEVEL, 99.0)], charge_values: vec![],
    };
    assert_eq!(node.base(None, None, super::ATTR_SKILL_LEVEL), 3.0);
}

fn fixture_node(location: super::Location) -> super::Node {
    super::Node { type_id: 1, data_position: None, location, online: true,
        active: false, overloaded: false, projected_effect: None,
        charge_type: None, level: None, overrides: vec![], charge_values: vec![] }
}

fn fixture_modifier(func: &str, domain: &str) -> super::Modifier {
    super::Modifier { func: func.into(), domain: domain.into(), op: 6,
        modified: 1, modifying: 2, group: Some(10), skill: Some(20), target_category: None }
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
        assert_eq!(applies(source, &fixture_modifier("ItemModifier", domain), target, &ship, None, false), expected, "{domain}");
    }
    assert!(applies(Key::Charge(1), &fixture_modifier("ItemModifier", "otherID"), source, &module, None, false));
    assert!(applies(Key::Projected(9), &fixture_modifier("ItemModifier", "targetID"), Key::Ship, &ship, None, false));
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
        assert_eq!(applies(source, &fixture_modifier("LocationModifier", domain), target, node, None, false), expected, "{domain}");
    }
    for domain in ["charID", "shipID"] {
        let group = fixture_modifier("LocationGroupModifier", domain);
        assert!(applies(source, &group, Key::Drone(1), &drone, Some(10), false));
        assert!(!applies(source, &group, Key::Drone(1), &drone, Some(11), false));
        let required = fixture_modifier("LocationRequiredSkillModifier", domain);
        assert!(applies(source, &required, Key::Drone(1), &drone, None, true));
        assert!(!applies(source, &required, Key::Drone(1), &drone, None, false));
    }
    let owner = fixture_modifier("OwnerRequiredSkillModifier", "charID");
    assert!(applies(source, &owner, Key::Ship, &ship, None, true));
    assert!(applies(source, &owner, Key::Module(1), &module, None, true));
    assert!(applies(source, &owner, Key::Drone(1), &drone, None, true));
    assert!(!applies(source, &owner, Key::Implant(1), &brain, None, true));
    assert!(!applies(source, &owner, Key::Ship, &ship, None, false));
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
    assert!(modifier_applies(Key::Projected(1), &modifier, Key::Ship, &target, None, false));
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
            && modifier_applies(source, &modifier, Key::Ship, &ship, None, true) {
            collected.push((modifier.op, node.base(None, None, modifier.modifying), false));
        }
    }
    assert_eq!(collected.len(), 3);
    assert!((apply(100.0, &collected) - 118.8).abs() < 1e-9);
    assert!(!modifier_applies(Key::Implant(1), &modifier, Key::Ship, &ship, None, false));
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
    assert!(modifier_applies(Key::Projected(5), &script, Key::Ship, &victim, None, false));
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
fn node_override_precedence_and_modifier() {
    use super::{Node, Location, ATTR_SKILL_LEVEL};
    let mut node = Node { type_id: 1, data_position: None, location: Location::Char, online: true, active: false, overloaded: false, projected_effect: None, charge_type: None, level: None, overrides: vec![(1, 10.0), (ATTR_SKILL_LEVEL, 99.0)], charge_values: vec![] };
    let meta = super::Attribute { attribute_id: 1, default_value: Some(7.5), stackable: None };
    assert_eq!(node.base(None, Some(&meta), 1), 10.0);
    assert_eq!(node.base(None, None, 2), 0.0);
    assert_eq!(apply(node.base(None, Some(&meta), 1), &[(6, 25.0, false)]), 12.5);
    node.level = Some(4);
    assert_eq!(node.base(None, None, ATTR_SKILL_LEVEL), 4.0);
    node.overrides.clear();
    assert_eq!(node.base(None, Some(&meta), 1), 7.5);
}


