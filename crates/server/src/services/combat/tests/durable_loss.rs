//! Dedicated database regressions for committed player damage and ship loss.
use super::*;
use infinity_core::{consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HI_SLOT_FIRST, TYPE_CAPSULE}};
use crate::{dao::{characters, insurance, items}, services::combat::damage::DamageSource};
use crate::tests::catch_scenario;

struct Fixture {
    db: Db,
    world: Arc<World>,
    account: i64,
    owner: i64,
    attackers: [i64; 2],
    ship: i64,
    ship_type: i32,
    system: i64,
    attacker_ships: [i64; 2],
    modules: [i64; 2],
    weapon_type: i32,
    effect: i64,
    generations: [u64; 2],
    sequences: [std::sync::atomic::AtomicU64; 2],
}

impl Fixture {
    async fn new() -> Self {
        crate::tests::init_config();
        let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let sde = Arc::new(Sde::load(&db).await.unwrap());
        let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
            && t.type_id != TYPE_CAPSULE && t.base_price.is_some_and(|v| v > 1.0)
            && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_RECHARGE_RATE,
                ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE]
                .iter().all(|&a| t.attribute(a).unwrap_or(0.0) > 0.0)).unwrap().type_id;
        let (weapon_type, effect) = sde.types.iter()
            .filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE))
            .find_map(|t| t.effects().iter().find_map(|e| {
                crate::services::effects::registry::resolve(&sde, e.effect_id)
                    .filter(|effect| effect.kind == crate::services::effects::EffectKind::Turret)
                    .map(|effect| (t.type_id, effect.id))
            })).expect("published turret module");
        let row = sde.map.systems().find(|s| s.constellation_id.is_some() && s.region_id.is_some()).unwrap();
        let (system, constellation, region) = (row.solar_system_id, row.constellation_id.unwrap(), row.region_id.unwrap());
        let (account, owner, attackers, ship, attacker_ships, modules) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
            let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
            let mut characters = Vec::new();
            for _ in 0..3 {
                characters.push(tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id,constellation_id,region_id,balance) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,$2,$3,$4,0) RETURNING character_id", &[&account, &system, &constellation, &region]).await?.get::<_, i64>("character_id"));
            }
            let owner = characters[0];
            let ship = items::creation::positioned(tx, ship_type, owner, system, [0.0; 3]).await?;
            characters::selection::set_ship(tx, owner, ship).await?;
            let attackers = [characters[1], characters[2]];
            let mut attacker_ships = [0; 2];
            let mut modules = [0; 2];
            for i in 0..2 {
                attacker_ships[i] = items::creation::positioned(tx, ship_type, attackers[i], system, [0.0; 3]).await?;
                characters::selection::set_ship(tx, attackers[i], attacker_ships[i]).await?;
                modules[i] = items::creation::rookie_ship(tx, weapon_type, attackers[i], attacker_ships[i], FLAG_HI_SLOT_FIRST).await?;
                tx.execute("UPDATE game.items SET online=true WHERE item_id=$1", &[&modules[i]]).await?;
            }
            Ok::<_, crate::db::Error>((account, owner, attackers, ship, attacker_ships, modules))
        }), |value, ()| async move { Ok(value) }).await.unwrap();
        let world = World::new(db.clone(), sde.clone(), 1);
        let fixture = Self { db, world, account, owner, attackers, ship, ship_type, system,
            attacker_ships, modules, weapon_type, effect: i64::from(effect),
            generations: [crate::space::targeting::next_generation(), crate::space::targeting::next_generation()],
            sequences: std::array::from_fn(|_| std::sync::atomic::AtomicU64::new(0)) };
        let result = catch_scenario(async {
            fixture.world.system(system).await.unwrap();
            for (item_id, pilot) in [(ship, owner), (attacker_ships[0], attackers[0]), (attacker_ships[1], attackers[1])] {
                let row = items::space_ships::FreeShip { item_id, type_id: ship_type, owner_id: pilot, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
                let prepared = crate::services::ship::balls::piloted(&fixture.db, sde.clone(), &row, pilot, 0.0).await.ok().unwrap();
                fixture.world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
            }
        }).await;
        if let Err(panic) = result {
            fixture.cleanup().await;
            std::panic::resume_unwind(panic);
        }
        fixture
    }

    fn source(&self, index: usize) -> DamageSource {
        DamageSource { attacker_id: Some(self.attackers[index]), source_ship_id: Some(self.attacker_ships[index]),
            source_ship_type: Some(self.ship_type), weapon_type_id: Some(self.weapon_type), ..Default::default() }
    }

    async fn hit(&self, index: usize, raw: [f64; 4], allowed: bool) -> Result<Option<damage::DamageOutcome>, crate::services::CallError> {
        let sequence = self.sequences[index].fetch_update(std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed, |n| n.checked_add(1)).expect("sequence overflow") + 1;
        let (ship, pilot, module, effect, weapon, generation, system) = (self.attacker_ships[index],
            self.attackers[index], self.modules[index], self.effect, self.weapon_type, self.generations[index], self.system);
        let weak = Arc::downgrade(&self.world);
        let attempt = self.world.with(system, move |s| {
            let facts = s.prepare_combat_source(ship, Some(module), Some(effect), Some(weapon),
                crate::lua::events::resources::FactOrigin::Natural, generation, sequence, 1,
                move |s| s.pilot_ball(pilot).is_some_and(|ball| ball.id == ship)).unwrap();
            crate::services::effects::space_source::PreparedCombatAttempt::new(facts, weak, system)
        }).await.unwrap();
        let mut source = self.source(index);
        source.facts = Some(attempt.facts());
        let result = damage::apply_damage_guarded(&self.db, &self.world, system, self.ship, raw, source, false, move |_| allowed).await;
        drop(attempt);
        result
    }

    async fn insured(&self) -> f64 {
        let price = insurance::value(&self.db, self.ship_type).await.unwrap();
        let base = self.world.sde().types.get(self.ship_type).unwrap().base_price;
        let price = price.as_ref().and_then(|p| p.adjusted).filter(|v| *v != 0.0)
            .or_else(|| price.as_ref().and_then(|p| p.average).filter(|v| *v != 0.0)).or(base).unwrap();
        let now = filetime_now();
        insurance::insure(&self.db, self.ship, self.owner, self.ship_type, 1.0, now, now + 3600 * FILETIME_SECOND).await.unwrap();
        crate::services::market::round2(price)
    }

    async fn cleanup(self) {
        if let Some(ship) = characters::selection::ship_id(&self.db, self.owner).await.unwrap() { self.world.remove_ball(self.system, ship).await; }
        self.world.remove_ball(self.system, self.ship).await;
        self.world.unregister_pilot_generation(self.owner, None);
        for i in 0..2 {
            self.world.remove_ball(self.system, self.attacker_ships[i]).await;
            self.world.unregister_pilot_generation(self.attackers[i], None);
        }
        self.db.execute("DELETE FROM game.map_events WHERE source=$1 AND source_id IN (SELECT kill_id FROM game.killmails WHERE victim_item_id=$2)", &[&(crate::dao::map_telemetry::EventSource::Killmail as i16), &self.ship]).await.unwrap();
        let characters = vec![self.owner, self.attackers[0], self.attackers[1]];
        let account = self.account;
        let subjects = [self.owner, self.attackers[0], self.attackers[1]];
        self.db.transaction(|tx, _: &mut ()| Box::pin(async move {
            crate::services::tests::fixture::cleanup_progress(tx, &[account, subjects[0], subjects[1], subjects[2]], &subjects).await
        }), |(), ()| async move { Ok(()) }).await.unwrap();
        let kills: Vec<i64> = self.db.query("SELECT kill_id FROM game.killmails WHERE victim_item_id=$1", &[&self.ship]).await.unwrap().iter().map(|row| row.get("kill_id")).collect();
        for kill in &kills {
            let job = format!("{}:{kill}", crate::services::bounty::payout::KIND);
            self.db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap();
            self.db.execute("DELETE FROM game.bounty_payouts WHERE killmail_id=$1", &[&kill]).await.unwrap();
        }
        self.db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1)", &[&characters]).await.unwrap();
        let messages: Vec<i64> = self.db.query("SELECT DISTINCT m.message_id FROM game.mail_messages m LEFT JOIN game.mail_status s ON s.message_id=m.message_id WHERE m.sender_id=ANY($1) OR s.char_id=ANY($1)", &[&characters]).await.unwrap().iter().map(|row| row.get("message_id")).collect();
        self.db.execute("DELETE FROM game.mail_status WHERE message_id=ANY($1)", &[&messages]).await.unwrap();
        self.db.execute("DELETE FROM game.mail_messages WHERE message_id=ANY($1)", &[&messages]).await.unwrap();
        self.db.execute("DELETE FROM game.insurance WHERE ship_id=$1", &[&self.ship]).await.unwrap();
        self.db.execute("DELETE FROM game.killmails WHERE victim_item_id=$1", &[&self.ship]).await.unwrap();
        self.db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=ANY($1)", &[&characters]).await.unwrap();
        self.db.execute("DELETE FROM game.items WHERE owner_id=ANY($1)", &[&characters]).await.unwrap();
        for id in [self.owner, self.attackers[0], self.attackers[1]] { self.db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&id]).await.unwrap(); }
        self.db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&self.account]).await.unwrap();
    }
}

