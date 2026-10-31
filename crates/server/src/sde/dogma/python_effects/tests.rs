use super::*;

fn effect(name: &str) -> Effect {
    Effect {
        effect_id: 1, name: Some(name.into()), guid: None, effect_category: None,
        duration_attribute_id: None, is_offensive: false, is_assistance: false,
        disallow_auto_repeat: false, electronic_chance: false, propulsion_chance: false,
        discharge_attribute_id: None, range_attribute_id: None, falloff_attribute_id: None,
        tracking_attribute_id: None, modifiers: Vec::new(),
    }
}

#[test]
fn supplement_only_empty_lists_and_preserve_webifier_row() {
    let mut effect = effect("remoteWebifierFalloff");
    supplement(&mut effect);
    assert_eq!(effect.modifiers.len(), 1);
    let row = &effect.modifiers[0];
    assert_eq!((row.func.as_str(), row.domain.as_str(), row.op, row.modified, row.modifying),
        ("ItemModifier", "targetID", POST_PERCENT, ATTR_MAX_VELOCITY, ATTR_SPEED_FACTOR_INTERIM));
    effect.modifiers[0].modifying = 1;
    supplement(&mut effect);
    assert_eq!(effect.modifiers.len(), 1);
    assert_eq!(effect.modifiers[0].modifying, 1);
    let mut unknown = self::effect("unrecognized");
    supplement(&mut unknown);
    assert!(unknown.modifiers.is_empty());
}

#[test]
fn fitting_effects_supply_client_ship_modifiers_without_duplication() {
    for (name, expected) in [
        ("online", vec![(2, 49, 50), (2, 15, 30)]),
        ("launcherFitted", vec![(3, 101, 47)]),
        ("turretFitted", vec![(3, 102, 47)]),
        ("rigSlot", vec![(3, 1154, 47), (2, 1152, 1153)]),
    ] {
        let mut e = effect(name);
        supplement(&mut e);
        let actual: Vec<_> = e.modifiers.iter().map(|m| {
            assert_eq!((m.func.as_str(), m.domain.as_str()), ("ItemModifier", "shipID"));
            (m.op, m.modified, m.modifying)
        }).collect();
        assert_eq!(actual, expected);
        supplement(&mut e);
        assert_eq!(e.modifiers.len(), expected.len());
    }
}

#[test]
fn falloff_rows_resolve_their_raw_source_attributes() {
    for (name, expected) in [
        ("remoteWebifierFalloff", vec![ATTR_SPEED_FACTOR]),
        ("remoteTargetPaintFalloff", vec![ATTR_SIGNATURE_RADIUS_BONUS]),
        ("remoteSensorDampFalloff", vec![ATTR_SCAN_GRAVIMETRIC_STRENGTH_PERCENT, ATTR_SCAN_LADAR_STRENGTH_PERCENT, ATTR_SCAN_MAGNETOMETRIC_STRENGTH_PERCENT, ATTR_SCAN_RADAR_STRENGTH_PERCENT, ATTR_SCAN_RESOLUTION_BONUS, ATTR_MAX_TARGET_RANGE_BONUS]),
        ("remoteSensorBoostFalloff", vec![ATTR_SCAN_GRAVIMETRIC_STRENGTH_PERCENT, ATTR_SCAN_LADAR_STRENGTH_PERCENT, ATTR_SCAN_MAGNETOMETRIC_STRENGTH_PERCENT, ATTR_SCAN_RADAR_STRENGTH_PERCENT, ATTR_SCAN_RESOLUTION_BONUS, ATTR_MAX_TARGET_RANGE_BONUS]),
    ] {
        let mut effect = effect(name);
        supplement(&mut effect);
        assert_eq!(effect.modifiers.iter().map(|row| projected_interim_source(row.modifying).unwrap()).collect::<Vec<_>>(), expected);
    }
    assert_eq!(projected_interim_source(ATTR_SPEED_FACTOR), None);
}

