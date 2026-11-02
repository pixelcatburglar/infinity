use super::*;
use crate::{db::Db, sde::Sde, services::tests::fixture};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external database lock"]
async fn grant_persists_first_starting_skill_level_and_points() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let races = crate::dao::sde::characters::races(&db).await.unwrap();
    let race = races.iter().find_map(|row| {
        sde.characters.race(row.race_id).filter(|race| !race.skills.is_empty())
    }).expect("SDE race with starting skills");
    let f = fixture::create(&db).await.unwrap();
    let result: anyhow::Result<()> = async {
        grant_starting_skills(&db, &sde, f.character, race.race_id as i32).await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let mut seen = std::collections::HashSet::new();
        let mut expected: Vec<_> = race.skills.iter().filter(|skill| seen.insert(skill.type_id)).map(|skill| {
            let rank = skill_type(&sde, skill.type_id).rank;
            (skill.type_id, skill.level, sp_for_level(rank, i64::from(skill.level)))
        }).collect();
        expected.sort_unstable();
        let mut actual: Vec<_> = crate::dao::skills::training::current(&db, f.character).await?
            .into_iter().map(|skill| (skill.type_id, skill.level, skill.skill_points)).collect();
        actual.sort_unstable();
        anyhow::ensure!(actual == expected, "persisted starting skills differ from SDE and skill point formula");
        Ok(())
    }.await;
    fixture::cleanup(&db, f).await.unwrap();
    result.unwrap();
}
