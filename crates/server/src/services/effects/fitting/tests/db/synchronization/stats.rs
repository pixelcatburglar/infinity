//! Real SDE ship-stat modifiers through inventory movement and online transitions.
use infinity_core::consts::dogma::{ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_MASS, ATTR_MAX_VELOCITY};
use super::fixture::{self, MovementFixture};
/// SDE dogma effect category for passive modifiers.
const PASSIVE_EFFECT_CATEGORY: i32 = 0;
/// SDE dogma effect category for modifiers active while online.
const ONLINE_EFFECT_CATEGORY: i32 = 4;

async fn ship_stat_roundtrip(attribute: i32) {
    let f = MovementFixture::create_stat(attribute).await;
    let db = f.client.db.clone();
    let world = f.client.world.clone();
    let rows = f.rows;
    let result = tokio::spawn(async move {
        let mut f = f;
        let ship = rows.ship;
        let module = rows.modules[0];
        let base = f.value(ship, attribute).await;
        let mut previous_effective = None;
        for _ in 0..2 {
            assert_eq!(f.move_to(ship, ship, f.slot).await.ok().flatten(), Some(module));
            let installed = f.value(ship, attribute).await;
            let events = f.drain();
            if f.requires_online {
                assert_eq!(installed, base);
                assert!(fixture::attribute_rows(&events, rows.owner, ship, attribute).is_empty());
                f.set_online(ship, module, true).await.unwrap();
            }
            let effective = f.value(ship, attribute).await;
            assert_ne!(effective, base, "selected real SDE modifier must change the effective value");
            if let Some(previous) = previous_effective { assert_eq!(effective, previous); }
            previous_effective = Some(effective);
            let events = if f.requires_online { f.drain() } else { events };
            assert_eq!(fixture::attribute_rows(&events, rows.owner, ship, attribute), vec![(base, effective)]);
            assert_eq!(f.move_to(ship, ship, fixture::CARGO).await.ok().flatten(), Some(module));
            let restored = f.value(ship, attribute).await;
            assert_eq!(restored, base);
            assert_eq!(fixture::attribute_rows(&f.drain(), rows.owner, ship, attribute), vec![(effective, restored)]);
        }
    }).await;
    super::super::fixture::cleanup(&db, &world, rows).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn mass_modifier_install_remove_restores_ship() {
    ship_stat_roundtrip(ATTR_MASS).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn velocity_modifier_install_remove_restores_ship() {
    ship_stat_roundtrip(ATTR_MAX_VELOCITY).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn resistance_modifier_install_remove_restores_ship() {
    ship_stat_roundtrip(ATTR_ARMOR_EM_DAMAGE_RESONANCE).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn module_group_modifier_updates_surviving_receiver() {
    use infinity_core::{consts::{CATEGORY_MODULE, CATEGORY_SHIP, dogma::*}};
    use crate::{services::{fit::{self, Key}, skills}};
    let f = MovementFixture::create(ATTR_ARMOR_HP).await;
    let db = f.client.db.clone();
    let world = f.client.world.clone();
    let rows = f.rows;
    let result = tokio::spawn(async move {
        let mut f = f;
        let sde = &f.client.sde;
        let rack = |ty| super::super::super::super::rack(&sde.types, ty)
            .filter(|(_, attr, _)| [ATTR_LOW_SLOTS, ATTR_MED_SLOTS, ATTR_HI_SLOTS].contains(attr));
        let unrestricted = |ty: &crate::sde::types::TypeData| {
            ty.published == Some(true) && ty.category_id == Some(CATEGORY_MODULE)
                && rack(ty.type_id).is_some()
                && super::super::super::super::CAN_FIT_GROUPS.iter().chain(super::super::super::super::CAN_FIT_TYPES.iter())
                    .all(|a| ty.attribute(*a).unwrap_or(0.0) == 0.0)
        };
        let mut selected = None;
        'sources: for source in sde.types.iter().filter(|t| unrestricted(t)) {
            for entry in source.effects() {
                let Some(effect) = sde.dogma.effect(entry.effect_id) else { continue };
                if !matches!(effect.effect_category.unwrap_or(PASSIVE_EFFECT_CATEGORY), PASSIVE_EFFECT_CATEGORY | ONLINE_EFFECT_CATEGORY) { continue; }
                if effect.effect_category == Some(ONLINE_EFFECT_CATEGORY) && !source.effects().iter().any(|e| i64::from(e.effect_id) == EFFECT_ONLINE) { continue; }
                for m in effect.modifiers() {
                    if m.func != "LocationGroupModifier" || m.domain != "shipID" || m.group.is_none() || m.target_category.is_some()
                        || !matches!(m.op, fit::OP_MOD_ADD | fit::OP_MOD_SUB | fit::OP_POST_PERCENT | fit::OP_POST_PERCENT_UNNERFED | fit::OP_POST_MUL)
                        || !source.attribute(m.modifying).is_some_and(|v| v != 0.0 && (m.op != fit::OP_POST_MUL || v != 1.0)) { continue; }
                    let Some(receiver) = sde.types.iter().find(|t| unrestricted(t) && t.type_id != source.type_id
                        && t.group_id == m.group && t.fit_attribute(m.modified).is_some_and(|v| v != 0.0)) else { continue };
                    let (_, source_slots, source_flag) = rack(source.type_id).unwrap();
                    let (_, receiver_slots, receiver_flag) = rack(receiver.type_id).unwrap();
                    let needs_turret = receiver.effects().iter().chain(source.effects()).any(|e| e.effect_id == EFFECT_TURRET_FITTED);
                    let needs_launcher = receiver.effects().iter().chain(source.effects()).any(|e| e.effect_id == EFFECT_LAUNCHER_FITTED);
                    let Some(hull) = sde.types.iter().find(|h| h.published == Some(true) && h.category_id == Some(CATEGORY_SHIP)
                        && h.attribute(source_slots).unwrap_or(0.0) >= (if source_slots == receiver_slots { 2.0 } else { 1.0 })
                        && h.attribute(receiver_slots).unwrap_or(0.0) >= 1.0
                        && (!needs_turret || h.attribute(ATTR_TURRET_SLOTS_LEFT).unwrap_or(0.0) >= 1.0)
                        && (!needs_launcher || h.attribute(ATTR_LAUNCHER_SLOTS_LEFT).unwrap_or(0.0) >= 1.0)
                        && h.fit_attribute(ATTR_CAPACITY).unwrap_or(0.0) >= source.volume.unwrap_or(0.0) + receiver.volume.unwrap_or(0.0)) else { continue };
                    selected = Some((source.type_id, receiver.type_id, hull.type_id, source_flag,
                        receiver_flag + i32::from(source_flag == receiver_flag), m.modified, effect.effect_category == Some(ONLINE_EFFECT_CATEGORY)));
                    break 'sources;
                }
            }
        }
        let (source_type, receiver_type, hull_type, source_slot, receiver_slot, attribute, online) =
            selected.expect("SDE must contain a published compatible ship, source, and receiver for a supported location group modifier");
        for (item, ty) in [(rows.ship, hull_type), (rows.modules[0], source_type), (rows.modules[1], receiver_type)] {
            f.client.db.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1", &[&item, &ty]).await.unwrap();
        }
        let mut required = Vec::new();
        for ty in [source_type, receiver_type] {
            for (skill, _) in skills::requirements::requirements(&f.client.sde, ty) {
                if !required.iter().any(|&(id, _)| id == skill) { required.push((skill, 5)); }
            }
            for attr in ATTR_REQUIRED_SKILLS {
                if let Some(skill) = f.client.sde.types.get(ty).and_then(|t| t.attribute(attr)).filter(|v| *v > 0.0) {
                    let skill = skill as i32;
                    if !required.iter().any(|&(id, _)| id == skill) { required.push((skill, 5)); }
                }
            }
        }
        skills::gm::apply(&mut f.client, rows.owner, skills::gm::Change::Set(required)).await.ok().expect("seed module skills");
        let [source, receiver] = rows.modules;
        assert_eq!(f.move_module(rows.ship, rows.ship, receiver_slot, receiver).await.ok().flatten(), Some(receiver));
        let base = f.fit(rows.ship).await.value(Key::Module(receiver), attribute);
        f.drain();
        let mut previous = None;
        for _ in 0..2 {
            assert_eq!(f.move_module(rows.ship, rows.ship, source_slot, source).await.ok().flatten(), Some(source));
            let installed = f.fit(rows.ship).await.value(Key::Module(receiver), attribute);
            let events = f.drain();
            if online {
                assert_eq!(installed, base);
                assert!(fixture::attribute_rows(&events, rows.owner, receiver, attribute).is_empty());
                f.set_online(rows.ship, source, true).await.unwrap();
            }
            let effective = f.fit(rows.ship).await.value(Key::Module(receiver), attribute);
            assert_ne!(effective, base, "real supported modifier must affect the fitted receiver");
            if let Some(value) = previous { assert_eq!(effective, value); }
            previous = Some(effective);
            let events = if online { f.drain() } else { events };
            assert_eq!(fixture::attribute_rows(&events, rows.owner, receiver, attribute), vec![(base, effective)]);
            assert_eq!(f.move_module(rows.ship, rows.ship, fixture::CARGO, source).await.ok().flatten(), Some(source));
            let restored = f.fit(rows.ship).await.value(Key::Module(receiver), attribute);
            assert_eq!(restored, base);
            assert_eq!(fixture::attribute_rows(&f.drain(), rows.owner, receiver, attribute), vec![(effective, restored)]);
        }
    }).await;
    super::super::fixture::cleanup(&db, &world, rows).await.unwrap();
    result.unwrap();
}