#[test]
fn npc_projected_rows_match_client_ship_target_modifiers() {
    for (npc, player) in [
        ("remoteWebifierEntity", "remoteWebifierFalloff"),
        ("npcBehaviorWebifier", "remoteWebifierFalloff"),
        ("remoteTargetPaintEntity", "remoteTargetPaintFalloff"),
        ("behaviorTargetPainter", "remoteTargetPaintFalloff"),
        ("remoteSensorDampEntity", "remoteSensorDampFalloff"),
        ("behaviorSensorDampener", "remoteSensorDampFalloff"),
        ("npcBehaviorTrackingDisruptor", "shipModuleTrackingDisruptor"),
        ("npcEntityTrackingDisruptor", "shipModuleTrackingDisruptor"),
        ("npcBehaviorGuidanceDisruptor", "shipModuleGuidanceDisruptor"),
    ] {
        let mut npc = effect(npc);
        let mut player = effect(player);
        supplement(&mut npc);
        supplement(&mut player);
        assert_eq!(npc.modifiers.len(), player.modifiers.len());
        for (a, b) in npc.modifiers.iter().zip(&player.modifiers) {
            assert_eq!((&a.func, &a.domain, a.op, a.modified, a.modifying, a.skill),
                       (&b.func, &b.domain, b.op, b.modified, b.modifying, b.skill));
        }
    }
    let mut combined = effect("npcEntityWeaponDisruptor");
    let mut tracking = effect("shipModuleTrackingDisruptor");
    let mut guidance = effect("shipModuleGuidanceDisruptor");
    supplement(&mut combined);
    supplement(&mut tracking);
    supplement(&mut guidance);
    let expected = tracking.modifiers.iter().chain(&guidance.modifiers);
    assert_eq!(combined.modifiers.len(), tracking.modifiers.len() + guidance.modifiers.len());
    for (a, b) in combined.modifiers.iter().zip(expected) {
        assert_eq!((&a.func, &a.domain, a.op, a.modified, a.modifying, a.skill),
                   (&b.func, &b.domain, b.op, b.modified, b.modifying, b.skill));
    }
}

#[test]
fn disruption_categories_match_client_target_triples() {
    let tracking = [
        (ATTR_MAX_RANGE, ATTR_MAX_RANGE_BONUS),
        (ATTR_FALLOFF, ATTR_FALLOFF_BONUS),
        (ATTR_TRACKING_SPEED, ATTR_TRACKING_SPEED_BONUS),
    ];
    let guidance = [
        (ATTR_MISSILE_ENTITY_VELOCITY_MULTIPLIER, ATTR_MISSILE_VELOCITY_BONUS),
        (ATTR_MISSILE_ENTITY_FLIGHT_TIME_MULTIPLIER, ATTR_EXPLOSION_DELAY_BONUS),
        (ATTR_MISSILE_ENTITY_AOE_VELOCITY_MULTIPLIER, ATTR_AOE_VELOCITY_BONUS),
        (ATTR_MISSILE_ENTITY_AOE_CLOUD_SIZE_MULTIPLIER, ATTR_AOE_CLOUD_SIZE_BONUS),
    ];
    for (name, want_tracking, want_guidance) in [
        ("shipModuleTrackingDisruptor", true, false),
        ("shipModuleGuidanceDisruptor", false, true),
        ("npcEntityWeaponDisruptor", true, true),
    ] {
        let mut e = effect(name);
        supplement(&mut e);
        for category in [CATEGORY_ENTITY, CATEGORY_DRONE] {
            let actual: Vec<_> = e.modifiers.iter().filter(|m| m.target_category == Some(category))
                .map(|m| {
                    assert_eq!((m.func.as_str(), m.domain.as_str(), m.op, m.skill),
                        ("ItemModifier", "targetID", POST_PERCENT, None));
                    (m.modified, m.modifying)
                }).collect();
            let expected: Vec<_> = tracking.iter().copied().filter(|_| want_tracking)
                .chain(guidance.iter().copied().filter(|_| want_guidance)).collect();
            assert_eq!(actual, expected);
        }
        for m in e.modifiers.iter().filter(|m| m.target_category == Some(CATEGORY_SHIP)) {
            assert_eq!((m.func.as_str(), m.domain.as_str(), m.op),
                ("LocationRequiredSkillModifier", "targetID", POST_PERCENT));
            assert!(matches!(m.skill, Some(TYPE_GUNNERY | TYPE_MISSILE_LAUNCHER_OPERATION)));
        }
    }
}

