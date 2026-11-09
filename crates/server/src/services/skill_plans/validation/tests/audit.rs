//! Startup audit retains legacy invalid plans while rejecting new invalid writes.
use std::{io, sync::{Arc, Mutex}};
use tracing::instrument::WithSubscriber;
use crate::{dao::skills::plans, services::{identity::Identity, tests::fixture}};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.0.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len()) }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

#[tokio::test]
#[ignore = "requires an isolated dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn audit_warns_with_each_legacy_plan_identity_and_refuses_new_invalid_writes() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let unknown = (1..i32::MAX).find(|id| sde.types.get(*id).is_none()).unwrap();
    let unpublished = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SKILL) && t.published == Some(false)).expect("unpublished skill in SDE").type_id;
    let obsolete = sde.types.iter().filter(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SKILL) && t.published == Some(true) && sde.types.attribute(t.type_id, super::super::ATTR_SKILL_IS_OBSOLETE).unwrap_or(0.0) != 0.0).min_by_key(|t| t.type_id).map(|t| t.type_id);
    if obsolete.is_none() {
        eprintln!("No published obsolete skill in loaded SDE; real obsolete audit case not exercised (synthetic predicate coverage is separate).");
    }
    let valid = sde.types.iter().find(|t| super::super::valid_skill(&sde, t.type_id) && sde.skill_plans.requirements(t.type_id).is_empty()).unwrap().type_id;
    let identity = Identity { char_id: owner.character, ship_id: None, corp_id: 0, station_id: None, system_id: None };
    let mut ids = Vec::new();
    for skill in [Some(unknown), Some(unpublished), obsolete].into_iter().flatten() {
        let id = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
        plans::create(&db, &id, owner.character, "legacy", "", &[skill], &[1]).await.unwrap();
        let new_id = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
        assert!(matches!(crate::services::skill_plans::personal::create(&identity, &db, &sde, &new_id, "new", "", &[skill], &[1]).await, Err(crate::services::skill_plans::Error::Precondition)));
        ids.push((id, skill));
    }
    let valid_id = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    crate::services::skill_plans::personal::create(&identity, &db, &sde, &valid_id, "valid", "", &[valid], &[1]).await.unwrap();
    // Native SDE Polaris skill, an unpublished self-loop.
    let cyclic = 9955;
    assert!(sde.skill_plans.is_cyclic(cyclic));
    let milestone = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    crate::dao::skills::plan_milestones::create(&db, &milestone, &valid_id, "skill", cyclic, Some(1)).await.unwrap();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let capture = Capture(bytes.clone());
    let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false).with_writer(move || capture.clone()).finish();
    super::super::audit(&db, &sde).with_subscriber(subscriber).await.unwrap();
    let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    let matching: Vec<_> = output.lines().filter(|line| line.contains("legacy skill plan has invalid static skill sequence") && line.contains(&format!("character_id={}", owner.character))).collect();
    assert_eq!(matching.len(), ids.len() + 1, "{output}");
    assert_eq!(output.lines().filter(|line| line.contains("cyclic milestone target") && line.contains(&format!("character_id={}", owner.character)) && line.contains(&format!("plan_id={valid_id:?}"))).count(), 1, "{output}");
    for (id, skill) in &ids {
        assert_eq!(matching.iter().filter(|line| line.contains(&format!("plan_id={id:?}"))).count(), 1, "{output}");
        let stored = plans::get(&db, id, Some(owner.character)).await.unwrap().unwrap();
        assert_eq!(&stored.plan_id, id);
        assert_eq!(stored.name, "legacy");
        assert_eq!(stored.description, "");
        assert_eq!(stored.skill_ids, vec![*skill]);
        assert_eq!(stored.levels, vec![1]);
        plans::delete(&db, owner.character, id).await.unwrap();
    }
    plans::delete(&db, owner.character, &valid_id).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
}