async fn assert_loss(f: &Fixture, payout: f64, winner: &damage::DamageOutcome, source: &damage::DamageSource) {
    assert_eq!(crate::db::outbox::tests::events(&f.db, "ShipLost", f.owner).await,
        vec![crate::lua::events::GameEvent::ShipLost { char_id: f.owner, ship_id: f.ship, type_id: f.ship_type, system_id: f.system }]);
    let journal = f.db.query("SELECT source_namespace,source_key FROM game.progress_events WHERE kind=$1 AND subject_character_id=$2", &[&"ShipLost", &f.owner]).await.unwrap();
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].get::<_, String>("source_namespace"), "business");
    assert_eq!(journal[0].get::<_, String>("source_key"), format!("ShipLost:{}", f.ship));
    let capsule = characters::selection::ship_id(&f.db, f.owner).await.unwrap().unwrap();
    assert_ne!(capsule, f.ship);
    assert_eq!(f.db.query("SELECT item_id FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&f.owner, &TYPE_CAPSULE]).await.unwrap().len(), 1);
    assert!(insurance::contract(&f.db, f.ship).await.unwrap().is_none());
    assert_eq!(crate::services::wallets::balance(&f.db, f.owner).await.ok().unwrap(), crate::isk::IskAmount::from_isk(payout).unwrap());
    let rows = f.db.query("SELECT final_char_id,final_ship_type,final_weapon_type,final_damage,damage_taken FROM game.killmails WHERE victim_item_id=$1", &[&f.ship]).await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.get::<_, Option<i64>>("final_char_id"), source.attacker_id);
    assert_eq!(row.get::<_, i32>("final_ship_type"), source.source_ship_type.unwrap());
    assert_eq!(row.get::<_, i32>("final_weapon_type"), source.weapon_type_id.unwrap());
    assert_eq!(row.get::<_, f64>("final_damage"), winner.dealt);
    assert_eq!(row.get::<_, f64>("damage_taken"), winner.dealt);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn concurrent_insured_lethal_hits_commit_one_winner_and_one_payout() {
    let f = Fixture::new().await;
    let result = catch_scenario(async {
    let payout = f.insured().await;
    let (a, b) = tokio::join!(f.hit(0, [1e30; 4], true), f.hit(1, [1e30; 4], true));
    let (a, b) = (a.ok().unwrap(), b.ok().unwrap());
    assert_ne!(a.is_some(), b.is_some());
    let (winner, source) = if let Some(winner) = a.as_ref() { (winner, f.source(0)) } else { (b.as_ref().unwrap(), f.source(1)) };
    assert!(winner.lethal);
    assert_loss(&f, payout, winner, &source).await;
    let later = f.hit(1, [0.0; 4], true).await.ok().unwrap();
    assert!(later.is_none());
    assert_loss(&f, payout, winner, &source).await;
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn serialization_failure_inside_loss_retries_real_damage_once() {
    let f = Fixture::new().await;
    let name = format!("durable_loss_fault_{}", f.ship);
    let sequence = format!("game.{name}_seq");
    let function = format!("game.{name}_fn");
    let (mut sequence_created, mut function_created, mut trigger_created) = (false, false, false);
    let result = catch_scenario(async {
    let prefix = "durable_loss_fault_";
    for query in [
        "SELECT format('DROP TRIGGER IF EXISTS %I ON %I.%I', t.tgname, n.nspname, c.relname) AS statement FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='game' AND NOT t.tgisinternal AND left(t.tgname, length($1))=$1",
        "SELECT format('DROP FUNCTION IF EXISTS %I.%I(%s)', n.nspname, p.proname, pg_catalog.pg_get_function_identity_arguments(p.oid)) AS statement FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='game' AND p.prokind='f' AND left(p.proname, length($1))=$1",
        "SELECT format('DROP SEQUENCE IF EXISTS %I.%I', n.nspname, c.relname) AS statement FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='game' AND c.relkind='S' AND left(c.relname, length($1))=$1",
    ] {
        for row in f.db.query(query, &[&prefix]).await.unwrap() {
            let statement: String = row.get("statement");
            f.db.execute(&statement, &[]).await.unwrap();
        }
    }
    let payout = f.insured().await;
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(f.owner, sender);
    f.world.set_online_session(f.owner, Some(infinity_core::session::OnlineSession { character_id: f.owner, user_id: None, session_id: 1, ship_id: Some(f.ship), station_id: None, system_id: Some(f.system), corporation_id: Some(1), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 }));
    f.db.execute(&format!("CREATE SEQUENCE {sequence}"), &[]).await.unwrap();
    sequence_created = true;
    f.db.execute(&format!("CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.victim_item_id = {} AND nextval('{sequence}') = 1 THEN RAISE EXCEPTION 'fixture serialization failure' USING ERRCODE = '40001'; END IF; RETURN NEW; END $$", f.ship), &[]).await.unwrap();
    function_created = true;
    f.db.execute(&format!("CREATE TRIGGER {name} BEFORE INSERT ON game.killmails FOR EACH ROW EXECUTE FUNCTION {function}()"), &[]).await.unwrap();
    trigger_created = true;
    let ship = f.ship;
    let prehit = f.world.with(f.system, move |s| s.combat_condition_snapshot(ship).unwrap().condition).await.unwrap();
    let result = f.hit(0, [1e30; 4], true).await;
    let count: i64 = f.db.query_one(&format!("SELECT last_value FROM {sequence}"), &[]).await.unwrap().get("last_value");
    let winner = result.ok().unwrap().unwrap();
    assert_eq!(count, 2);
    assert!(winner.lethal && winner.dealt > 0.0);
    assert_loss(&f, payout, &winner, &f.source(0)).await;
    let later = f.hit(1, [0.0; 4], true).await.ok().unwrap();
    assert!(later.is_none());
    assert_loss(&f, payout, &winner, &f.source(0)).await;
    let mut destroyed = 0;
    let mut changes = 0;
    while let Ok((event, payload)) = receiver.try_recv() {
        destroyed += usize::from(event == "__ship_destroyed");
        changes += usize::from(event == "OnModuleAttributeChanges");
        if event == "OnModuleAttributeChanges" {
            let [Value::List(rows)] = payload.as_slice() else { panic!("expected change list") };
            let ids = [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE];
            let expected_new = [0.0, prehit.stats.max[1], prehit.stats.max[2]];
            let expected_old = [prehit.now[0], prehit.stats.max[1] - prehit.now[1], prehit.stats.max[2] - prehit.now[2]];
            assert_eq!(rows.len(), 3);
            for (i, change) in rows.iter().enumerate() {
                let Value::Tuple(row) = change else { panic!("expected attribute tuple") };
                assert_eq!(row.len(), 8);
                assert_eq!(&row[..4], &[Value::str("OnModuleAttributeChange"), Value::Int(f.owner), Value::Int(f.ship), Value::Int(i64::from(ids[i]))]);
                assert_eq!(row[5], Value::Float(expected_new[i]));
                let Value::Float(old) = &row[6] else { panic!("expected old value") };
                if i == 0 {
                    assert!(*old >= expected_old[i] && *old <= prehit.stats.max[i]);
                } else {
                    assert_eq!(*old, expected_old[i]);
                }
                assert_eq!(row[7], Value::None);
            }
        }
    }
    assert_eq!((destroyed, changes), (1, 1));
    }).await;
    let mut cleanup_errors = Vec::new();
    if trigger_created {
        if let Err(error) = f.db.execute(&format!("DROP TRIGGER {name} ON game.killmails"), &[]).await { cleanup_errors.push(error.to_string()); }
    }
    if function_created {
        if let Err(error) = f.db.execute(&format!("DROP FUNCTION {function}()"), &[]).await { cleanup_errors.push(error.to_string()); }
    }
    if sequence_created {
        if let Err(error) = f.db.execute(&format!("DROP SEQUENCE {sequence}"), &[]).await { cleanup_errors.push(error.to_string()); }
    }
    f.cleanup().await;
    assert!(cleanup_errors.is_empty(), "fixture DDL cleanup failed: {cleanup_errors:?}");
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn typed_hit_notifies_immediately_and_checkpoints_all_layers() {
    let f = Fixture::new().await;
    let result = catch_scenario(async {
    let fit = Fit::load_in_space(&f.db, &f.world, f.system, f.owner, f.ship).await.ok().unwrap();
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    let gate = f.world.lock_condition_snapshot(f.system).await.unwrap();
    let ids = [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE];
    let old = [1.0, stats.max[1] - 1.0, 0.0];
    let at = filetime_now() + 60 * FILETIME_SECOND;
    assert_eq!(items::dogma::save_conditions(&f.db, f.system, &ids, &[(f.ship, old, at)]).await.unwrap(), vec![f.ship]);
    let ship = f.ship;
    let seeded = Condition { stats: stats.clone(), now: [old[0], 1.0, stats.max[2]], shield_at: at };
    f.world.with(f.system, move |s| s.install_condition(ship, seeded, fit)).await.unwrap();
    let before_rows = items::dogma::overrides(&f.db, f.ship).await.unwrap().iter()
        .map(|r| (r.attribute_id, r.value, r.updated_at)).collect::<Vec<_>>();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(f.owner, sender);
    assert!(f.hit(1, [1e30; 4], false).await.ok().unwrap().is_none());
    assert!(receiver.try_recv().is_err());
    let raw = 1.0 / stats.resonance[0][0] + 1.0 / stats.resonance[1][0] + stats.max[2] * 0.1 / stats.resonance[2][0];
    let before = filetime_now();
    let hit = f.hit(0, [raw, 0.0, 0.0, 0.0], true).await.ok().unwrap().unwrap();
    let after = filetime_now();
    let condition = f.world.with(f.system, move |s| s.combat_condition_snapshot(ship).unwrap().condition).await.unwrap();
    let values = [condition.now[0], condition.stats.max[1] - condition.now[1], condition.stats.max[2] - condition.now[2]];
    assert!(!hit.lethal && hit.dealt > 0.0);
    assert!(values[0] < old[0] && values[1] > old[1] && values[2] > old[2]);
    let rows = items::dogma::overrides(&f.db, f.ship).await.unwrap();
    assert_eq!(rows.iter().map(|r| (r.attribute_id, r.value, r.updated_at)).collect::<Vec<_>>(), before_rows);
    assert!((hit.dealt - ((old[0] - values[0]) + (values[1] - old[1]) + (values[2] - old[2]))).abs() < 1e-8);
    let (event, payload) = receiver.try_recv().unwrap();
    assert_eq!(event, "OnModuleAttributeChanges");
    let [Value::List(changes)] = payload.as_slice() else { panic!("expected change list") };
    assert_eq!(changes.len(), 3);
    for i in 0..3 {
        let Value::Tuple(row) = &changes[i] else { panic!("expected attribute tuple") };
        assert_eq!(row.len(), 8);
        assert_eq!(&row[..4], &[Value::str("OnModuleAttributeChange"), Value::Int(f.owner), Value::Int(f.ship), Value::Int(i64::from(ids[i]))]);
        let Value::Float(value) = &row[5] else { panic!("expected new value") };
        assert!((*value - values[i]).abs() < 1e-8);
        let Value::Float(previous) = &row[6] else { panic!("expected old value") };
        assert!((*previous - old[i]).abs() < 1e-8);
        assert_eq!(row[7], Value::None);
    }
    drop(gate);
    f.world.flush_positions().await.unwrap();
    let rows = items::dogma::overrides(&f.db, f.ship).await.unwrap();
    let saved = ids.map(|id| rows.iter().find(|row| row.attribute_id == id).unwrap().value);
    assert!(saved[0] >= values[0]);
    assert!((saved[1] - values[1]).abs() < 1e-8);
    assert!((saved[2] - values[2]).abs() < 1e-8);
    let timestamp = rows.iter().find(|row| row.attribute_id == ATTR_SHIELD_CHARGE).unwrap().updated_at;
    assert!(timestamp >= before && timestamp >= after);
    let fit = Fit::load_in_space(&f.db, &f.world, f.system, f.owner, f.ship).await.ok().unwrap();
    let reloaded = prepare_condition_with_fit(&f.db, f.ship, &fit).await.ok().unwrap().fractions();
    assert!((reloaded[1] - (1.0 - values[1] / stats.max[1])).abs() < 1e-9);
    assert!((reloaded[2] - (1.0 - values[2] / stats.max[2])).abs() < 1e-9);
    let elapsed = filetime_now() - 60 * FILETIME_SECOND;
    assert_eq!(items::dogma::save_conditions(&f.db, f.system, &ids,
        &[(f.ship, [stats.max[0] * 0.25, values[1], values[2]], elapsed)]).await.unwrap(), vec![f.ship]);
    let recharged = prepare_condition_with_fit(&f.db, f.ship, &fit).await.ok().unwrap().fractions();
    assert!(recharged[0] > 0.25 && recharged[0] <= 1.0);
    assert!((recharged[1] - reloaded[1]).abs() < 1e-9 && (recharged[2] - reloaded[2]).abs() < 1e-9);
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn position_failure_rolls_back_condition_and_retries_both() {
    let f = Fixture::new().await;
    let name = format!("combat_checkpoint_fault_{}", f.ship);
    let function = format!("game.{name}_fn");
    let (mut function_created, mut trigger_created) = (false, false);
    let result = catch_scenario(async {
        let gate = f.world.lock_condition_snapshot(f.system).await.unwrap();
        let rows = items::dogma::overrides(&f.db, f.ship).await.unwrap();
        let before = rows.iter().map(|r| (r.attribute_id, r.value, r.updated_at)).collect::<Vec<_>>();
        let initial = f.db.query_one("SELECT x,y,z FROM game.items WHERE item_id=$1", &[&f.ship]).await.unwrap();
        let initial_pos = [initial.get::<_, f64>("x"), initial.get::<_, f64>("y"), initial.get::<_, f64>("z")];
        let ship = f.ship;
        f.world.with(f.system, move |s| s.command(ship, |ball, _, _| {
            ball.pos = [1e12; 3];
            ball.vel = [0.0; 3];
            Vec::new()
        })).await.unwrap();
        assert!(f.world.after(f.system, 2_000).await);
        let selected_pos = f.world.with(f.system, move |s| s.ball(ship).unwrap().pos).await.unwrap();
        assert_ne!(selected_pos, initial_pos);
        let hit = f.hit(0, [1.0, 0.0, 0.0, 0.0], true).await.ok().unwrap().unwrap();
        assert!(hit.dealt > 0.0 && !hit.lethal);
        f.db.execute(&format!("CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.item_id = {} THEN RAISE EXCEPTION 'fixture position failure'; END IF; RETURN NEW; END $$", f.ship), &[]).await.unwrap();
        function_created = true;
        f.db.execute(&format!("CREATE TRIGGER {name} BEFORE UPDATE OF x ON game.items FOR EACH ROW EXECUTE FUNCTION {function}()"), &[]).await.unwrap();
        trigger_created = true;
        drop(gate);
        assert!(f.world.flush_positions().await.is_err());
        assert!(crate::db::outbox::tests::events(&f.db, "ShipLost", f.owner).await.is_empty());
        let rows = items::dogma::overrides(&f.db, f.ship).await.unwrap();
        assert_eq!(rows.iter().map(|r| (r.attribute_id, r.value, r.updated_at)).collect::<Vec<_>>(), before);
        let rolled_back = f.db.query_one("SELECT x,y,z FROM game.items WHERE item_id=$1", &[&f.ship]).await.unwrap();
        assert_eq!([rolled_back.get::<_, f64>("x"), rolled_back.get::<_, f64>("y"), rolled_back.get::<_, f64>("z")], initial_pos);
        assert!(f.world.with(f.system, move |s| { let at = s.combat_fact_time(); s.capture_dirty_condition(ship, true, at).is_some() }).await.unwrap());
        f.db.execute(&format!("DROP TRIGGER {name} ON game.items"), &[]).await.unwrap();
        trigger_created = false;
        f.db.execute(&format!("DROP FUNCTION {function}()"), &[]).await.unwrap();
        function_created = false;
        f.world.flush_positions().await.unwrap();
        let rows = items::dogma::overrides(&f.db, f.ship).await.unwrap();
        assert_ne!(rows.iter().map(|r| (r.attribute_id, r.value, r.updated_at)).collect::<Vec<_>>(), before);
        let saved = f.db.query_one("SELECT x,y,z FROM game.items WHERE item_id=$1", &[&f.ship]).await.unwrap();
        assert_eq!([saved.get::<_, f64>("x"), saved.get::<_, f64>("y"), saved.get::<_, f64>("z")], selected_pos);
        assert_eq!(f.world.with(f.system, move |s| s.ball(ship).unwrap().pos).await.unwrap(), selected_pos);
        assert!(!f.world.with(f.system, move |s| { let at = s.combat_fact_time(); s.capture_dirty_condition(ship, true, at).is_some() }).await.unwrap());
        assert!(crate::db::outbox::tests::events(&f.db, "ShipLost", f.owner).await.is_empty());
    }).await;
    let mut cleanup_errors = Vec::new();
    if trigger_created {
        if let Err(error) = f.db.execute(&format!("DROP TRIGGER {name} ON game.items"), &[]).await { cleanup_errors.push(error.to_string()); }
    }
    if function_created {
        if let Err(error) = f.db.execute(&format!("DROP FUNCTION {function}()"), &[]).await { cleanup_errors.push(error.to_string()); }
    }
    f.cleanup().await;
    assert!(cleanup_errors.is_empty(), "fixture DDL cleanup failed: {cleanup_errors:?}");
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn checkpoint_location_miss_keeps_condition_dirty_until_retry() {
    let f = Fixture::new().await;
    let result = catch_scenario(async {
        let gate = f.world.lock_condition_snapshot(f.system).await.unwrap();
        let before = items::dogma::overrides(&f.db, f.ship).await.unwrap().iter()
            .map(|r| (r.attribute_id, r.value, r.updated_at)).collect::<Vec<_>>();
        let hit = f.hit(0, [1.0, 0.0, 0.0, 0.0], true).await.ok().unwrap().unwrap();
        assert!(hit.dealt > 0.0 && !hit.lethal);
        f.db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&f.ship, &f.owner]).await.unwrap();
        drop(gate);
        f.world.flush_positions().await.unwrap();
        let rows = items::dogma::overrides(&f.db, f.ship).await.unwrap();
        assert_eq!(rows.iter().map(|r| (r.attribute_id, r.value, r.updated_at)).collect::<Vec<_>>(), before);
        assert!(crate::db::outbox::tests::events(&f.db, "ShipLost", f.owner).await.is_empty());
        let ship = f.ship;
        assert!(f.world.with(f.system, move |s| { let at = s.combat_fact_time(); s.capture_dirty_condition(ship, true, at).is_some() }).await.unwrap());
        f.db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&f.ship, &f.system]).await.unwrap();
        f.world.flush_positions().await.unwrap();
        assert!(!f.world.with(f.system, move |s| { let at = s.combat_fact_time(); s.capture_dirty_condition(ship, true, at).is_some() }).await.unwrap());
        let rows = items::dogma::overrides(&f.db, f.ship).await.unwrap();
        assert_ne!(rows.iter().map(|r| (r.attribute_id, r.value, r.updated_at)).collect::<Vec<_>>(), before);
    }).await;
    f.db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&f.ship, &f.system]).await.unwrap();
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