#[test]
fn actual_bastion_name_supplements_only_empty_sde_modifiers() {
    let mut bastion = effect("moduleBonusBastionModule");
    bastion.effect_id = 6658;
    supplement(&mut bastion);
    assert!(bastion.modifiers.iter().any(|row|
        row.func == "ItemModifier" && row.domain == "shipID"
            && row.op == POST_PERCENT && row.modified == ATTR_MAX_VELOCITY
            && row.modifying == ATTR_SPEED_FACTOR));

    let mut existing = effect("moduleBonusBastionModule");
    existing.effect_id = 6658;
    existing.modifiers.push(bastion.modifiers.remove(0));
    existing.modifiers[0].modifying = 1;
    supplement(&mut existing);
    assert_eq!(existing.modifiers.len(), 1);
    assert_eq!(existing.modifiers[0].modifying, 1);

    for name in ["marauderModeEffect", "fighterMicroJumpDrive", "fighterAbilityMicroJumpDrive"] {
        let mut excluded = effect(name);
        supplement(&mut excluded);
        assert!(excluded.modifiers.is_empty(), "{name}");
    }

    let mut mjd = effect("microJumpDrive");
    mjd.effect_id = 4921;
    supplement(&mut mjd);
    assert_eq!(mjd.modifiers.len(), 1);
    assert_eq!((mjd.modifiers[0].op, mjd.modifiers[0].modified, mjd.modifiers[0].modifying),
        (POST_PERCENT, ATTR_SIGNATURE_RADIUS, ATTR_SIGNATURE_RADIUS_BONUS_PERCENT));
}

#[test]
fn migrated_skills_keep_required_source_skill_and_operations() {
    for (name, func, domain, modified, modifying) in [
        ("missileEMDmgBonus", "OwnerRequiredSkillModifier", "charID", ATTR_EM_DAMAGE, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("missileExplosiveDmgBonus", "OwnerRequiredSkillModifier", "charID", ATTR_EXPLOSIVE_DAMAGE, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("missileKineticDmgBonus2", "OwnerRequiredSkillModifier", "charID", ATTR_KINETIC_DAMAGE, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("missileThermalDmgBonus", "OwnerRequiredSkillModifier", "charID", ATTR_THERMAL_DAMAGE, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("droneDmgBonus", "OwnerRequiredSkillModifier", "charID", ATTR_DAMAGE_MULTIPLIER, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("cloakingTargetingDelayBonusPostPercentCloakingTargetingDelayBonusForShipModulesRequiringCloaking", "LocationRequiredSkillModifier", "shipID", ATTR_CLOAKING_TARGETING_DELAY, ATTR_CLOAKING_TARGETING_DELAY_BONUS),
        ("selfRof", "LocationRequiredSkillModifier", "shipID", ATTR_RATE_OF_FIRE, ATTR_ROF_BONUS),
    ] {
        let mut effect = effect(name);
        supplement(&mut effect);
        assert_eq!(effect.modifiers.len(), 1);
        let row = &effect.modifiers[0];
        assert_eq!((row.func.as_str(), row.domain.as_str(), row.op, row.modified, row.modifying, row.skill),
            (func, domain, POST_PERCENT, modified, modifying, Some(SOURCE_ITEM_TYPE)));
    }
}

#[test]
fn propulsion_rows_cover_mass_and_microwarp_signature_once() {
    let mut afterburner = effect("moduleBonusAfterburner");
    let mut microwarpdrive = effect("moduleBonusMicrowarpdrive");
    supplement(&mut afterburner);
    supplement(&mut microwarpdrive);
    assert_eq!(afterburner.modifiers.len(), 1);
    assert_eq!((afterburner.modifiers[0].op, afterburner.modifiers[0].modified, afterburner.modifiers[0].modifying),
        (MOD_ADD, ATTR_MASS, ATTR_MASS_ADDITION));
    assert_eq!(microwarpdrive.modifiers.len(), 2);
    assert_eq!((microwarpdrive.modifiers[0].op, microwarpdrive.modifiers[0].modified), (MOD_ADD, ATTR_MASS));
    assert_eq!((microwarpdrive.modifiers[1].op, microwarpdrive.modifiers[1].modified, microwarpdrive.modifiers[1].modifying),
        (POST_PERCENT, ATTR_SIGNATURE_RADIUS, ATTR_SIGNATURE_RADIUS_BONUS));
}
