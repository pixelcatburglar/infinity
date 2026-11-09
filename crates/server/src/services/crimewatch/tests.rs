use super::*;
use crate::space::System;

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn entosis_activation_refreshes_only_the_weapons_timer() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let world = World::new(db.clone(), Arc::new(crate::sde::Sde::load(&db).await.unwrap()), 0);
    let character = -filetime_now();
    let before = filetime_now();
    weapons_activate(&world, character);
    let first = state().pilots.get(&character).unwrap().weapons_until;
    assert!(first >= weapons_deadline(before));
    {
        let mut st = state();
        st.pilots.get_mut(&character).unwrap().weapons_until = 1;
    }
    let before_refresh = filetime_now();
    weapons_activate(&world, character);
    let mut st = state();
    let pilot = st.pilots.get(&character).unwrap();
    assert!(pilot.weapons_until >= weapons_deadline(before_refresh));
    assert_eq!((pilot.pvp_until, pilot.criminal_until, pilot.suspect_until), (0, 0, 0));
    assert!(!st.engagements.keys().any(|&(a, b)| a == character || b == character));
    assert!(!st.concord.contains(&character));
    st.pilots.remove(&character);
}

#[test]
fn concord_reservation_respects_policy_without_clearing_criminal_flag() {
    let criminal = -104;
    state().pilots.entry(criminal).or_default().criminal_until = 10;

    assert!(!reserve_concord(false, 1.0, 0.5, criminal));
    assert!(!state().concord.contains(&criminal));
    assert!(is_criminal(criminal, 0));
    assert!(!reserve_concord(true, 0.4, 0.5, criminal));
    assert!(reserve_concord(true, 0.5, 0.5, criminal));
    assert!(!reserve_concord(true, 1.0, 0.5, criminal));

    let mut st = state();
    assert!(st.concord.remove(&criminal));
    st.pilots.remove(&criminal);
}

#[test]
fn discarded_concord_callback_clears_marker() {
    crate::tests::init_config();
    let criminal = -101;
    assert!(state().concord.insert(criminal));
    let callback = concord_response(PendingConcord(criminal), 1, Weak::new());
    assert!(state().concord.contains(&criminal));
    drop(callback);
    assert!(!state().concord.contains(&criminal));
}

#[test]
fn dropping_system_clears_queued_concord_marker() {
    crate::tests::init_config();
    let criminal = -102;
    assert!(state().concord.insert(criminal));
    let mut system = System::default();
    system.schedule(1, concord_response(PendingConcord(criminal), 1, Weak::new()));
    assert!(state().concord.contains(&criminal));
    drop(system);
    assert!(!state().concord.contains(&criminal));
}

#[test]
fn executed_concord_callback_allows_another_response() {
    crate::tests::init_config();
    let criminal = -103;
    assert!(state().concord.insert(criminal));
    let mut system = System::default();
    system.schedule(1, concord_response(PendingConcord(criminal), 1, Weak::new()));
    system.tick(1);
    assert!(state().concord.insert(criminal));
    drop(system);
    assert!(state().concord.remove(&criminal));
}

#[test]
fn concord_delay_uses_policy_and_clamps_security() {
    crate::tests::init_config();
    let policy = &crate::config::game().crimewatch;
    assert_eq!(concord_delay_seconds(1.0, policy.concord_base_delay_secs, policy.concord_delay_per_security_secs), 6);
    assert_eq!(concord_delay_seconds(0.9, policy.concord_base_delay_secs, policy.concord_delay_per_security_secs), 8);
    assert_eq!(concord_delay_seconds(0.1, policy.concord_base_delay_secs, policy.concord_delay_per_security_secs), 20);
    assert_eq!(concord_delay_seconds(0.75, 8, 40), 18);
}

