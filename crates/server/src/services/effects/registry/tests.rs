use super::*;
use infinity_core::consts::{CATEGORY_STRUCTURE, CATEGORY_STRUCTURE_MODULE};
use super::super::festival::GROUP_MISSILE_LAUNCHER_FESTIVAL;

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
        (EFFECT_HACKING, Hacking),
        (EFFECT_SURVEY_SCAN, SurveyScan),
        (EFFECT_EMP_WAVE, Smartbomb), (EFFECT_NPC_BEHAVIOR_SMARTBOMB, Smartbomb),
        (EFFECT_CHAIN_LIGHTNING, Vorton), (EFFECT_ENTITY_CHAIN_LIGHTNING, Vorton), (EFFECT_BOMB_LAUNCHING, Bomb),
        (EFFECT_CLOAKING, Cloak), (EFFECT_CLOAKING_PROTOTYPE, Cloak), (EFFECT_CLOAKING_WARP_SAFE, Cloak),
        (EFFECT_MICRO_JUMP_DRIVE, MicroJumpDrive), (EFFECT_MICRO_JUMP_PORTAL, MicroJumpField), (EFFECT_TRACTOR_BEAM, Tractor),
        (EFFECT_CARGO_SCAN, CargoScan), (EFFECT_SHIP_SCAN, ShipScan),
        (EFFECT_TARGET_HOSTILES, AutoTarget), (EFFECT_TARGET_PASSIVELY, PassiveTarget),
        (EFFECT_INDUSTRIAL_ITEM_COMPRESSION, Compression),
        (EFFECT_ONLINE as i32, Online),
        (EFFECT_DOOMSDAY_BEAM_DOT, SuperWeapon), (EFFECT_DOOMSDAY_CONE_DOT, SuperWeapon),
        (EFFECT_DOOMSDAY_SLASH, SuperWeapon),
        (EFFECT_SUPER_WEAPON_AMARR, SuperWeapon), (EFFECT_SUPER_WEAPON_CALDARI, SuperWeapon),
        (EFFECT_SUPER_WEAPON_GALLENTE, SuperWeapon), (EFFECT_SUPER_WEAPON_MINMATAR, SuperWeapon),
        (EFFECT_FOCUSED_WARP_SCRAMBLING_SCRIPT, Projected { jamming: JammingType::Scramble }),
        (EFFECT_FOCUSED_WARP_DISRUPTION_SCRIPT, Projected { jamming: JammingType::Disrupt }),
        (EFFECT_WARP_DISRUPT_SPHERE, Interdiction),
    ];
    for (id, expected) in cases {
        assert_eq!(classify(id, false), Some(expected), "effect {id}");
        assert_eq!(classify(id, true), Some(expected), "effect {id} with modifiers");
    }
    assert_eq!(classify(EFFECT_PROBE_LAUNCHING, false), None);
    assert_eq!(classify(EFFECT_PROBE_LAUNCHING, true), Some(Modifiers));
    assert_eq!(classify(i32::MAX, false), None);
    assert_eq!(classify(i32::MAX, true), Some(Modifiers));
    assert_eq!(classify(EFFECT_MICRO_JUMP_PORTAL_CAPITAL, false), None);
    assert_eq!(classify(EFFECT_MICRO_JUMP_PORTAL_CAPITAL, true), None);
    assert_eq!(classify(EFFECT_BASTION, true), Some(Modifiers));
    assert_eq!(classify(EFFECT_SACRIBIS_BASTION, true), Some(Modifiers));
    assert_eq!(classify(EFFECT_TRIAGE, false), Some(Modifiers));
    assert_eq!(classify(EFFECT_TRIAGE, true), Some(Modifiers));
    assert_eq!(classify(EFFECT_STELLIAN_TRIAGE, false), Some(Modifiers));
    assert_eq!(classify(EFFECT_STELLIAN_TRIAGE, true), Some(Modifiers));
    assert_eq!(classify(EFFECT_TRIAGE_MODE, true), None);
    assert_eq!(classify(EFFECT_TRIAGE_MODE_7, true), None);
    assert_eq!(classify(EFFECT_SIEGE, false), Some(Modifiers));
    assert_eq!(classify(EFFECT_SIEGE, true), Some(Modifiers));
    assert_eq!(classify(EFFECT_SIEGE_MODE, false), None);
    assert_eq!(classify(EFFECT_SIEGE_MODE, true), None);
    assert_eq!(classify(EFFECT_EXCLUDED_6474, false), None);
    assert_eq!(classify(EFFECT_EXCLUDED_6474, true), None);
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

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn activation_scope_rejects_unsupported_launchers_and_non_modules() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let typed = |group: i64| sde.types.iter().find(|row| row.category_id == Some(CATEGORY_MODULE) && row.group_id == Some(group as i32)).map(|row| row.type_id).unwrap_or(0);
    for group in [GROUP_MISSILE_LAUNCHER_FESTIVAL, GROUP_BUBBLE_PROBE_LAUNCHER, GROUP_BOMB_LAUNCHER,
        GROUP_XL_TORPEDO_LAUNCHER, GROUP_XL_CRUISE_LAUNCHER, GROUP_RAPID_TORPEDO_LAUNCHER] {
        let type_id = typed(group);
        let missile = activation_in_scope(&sde, type_id, EffectKind::Missile);
        if group == GROUP_MISSILE_LAUNCHER_FESTIVAL { assert_eq!(missile, super::super::festival::launcher_in_scope(&sde, type_id)); }
        else { assert!(!missile); }
        assert!(activation_in_scope(&sde, type_id, EffectKind::Modifiers));
    }
    let excluded = [GROUP_MISSILE_LAUNCHER_FESTIVAL, GROUP_BUBBLE_PROBE_LAUNCHER, GROUP_BOMB_LAUNCHER,
        GROUP_XL_TORPEDO_LAUNCHER, GROUP_XL_CRUISE_LAUNCHER, GROUP_RAPID_TORPEDO_LAUNCHER];
    let ordinary = sde.types.iter().find(|row| row.published == Some(true) && row.category_id == Some(CATEGORY_MODULE)
        && !excluded.contains(&i64::from(row.group_id.unwrap_or(0)))).expect("ordinary published module").type_id;
    assert!(activation_in_scope(&sde, ordinary, EffectKind::Missile));
    let structure = sde.types.iter().find(|row| row.category_id == Some(CATEGORY_STRUCTURE)).expect("structure type").type_id;
    assert!(!activation_in_scope(&sde, structure, EffectKind::Missile));
    let structure_module = sde.types.iter().find(|row| row.category_id == Some(CATEGORY_STRUCTURE_MODULE)).expect("structure module").type_id;
    assert!(!activation_in_scope(&sde, structure_module, EffectKind::Mining));
}

#[test]
fn modifier_activation_requires_an_active_category() {
    let mut effect = Effect { id: 1, kind: EffectKind::Modifiers, guid: None, category: None, attributes: [None; 6], flags: 0, festival: None };
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
