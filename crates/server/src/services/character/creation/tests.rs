use super::*;

#[test]
fn name_shape_checks_configured_boundaries_and_spacing() {
    crate::tests::init_config();
    let policy = &crate::config::game().characters;
    assert_eq!(name_shape_code(&"a".repeat((policy.name_min_len - 1) as usize)), NAME_TOO_SHORT);
    assert_eq!(name_shape_code(&"a".repeat(policy.name_min_len as usize)), NAME_VALID);
    assert_eq!(name_shape_code(&"a".repeat((policy.name_max_len + 1) as usize)), NAME_TOO_LONG);
    assert_eq!(name_shape_code("A  B"), NAME_CONSECUTIVE_SPACES);
    assert_eq!(name_shape_code(&format!("A{}", " B".repeat((policy.name_max_spaces + 1) as usize))), NAME_TOO_MANY_SPACES);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external database lock"]
async fn creation_rolls_back_skill_failure_and_commits_before_ship_provisioning() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let races = crate::dao::sde::characters::races(&db).await.unwrap();
    let (race, school) = races.iter().find_map(|race| {
        let loaded = sde.characters.race(race.race_id)?;
        if loaded.skills.is_empty() { return None; }
        sde.characters.schools_for_race(race.race_id as i32).find(|school| {
            school.corporation_id.is_some()
                && school.starting_stations.iter().filter_map(|&id| sde.map.station(id)).next().is_some()
                && school.starting_stations.iter().filter_map(|&id| sde.map.station(id))
                    .all(|station| station.solar_system_id.is_some() && station.constellation_id.is_some() && station.region_id.is_some())
        }).map(|school| (loaded, school))
    }).expect("SDE race with skills and a usable starting school");
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('creation-atomic-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"test"[..]]).await.unwrap().get("user_id");
    let failed_name = format!("Fail{account}");
    let success_name = format!("Success{account}");
    let result: anyhow::Result<()> = async {
        db.execute("CREATE FUNCTION game.test_creation_skill_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF EXISTS (SELECT 1 FROM game.characters WHERE character_id = NEW.character_id AND name = TG_ARGV[0]) THEN RAISE EXCEPTION 'creation fixture skill failure'; END IF; RETURN NEW; END $$", &[]).await?;
        // The fixture name contains only a fixed ASCII prefix and the numeric account ID.
        db.execute(&format!("CREATE TRIGGER test_creation_skill_failure BEFORE INSERT ON game.skills FOR EACH ROW EXECUTE FUNCTION game.test_creation_skill_failure('{failed_name}')"), &[]).await?;
        let pilot = NewPilot {
            name: failed_name.clone(), race: race.race_id, bloodline: 1, ancestry: None, gender: 1,
            school: Some(school.school_id), balance: 0.0, appearance: Vec::new(), portrait: Vec::new(),
        };
        let failure = create(&db, &sde, account, pilot).await;
        anyhow::ensure!(matches!(&failure, Err(CallError::Internal(error)) if error.chain().any(|cause| cause.downcast_ref::<tokio_postgres::Error>().and_then(|e| e.as_db_error()).is_some_and(|e| e.message() == "creation fixture skill failure"))), "expected fixture skill insertion failure");
        anyhow::ensure!(!characters::creation::name_taken(&db, &failed_name).await?, "character survived failed skill grant");
        let pilot = NewPilot {
            name: success_name, race: race.race_id, bloodline: 1, ancestry: None, gender: 1,
            school: Some(school.school_id), balance: 0.0, appearance: Vec::new(), portrait: Vec::new(),
        };
        let created = create(&db, &sde, account, pilot).await.map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let actual = crate::dao::skills::training::current(&db, created.char_id).await?;
        let mut seen = std::collections::HashSet::new();
        let mut expected: Vec<_> = race.skills.iter().filter(|skill| seen.insert(skill.type_id))
            .map(|skill| (skill.type_id, skill.level)).collect();
        expected.sort_unstable();
        let mut actual_skills: Vec<_> = actual.iter().map(|skill| (skill.type_id, skill.level)).collect();
        actual_skills.sort_unstable();
        anyhow::ensure!(actual_skills == expected, "starting skills differ from SDE race skills");
        anyhow::ensure!(db.query("SELECT item_id FROM game.items WHERE owner_id = $1", &[&created.char_id]).await?.is_empty(), "items provisioned before preparation");
        let ship_id: Option<i64> = db.query_one("SELECT ship_id FROM game.characters WHERE character_id = $1", &[&created.char_id]).await?.get("ship_id");
        anyhow::ensure!(ship_id.is_none(), "ship assigned before preparation");
        Ok(())
    }.await;
    let drop_trigger = db.execute("DROP TRIGGER IF EXISTS test_creation_skill_failure ON game.skills", &[]).await;
    let drop_function = db.execute("DROP FUNCTION IF EXISTS game.test_creation_skill_failure()", &[]).await;
    let delete_skills = db.execute("DELETE FROM game.skills WHERE character_id IN (SELECT character_id FROM game.characters WHERE user_id = $1)", &[&account]).await;
    let delete_items = db.execute("DELETE FROM game.items WHERE owner_id IN (SELECT character_id FROM game.characters WHERE user_id = $1)", &[&account]).await;
    let delete_characters = db.execute("DELETE FROM game.characters WHERE user_id = $1", &[&account]).await;
    let delete_account = db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await;
    drop_trigger.unwrap();
    drop_function.unwrap();
    delete_skills.unwrap();
    delete_items.unwrap();
    delete_characters.unwrap();
    delete_account.unwrap();
    result.unwrap();
}
