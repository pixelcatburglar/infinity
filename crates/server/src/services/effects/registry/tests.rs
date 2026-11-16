use super::*;

#[test]
fn supported_numeric_effects_and_unknown() {
    use EffectKind::*;
    let cases = [
        (EFFECT_TARGET_ATTACK, Turret),
        (EFFECT_PROJECTILE_FIRED, Turret),
        (EFFECT_TARGET_DISINTEGRATOR_ATTACK, Turret),
        (EFFECT_USE_MISSILES, Missile),
        (EFFECT_SHIELD_BOOSTING, LocalRepair { layer: 0, amount_attr: ATTR_SHIELD_BONUS }),
        (EFFECT_ARMOR_REPAIR, LocalRepair { layer: 1, amount_attr: ATTR_ARMOR_DAMAGE_AMOUNT }),
        (EFFECT_STRUCTURE_REPAIR, LocalRepair { layer: 2, amount_attr: ATTR_STRUCTURE_DAMAGE_AMOUNT }),
        (EFFECT_AFTERBURNER, Propulsion), (EFFECT_MICROWARPDRIVE, Propulsion),
        (EFFECT_MINING_LASER, Mining), (EFFECT_MINING_CLOUDS, Mining),
        (EFFECT_HACKING, Hacking), (EFFECT_PROBE_LAUNCHING, ProbeLaunch),
        (EFFECT_SURVEY_SCAN, SurveyScan),
        (EFFECT_EMP_WAVE, Smartbomb), (EFFECT_NPC_BEHAVIOR_SMARTBOMB, Smartbomb),
        (EFFECT_CHAIN_LIGHTNING, Vorton), (EFFECT_ENTITY_CHAIN_LIGHTNING, Vorton), (EFFECT_BOMB_LAUNCHING, Bomb),
        (EFFECT_CLOAKING, Cloak), (EFFECT_CLOAKING_PROTOTYPE, Cloak), (EFFECT_CLOAKING_WARP_SAFE, Cloak),
        (EFFECT_MICRO_JUMP_DRIVE, MicroJumpDrive), (EFFECT_TRACTOR_BEAM, Tractor),
        (EFFECT_CARGO_SCAN, CargoScan), (EFFECT_SHIP_SCAN, ShipScan),
        (EFFECT_TARGET_HOSTILES, AutoTarget), (EFFECT_TARGET_PASSIVELY, PassiveTarget),
        (EFFECT_INDUSTRIAL_ITEM_COMPRESSION, Compression),
        (EFFECT_ONLINE as i32, Online),
    ];
    for (id, expected) in cases {
        assert_eq!(classify(id, false), Some(expected), "effect {id}");
        assert_eq!(classify(id, true), Some(expected), "effect {id} with modifiers");
    }
    assert_eq!(classify(i32::MAX, false), None);
    assert_eq!(classify(i32::MAX, true), Some(Modifiers));
    assert_eq!(classify(EFFECT_BASTION, true), None);
}

#[test]
fn remote_repair_effects_select_their_health_layer() {
    for (id, layer) in [
        (EFFECT_REMOTE_SHIELD_BOOSTER, 0), (EFFECT_REMOTE_ARMOR_REPAIRER, 1),
        (EFFECT_REMOTE_HULL_REPAIRER, 2), (EFFECT_ANCILLARY_REMOTE_SHIELD_BOOSTER, 0),
        (EFFECT_ANCILLARY_REMOTE_ARMOR_REPAIRER, 1), (EFFECT_MUTADAPTIVE_REMOTE_ARMOR_REPAIRER, 1),
        (EFFECT_NPC_BEHAVIOR_REMOTE_SHIELD, 0), (EFFECT_NPC_BEHAVIOR_REMOTE_ARMOR, 1),
    ] {
        assert_eq!(classify(id, false), Some(EffectKind::RemoteRepair { layer }));
    }
}

#[test]
fn activation_scope_rejects_unsupported_launchers_and_non_modules() {
    let module = i64::from(CATEGORY_MODULE);
    for group in [GROUP_MISSILE_LAUNCHER_FESTIVAL, GROUP_BUBBLE_PROBE_LAUNCHER, GROUP_BOMB_LAUNCHER,
        GROUP_XL_TORPEDO_LAUNCHER, GROUP_XL_CRUISE_LAUNCHER, GROUP_RAPID_TORPEDO_LAUNCHER] {
        assert!(!activation_in_scope(module, group, EffectKind::Missile));
        assert!(activation_in_scope(module, group, EffectKind::Modifiers));
    }
    assert!(activation_in_scope(module, 0, EffectKind::Missile));
    assert!(!activation_in_scope(65, 0, EffectKind::Missile));
    assert!(!activation_in_scope(66, 0, EffectKind::Mining));
}

#[test]
fn modifier_activation_requires_an_active_category() {
    let mut effect = Effect { id: 1, kind: EffectKind::Modifiers, guid: None, category: None, attributes: [None; 5], flags: 0 };
    for category in [None, Some(0), Some(3), Some(5)] {
        effect.category = category;
        assert!(!effect.manually_activatable(), "{category:?}");
    }
    for category in [Some(1), Some(2), Some(4)] {
        effect.category = category;
        assert!(effect.manually_activatable(), "{category:?}");
    }
    effect.kind = EffectKind::Online;
    effect.category = Some(1);
    assert!(effect.manually_activatable());
}