#[test]
fn effective_crime_flag_preserves_both_timers() {
    let mut pilot = Pilot { suspect_until: 20, criminal_until: 10, ..Default::default() };
    assert_eq!(effective_flag(&pilot, 5), (2, CRIMINAL_TIMER, Some(10)));
    assert_eq!(effective_flag(&pilot, 10), (1, SUSPECT_TIMER, Some(20)));
    pilot.criminal_until = 30;
    assert_eq!(effective_flag(&pilot, 20), (2, CRIMINAL_TIMER, Some(30)));
    assert_eq!(effective_flag(&pilot, 30), (0, CRIMINAL_IDLE, None));
}

#[test]
fn suspect_is_a_legal_target_until_expiry() {
    let suspect = -105;
    state().pilots.entry(suspect).or_default().suspect_until = 20;
    assert!(is_criminal(suspect, 19));
    assert!(!is_criminal(suspect, 20));
    state().pilots.remove(&suspect);
}

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn startup_restores_active_kill_rights_without_economy_changes() {
    use crate::{dao::{kill_rights as rights, wallets::characters}, space::World};
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let now = crate::client::filetime_now();
    let account = -now;
    let owner = account + 1;
    let target = account + 2;
    let expired_target = account + 3;
    let used_target = account + 4;
    let corp = account + 5;
    let name = format!("restore-test-{now}");
    let password: Vec<u8> = Vec::new();
    db.execute("INSERT INTO game.accounts (user_id,user_name,password_hash,role) VALUES ($1,$2,$3,0)", &[&account, &name, &password]).await.unwrap();
    for (id, suffix) in [(owner, "owner"), (target, "target"), (expired_target, "expired"), (used_target, "used")] {
        let name = format!("{name}-{suffix}");
        db.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id,balance) VALUES ($1,$2,$3,1,1,1,$4,123)", &[&id, &account, &name, &corp]).await.unwrap();
    }
    let until = now + 600 * infinity_core::consts::FILETIME_SECOND;
    let expiry = now + 1200 * infinity_core::consts::FILETIME_SECOND;
    let mut ids = Vec::new();
    for to in [target, expired_target, used_target] {
        let right = db.transaction(|tx, _: &mut ()| Box::pin(async move {
            rights::create(tx, owner, to, now, expiry).await
        }), |value, _| async move { Ok(value) }).await.unwrap().unwrap();
        rights::activate(&db, right.kill_right_id, owner, now, until).await.unwrap().unwrap();
        ids.push(right.kill_right_id);
    }
    db.execute("UPDATE game.kill_rights SET expiry=$2 WHERE kill_right_id=$1", &[&ids[1], &now]).await.unwrap();
    db.execute("UPDATE game.kill_rights SET used_at=$2 WHERE kill_right_id=$1", &[&ids[2], &now]).await.unwrap();
    let selected: Vec<_> = rights::active_activations(&db, now).await.unwrap().into_iter().filter(|r| ids.contains(&r.kill_right_id)).map(|r| r.kill_right_id).collect();
    assert_eq!(selected, vec![ids[0]]);
    for id in [target, expired_target, used_target] {
        state().pilots.remove(&id);
    }
    let world = World::new(db.clone(), Arc::new(crate::sde::Sde::load(&db).await.unwrap()), 0);
    let before_balances = [characters::balance(&db, owner).await.unwrap(), characters::balance(&db, target).await.unwrap()];
    let before_notes = db.query("SELECT notification_id FROM game.notifications WHERE receiver_id = ANY($1)", &[&vec![owner, target, expired_target, used_target]]).await.unwrap().len();
    let restored = match crate::services::bounty::restore_activations(&db, &world).await {
        Ok(count) => count,
        Err(_) => panic!("startup restoration failed"),
    };
    assert!(restored >= 1);
    assert!(is_criminal(target, now));
    {
        let st = state();
        let pilot = st.pilots.get(&target).unwrap();
        assert_eq!(pilot.suspect_until, until);
        assert_eq!(effective_flag(pilot, now), (1, SUSPECT_TIMER, Some(until)));
        assert!(!st.pilots.contains_key(&expired_target));
        assert!(!st.pilots.contains_key(&used_target));
    }
    let criminal_until = until + infinity_core::consts::FILETIME_SECOND;
    state().pilots.get_mut(&target).unwrap().criminal_until = criminal_until;
    let repeated = match crate::services::bounty::restore_activations(&db, &world).await {
        Ok(count) => count,
        Err(_) => panic!("repeated restoration failed"),
    };
    assert_eq!(repeated, restored);
    {
        let st = state();
        let pilot = st.pilots.get(&target).unwrap();
        assert_eq!(pilot.suspect_until, until);
        assert_eq!(effective_flag(pilot, now), (2, CRIMINAL_TIMER, Some(criminal_until)));
    }
    let right = rights::get(&db, ids[0]).await.unwrap().unwrap();
    assert_eq!((right.activated_until, right.activated_by, right.used_at), (Some(until), Some(owner), None));
    assert_eq!([characters::balance(&db, owner).await.unwrap(), characters::balance(&db, target).await.unwrap()], before_balances);
    assert_eq!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id = ANY($1)", &[&vec![owner, target, expired_target, used_target]]).await.unwrap().len(), before_notes);
    for id in [target, expired_target, used_target] {
        state().pilots.remove(&id);
    }
    db.execute("DELETE FROM game.kill_rights WHERE kill_right_id = ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE user_id=$1", &[&account]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
}

