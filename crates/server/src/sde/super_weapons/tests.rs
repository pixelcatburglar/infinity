use super::*;

#[test]
fn catalogue_admits_exactly_the_twelve_content_rows() {
    let catalogue = SuperWeapons::default();
    let cylinder = (Classification::Cylinder, Mode::Point, GuidFamily::DamageBeam, EFFECT_DOOMSDAY_BEAM_DOT);
    let slice = (Classification::Slice, Mode::Slash, GuidFamily::SlashBeam, EFFECT_DOOMSDAY_SLASH);
    let cone = (Classification::Cone, Mode::Point, GuidFamily::Cone, EFFECT_DOOMSDAY_CONE_DOT);
    let expected = [
        (40631, cylinder), (40632, slice), (40633, cone),
        (41439, cylinder), (41440, cylinder), (41441, cylinder),
        (41442, slice), (41443, slice), (41444, slice),
        (74251, cylinder), (74252, slice), (74253, cone),
    ];
    for (type_id, (classification, mode, family, effect)) in expected {
        let def = catalogue.get(type_id).unwrap_or_else(|| panic!("type {type_id} refused"));
        assert_eq!((def.type_id, def.classification, def.mode, def.family, def.effect),
            (type_id, classification, mode, family, effect), "type {type_id}");
        assert!(def.race_id.is_none(), "type {type_id} race resolved without type rows");
    }
}

#[test]
fn catalogue_refuses_every_unlisted_type() {
    let catalogue = SuperWeapons::default();
    // 40634 is the Hologram superweapon (refused class), 37474 and 62130 are
    // unadmitted neighbours; 74250 and 4489-4492 use the normal targeted path
    // and carry no positional catalogue row.
    for type_id in [40634, 37474, 62130, 74250, 4489, 4490, 4491, 4492, 0, i32::MAX] {
        assert!(catalogue.get(type_id).is_none(), "type {type_id} admitted");
    }
}

#[test]
fn proxy_angle_and_race_suffixes_are_exact() {
    assert_eq!(SuperWeapons::default().arc_path_half_angle_proxy(), 15.0);
    for (race, suffix) in [(1, "Caldari"), (2, "Minmatar"), (4, "Amarr"), (8, "Gallente")] {
        assert_eq!(race_suffix(Some(race)), Some(suffix), "race {race}");
    }
    for race in [None, Some(0), Some(3), Some(16)] {
        assert_eq!(race_suffix(race), None, "race {race:?}");
    }
}
