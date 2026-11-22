//! Protocol-table and deployed-SDE closure coverage for ship stances.
use super::*;
use crate::db::Db;

#[test]
fn table_round_trips_six_hulls_and_unique_modes() {
    assert_eq!(HULL_STANCES.len(), 6);
    let mut seen = std::collections::HashSet::new();
    for stances in HULL_STANCES.iter() {
        assert!(std::ptr::eq(for_hull(stances.hull_type).unwrap(), stances));
        assert_eq!(stances.modes.iter().map(|&(key, _)| key).collect::<std::collections::BTreeSet<_>>().len(), 3);
        for &(key, mode) in &stances.modes {
            assert_eq!(mode_type(stances.hull_type, key), Some(mode));
            assert_eq!(stance_for_mode(stances.hull_type, mode), Some(key));
            assert!(seen.insert(mode), "mode type {mode} shared between hulls");
        }
        assert_eq!(mode_type(stances.hull_type, 0), None);
    }
    assert_eq!(seen.len(), 18);
    assert!(for_hull(0).is_none());
    assert!(HULL_STANCES.iter().all(|s| mode_type(s.hull_type, DEFAULT_STANCE) == Some(s.modes[0].1)));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn deployed_sde_covers_every_hull_mode_and_effect_source() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let (types, dogma) = tokio::try_join!(Types::load(&db), Dogma::load(&db)).unwrap();
    validate(&types, &dogma).unwrap();
    let mut resolved = Vec::new();
    for stances in HULL_STANCES.iter() {
        resolved.push(switch_time_ms(&types, &dogma, stances.hull_type).unwrap());
        for &(_, mode) in &stances.modes {
            let mut sources = std::collections::HashSet::new();
            for type_effect in types.get(mode).unwrap().effects() {
                for modifier in dogma.effect(type_effect.effect_id).unwrap().modifiers() {
                    assert!(dogma.attribute(modifier.modified).is_some());
                    sources.insert(modifier.modifying);
                }
            }
            assert!(!sources.is_empty(), "mode {mode} modifies nothing");
        }
    }
    assert!(resolved.iter().map(|v| v.to_bits()).collect::<std::collections::HashSet<_>>().len() > 1, "every hull resolved the same switch time");
}