#[test]
fn historical_killmail_nullable_snapshots_roundtrip() {
    let row = crate::dao::killmails::Killmail {
        kill_id: 1, kill_time: 2, solar_system_id: 3, victim_id: 4, victim_corp_id: 5,
        victim_alliance_id: None, victim_ship_type: 6, damage_taken: 7.0,
        final_char_id: None, final_corp_id: None, final_alliance_id: None,
        final_ship_type: 0, final_weapon_type: 0, final_damage: 0.0,
        final_sec_status: 0.0, isk_lost: 8.0, blob: String::new(),
    };
    let value = killmails::killmail_value(row, Some(9));
    assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn killmail_writer_indexes_war_and_rolls_back_with_reports() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let mut characters = Vec::new();
    let mut corps = Vec::new();
    for _ in 0..3 {
        let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
        let corp: i64 = tx.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES (nextval('game.corporations_corporation_id_seq')::text,nextval('game.corporations_corporation_id_seq')::text,$1,$1,1,1) RETURNING corporation_id", &[&character]).await.unwrap().get("corporation_id");
        tx.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&corp, &character]).await.unwrap();
        characters.push(character);
        corps.push(corp);
    }
    let alliance: i64 = tx.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES (nextval('game.alliances_alliance_id_seq')::text,nextval('game.alliances_alliance_id_seq')::text,$1,$1,$2,1) RETURNING alliance_id", &[&corps[0], &characters[0]]).await.unwrap().get("alliance_id");
    tx.execute("UPDATE game.corporations SET alliance_id=$1 WHERE corporation_id=$2", &[&alliance, &corps[0]]).await.unwrap();
    let war = crate::dao::wars::create(&tx, alliance, corps[1], 1, 3).await.unwrap().unwrap();
    let save = tx.savepoint("killmail_writer").await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    let mut attackers = [
        (characters[1], Contribution { damage: 20.0, ship_type: 1, weapon_type: 1, illegal: false }),
        (characters[2], Contribution { damage: 10.0, ship_type: 1, weapon_type: 1, illegal: false }),
    ];
    let source = crate::services::combat::damage::DamageSource { attacker_id: Some(characters[1]), source_ship_type: Some(7), weapon_type_id: Some(42), ..Default::default() };
    let (kill_id, _, _, _) = match write_killmail(&save, &mut outbox, 1, characters[0], corps[0], 1, &Lost::new(), &mut attackers, 4, None, &source).await {
        Ok(result) => result,
        Err(_) => panic!("killmail writer failed"),
    };
    let rows = save.query("SELECT kill_id FROM game.killmails WHERE victim_id=$1", &[&characters[0]]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<_, i64>("kill_id"), kill_id);
    let event = save.query_one("SELECT solar_system_id, event_time, kind FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::Killmail as i16), &kill_id]).await.unwrap();
    assert_eq!((event.get::<_, i64>("solar_system_id"), event.get::<_, i64>("event_time"), event.get::<_, i16>("kind")), (1, 4, crate::dao::map_telemetry::EventKind::Ship as i16));
    let projections = crate::dao::killmails::wars::by_kills(&save, &[kill_id]).await.unwrap();
    assert_eq!(projections.len(), 1);
    let projection = &projections[0];
    assert_eq!((projection.kill_id, projection.war_id, projection.attacker_side, projection.defender_side), (kill_id, war.war_id, corps[1], alliance));
    let notes = save.query("SELECT receiver_id,type_id,group_id,sender_id,data FROM game.notifications WHERE receiver_id=ANY($1) ORDER BY type_id", &[&[characters[0], characters[1]].as_slice()]).await.unwrap();
    assert_eq!(notes.len(), 2);
    for (note, receiver, kind) in [(&notes[0], characters[0], NOTIFICATION_KILL_REPORT_VICTIM), (&notes[1], characters[1], NOTIFICATION_KILL_REPORT_FINAL_BLOW)] {
        assert_eq!(note.get::<_, i64>("receiver_id"), receiver);
        assert_eq!(note.get::<_, i32>("type_id"), kind);
        assert_eq!(note.get::<_, i32>("group_id"), NOTIFICATION_GROUP_WAR);
        assert_eq!(note.get::<_, i64>("sender_id"), characters[0]);
        assert!(note.get::<_, String>("data").contains(&format!("killMailID: {kill_id}")));
    }
    save.execute("UPDATE game.corporations SET alliance_id=NULL WHERE corporation_id=$1", &[&corps[0]]).await.unwrap();
    let mut history = crate::dao::killmails::wars::killmails(&save, war.war_id).await.unwrap();
    assert_eq!(history.len(), 1);
    let row = history.pop().unwrap();
    assert_eq!((row.projection.attacker_side, row.projection.defender_side), (corps[1], alliance));
    assert_eq!(row.killmail.victim_alliance_id, Some(alliance));
    assert_eq!(row.killmail.final_char_id, Some(characters[1]));
    assert_eq!(row.killmail.final_corp_id, Some(corps[1]));
    assert_eq!((row.killmail.final_ship_type, row.killmail.final_weapon_type, row.killmail.final_damage), (7, 42, 20.0));
    assert_eq!(row.killmail.final_alliance_id, None);
    let value = killmails::killmail_value(row.killmail, Some(row.projection.war_id));
    let value = marshal::loads(&marshal::dumps(&value)).unwrap();
    let Value::Instance(_, fields) = value else { panic!("expected KeyVal") };
    assert_eq!(fields.get("killID"), Some(&Value::Int(kill_id)));
    assert_eq!(fields.get("victimAllianceID"), Some(&Value::Int(alliance)));
    assert_eq!(fields.get("finalCharacterID"), Some(&Value::Int(characters[1])));
    assert_eq!(fields.get("finalCorporationID"), Some(&Value::Int(corps[1])));
    assert_eq!(fields.get("finalAllianceID"), Some(&Value::None));
    assert_eq!(fields.get("warID"), Some(&Value::Int(war.war_id)));
    let (pod_id, _, _, _) = write_killmail(&save, &mut outbox, 1, characters[0], corps[0], infinity_core::consts::TYPE_CAPSULE, &Lost::new(), &mut [], 5, None, &Default::default()).await.unwrap();
    let pod = save.query_one("SELECT solar_system_id, event_time, kind FROM game.map_events WHERE source=$1 AND source_id=$2", &[&(crate::dao::map_telemetry::EventSource::Killmail as i16), &pod_id]).await.unwrap();
    assert_eq!((pod.get::<_, i64>("solar_system_id"), pod.get::<_, i64>("event_time"), pod.get::<_, i16>("kind")), (1, 5, crate::dao::map_telemetry::EventKind::Pod as i16));
    assert_eq!(save.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=ANY($2)", &[&(crate::dao::map_telemetry::EventSource::Killmail as i16), &&[kill_id, pod_id][..]]).await.unwrap().len(), 2);
    // Equal timestamps do not choose the final blow; the explicit impact source does.
    let ship = crate::dao::items::creation::positioned(&save, 1, characters[0], 1, [0.0; 3]).await.unwrap();
    let now = crate::client::filetime_now();
    let window = crate::config::game().crimewatch.killmail_attacker_window_secs.saturating_mul(FILETIME_SECOND);
    let entity = |id| crate::lua::events::combat::CombatEntity { item_id: id, type_id: 1, incarnation: [0; 16], character_id: Some(id), corporation_id: None, alliance_id: None, faction_id: None };
    let context = crate::lua::events::resources::FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: Some(1), station_id: None, origin: crate::lua::events::resources::FactOrigin::Natural };
    let contributions = [
        crate::space::combat::ContributionEntry { attacker_id: characters[0], source_ship_id: None, source_ship_type: Some(1), weapon_type_id: Some(1), damage: 99.0, last_damage_at: now.saturating_sub(window).saturating_sub(1), illegal: false, entity: entity(characters[0]), context, exact_damage: Default::default() },
        crate::space::combat::ContributionEntry { attacker_id: characters[1], source_ship_id: None, source_ship_type: Some(1), weapon_type_id: Some(1), damage: 20.0, last_damage_at: now, illegal: false, entity: entity(characters[1]), context, exact_damage: Default::default() },
        crate::space::combat::ContributionEntry { attacker_id: characters[2], source_ship_id: None, source_ship_type: Some(1), weapon_type_id: Some(1), damage: 10.0, last_damage_at: now, illegal: false, entity: entity(characters[2]), context, exact_damage: Default::default() },
    ];
    let impact = crate::services::combat::damage::DamageSource {
        attacker_id: Some(characters[2]), source_ship_type: Some(7), weapon_type_id: Some(42), ..Default::default()
    };
    ship_lost_in(&save, &mut outbox, 1, characters[0], corps[0], ship, 1, &Lost::new(), &impact, now, &contributions).await.unwrap();
    let mail = save.query_one("SELECT final_char_id, final_ship_type, final_weapon_type, final_damage, attacker_ids FROM game.killmails WHERE victim_item_id=$1", &[&ship]).await.unwrap();
    assert_eq!(mail.get::<_, Option<i64>>("final_char_id"), Some(characters[2]));
    assert_eq!((mail.get::<_, i32>("final_ship_type"), mail.get::<_, i32>("final_weapon_type"), mail.get::<_, f64>("final_damage")), (7, 42, 10.0));
    assert_eq!(mail.get::<_, Vec<i64>>("attacker_ids"), vec![characters[1], characters[2]]);
    save.rollback().await.unwrap();
    assert!(tx.query("SELECT source_id FROM game.map_events WHERE source=$1 AND source_id=ANY($2)", &[&(crate::dao::map_telemetry::EventSource::Killmail as i16), &&[kill_id, pod_id][..]]).await.unwrap().is_empty());
    assert!(tx.query("SELECT kill_id FROM game.killmails WHERE victim_id=$1", &[&characters[0]]).await.unwrap().is_empty());
    assert!(crate::dao::killmails::wars::by_kills(&tx, &[kill_id]).await.unwrap().is_empty());
    assert!(crate::dao::killmails::wars::killmails(&tx, war.war_id).await.unwrap().is_empty());
    assert!(tx.query_opt("SELECT character_id FROM game.characters WHERE character_id=$1", &[&characters[0]]).await.unwrap().is_some());
    tx.rollback().await.unwrap();
}
