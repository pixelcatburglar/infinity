use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn native_prerequisites_preserve_zero_and_existing_dogma_edges() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = &crate::sde::Sde::load(&db).await.unwrap();
    let zero = sde.types.iter().find_map(|ty| requirements(sde, ty.type_id).find(|(_, level)| *level == 0).map(|edge| (ty.type_id, edge))).expect("native prerequisite with level zero");
    assert!(requirements(sde, zero.0).any(|edge| edge == zero.1));
    assert!(0 >= zero.1.1, "a missing trained skill has level zero and must satisfy this edge");
    for ty in sde.types.iter() {
        let native: std::collections::BTreeSet<_> = requirements(sde, ty.type_id).collect();
        for (skill_attr, level_attr) in infinity_core::consts::dogma::ATTR_REQUIRED_SKILLS.into_iter().zip(infinity_core::consts::dogma::ATTR_REQUIRED_SKILL_LEVELS) {
            let Some(skill) = sde.types.attribute(ty.type_id, skill_attr) else { continue; };
            let Some(level) = sde.types.attribute(ty.type_id, level_attr) else { continue; };
            if skill > 0.0 && level > 0.0 { assert!(native.contains(&(skill as i32, level as i32)), "missing prerequisite for {}", ty.type_id); }
        }
    }
}
