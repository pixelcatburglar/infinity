use super::*;

async fn clear_faults(db: &crate::db::Db) -> Result<()> {
    let triggers = db.query("SELECT t.tgname, n.nspname, c.relname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE NOT t.tgisinternal AND left(t.tgname, length('missile_admission_fault_')) = 'missile_admission_fault_'", &[]).await?;
    let functions = db.query("SELECT p.proname, n.nspname FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE left(p.proname, length('missile_admission_fault_')) = 'missile_admission_fault_' AND p.pronargs=0", &[]).await?;
    let sequences = db.query("SELECT c.relname, n.nspname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE c.relkind='S' AND left(c.relname, length('missile_admission_fault_')) = 'missile_admission_fault_'", &[]).await?;
    let mut error = None;
    for row in triggers {
        let (name, schema, table): (String, String, String) = (row.get("tgname"), row.get("nspname"), row.get("relname"));
        let quote = |v: &str| format!("\"{}\"", v.replace('\"', "\"\""));
        if let Err(e) = db.execute(&format!("DROP TRIGGER IF EXISTS {} ON {}.{}", quote(&name), quote(&schema), quote(&table)), &[]).await { error = Some(e); }
    }
    for row in functions {
        let (name, schema): (String, String) = (row.get("proname"), row.get("nspname"));
        let quote = |v: &str| format!("\"{}\"", v.replace('\"', "\"\""));
        if let Err(e) = db.execute(&format!("DROP FUNCTION IF EXISTS {}.{}()", quote(&schema), quote(&name)), &[]).await { error = Some(e); }
    }
    for row in sequences {
        let (name, schema): (String, String) = (row.get("relname"), row.get("nspname"));
        let quote = |v: &str| format!("\"{}\"", v.replace('\"', "\"\""));
        if let Err(e) = db.execute(&format!("DROP SEQUENCE IF EXISTS {}.{}", quote(&schema), quote(&name)), &[]).await { error = Some(e); }
    }
    if let Some(error) = error { return Err(error.into()); }
    Ok(())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn illegal_drone_missile_keeps_launch_source_after_disconnect() -> Result<()> {
    scenario(false).await
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn missile_impact_keeps_illegal_classification_when_apply_fails() -> Result<()> {
    scenario(true).await
}

async fn scenario(fault: bool) -> Result<()> {
    let mut f = Fixture::real_system().await;
    let result = catch_scenario(async {
        if fault { clear_faults(&f.db).await?; }
        let (victim, victim_ship) = f.add_pilot_with_signature(Some(10_000.0)).await;
        let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
            && [ATTR_DRONE_BANDWIDTH_USED, ATTR_MASS, ATTR_AGILITY, ATTR_MAX_VELOCITY].iter()
                .all(|attr| t.fit_attribute(*attr).is_some_and(|v| v.is_finite() && v > 0.0))
            && t.effects().iter().any(|e| registry::resolve(&f.sde, e.effect_id).is_some_and(|e| e.kind == EffectKind::Turret)))
            .map(|t| t.type_id).ok_or_else(|| anyhow!("published turret drone with valid dogma unavailable"))?;
        f.train_drone(ty).await;
        let source = f.drone(ty, 1, true).await;
        for (item, attr, value) in [(f.ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0),
            (source, ATTR_SCAN_RESOLUTION, 1_000_000.0), (source, ATTR_MAX_TARGET_RANGE, 1_000_000.0),
            (source, ATTR_MAX_RANGE, 1_000_000.0), (source, ATTR_RATE_OF_FIRE, 0.0),
            (victim_ship, ATTR_SHIELD_CAPACITY, 1_000_000.0),
            // Prevent recharge during missile flight from masking the small hit.
            (victim_ship, ATTR_SHIELD_RECHARGE_RATE, 1e30),
            (victim_ship, ATTR_SHIELD_EM_DAMAGE_RESONANCE, 1.0)] { f.set(item, attr, value).await; }
        // A drone effect may name a duration attribute other than rate of fire.
        // Suppress autonomous cycles while retaining the real lock and missile admission.
        let durations: Vec<_> = f.sde.types.get(ty).unwrap().effects().iter()
            .filter_map(|e| registry::resolve(&f.sde, e.effect_id))
            .filter_map(|e| e.attribute(crate::services::effects::registry::EffectAttribute::Duration))
            .collect();
        for attr in durations { f.set(source, attr, 0.0).await; }
        let fit = Fit::load_in_space(&f.db, &f.world, f.system, victim, victim_ship).await.map_err(|e| anyhow!("victim fit: {e:?}"))?;
        let (generation, revision, projection) = f.world.with(f.system, move |s| Some((s.combat_membership_generation(victim_ship)?, s.combat_fit_revision(victim_ship)?, s.projection_revision(victim_ship)))).await.flatten().ok_or_else(|| anyhow!("missing victim cache"))?;
        ensure!(f.world.with(f.system, move |s| s.replace_fitted_cache(victim_ship, generation, revision, fit, Key::Ship, Some(projection))).await == Some(true), "victim fit publication failed");
        let ship = f.ship;
        f.world.with(f.system, move |s| {
            // Place the engagement beyond every existing ball, including the system's sun.
            let x = s.balls().map(|b| b.pos[0] + f64::from(b.radius))
                .fold(0.0_f64, f64::max) + 1_000_000.0;
            for (id, offset) in [(ship, 0.0), (victim_ship, 200.0)] {
                s.command(id, move |b, _, _| { b.pos = [x + offset, 0.0, 0.0]; b.old_pos = b.pos; b.vel = [0.0; 3]; b.old_vel = b.vel; b.max_vel = 0.0; vec![] });
            }
        }).await.ok_or_else(|| anyhow!("actor stopped"))?;
        ensure!(f.world.after(f.system, 1_000).await, "actor stopped positioning engagement");
        launch(&mut f, source).await?;
        ensure!(f.world.with(f.system, move |s| s.drone(source).is_some_and(|d| d.spec.duration_ms < 1.0)).await == Some(true),
            "fixture drone must not fire autonomously while acquiring its lock");
        let guard = locked(&f, source, victim_ship).await?;
        let now = crate::client::filetime_now();
        ensure!(!crate::services::crimewatch::retaliation_allowed(f.owner, victim, now)
            && !crate::services::crimewatch::retaliation_allowed(victim, f.owner, now),
            "fixture pilots already have a crimewatch flag or engagement");
        let (from, target) = f.world.with(f.system, move |s| Some((s.sampled_ball(source)?, s.sampled_ball(victim_ship)?))).await.flatten().ok_or_else(|| anyhow!("missing balls"))?;
        ensure!([&from, &target].iter().all(|b| b.pos.iter().chain(b.vel.iter()).all(|v| v.is_finite())),
            "engagement has non-finite kinematics");
        let charge = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_CHARGE)
            && t.attribute(ATTR_AOE_CLOUD_SIZE).is_some_and(|v| v > 0.0)
            && t.attribute(ATTR_AOE_VELOCITY).is_some_and(|v| v > 0.0)
            && t.attribute(ATTR_AOE_DAMAGE_REDUCTION_FACTOR).is_some_and(|v| v > 0.0)).ok_or_else(|| anyhow!("missile charge unavailable"))?;
        let weapon = charge.type_id;
        let illegal = crate::services::crimewatch::classify(&f.db, &f.world, f.system, f.owner, victim).await.ok().ok_or_else(|| anyhow!("classification failed"))?;
        ensure!(illegal, "fresh high-security aggression should be illegal");
        if fault { ensure!(crate::config::game().crimewatch.security_loss_aggression > 0.0, "fault test requires nonzero aggression penalty"); }
        if fault {
            let name = format!("missile_admission_fault_{}", f.owner);
            f.db.execute(&format!("CREATE FUNCTION game.{name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'missile admission test' USING ERRCODE = '40001'; END $$"), &[]).await?;
            f.db.execute(&format!("CREATE TRIGGER {name} BEFORE UPDATE OF security_status ON game.characters FOR EACH ROW WHEN (OLD.character_id = {}) EXECUTE FUNCTION game.{name}()", f.owner), &[]).await?;
        }
        let security_before = crate::services::crimewatch::security_status(&f.db, f.owner).await.ok().ok_or_else(|| anyhow!("security lookup failed"))?;
        let missile = Missile { charge: weapon, raw: [1.0, 0.0, 0.0, 0.0],
            aoe_cloud: charge.attribute(ATTR_AOE_CLOUD_SIZE).unwrap(), aoe_velocity: charge.attribute(ATTR_AOE_VELOCITY).unwrap(),
            drf: charge.attribute(ATTR_AOE_DAMAGE_REDUCTION_FACTOR).unwrap(), velocity: 25.0, flight_range: 1_000_000.0,
            weapon, illegal: false };
        let cold_world = crate::space::World::new(f.db.clone(), f.sde.clone(), 1);
        let cold_missile = Missile { charge: weapon, raw: [1.0, 0.0, 0.0, 0.0],
            aoe_cloud: charge.attribute(ATTR_AOE_CLOUD_SIZE).unwrap(), aoe_velocity: charge.attribute(ATTR_AOE_VELOCITY).unwrap(),
            drf: charge.attribute(ATTR_AOE_DAMAGE_REDUCTION_FACTOR).unwrap(), velocity: 25.0, flight_range: 1_000_000.0,
            weapon, illegal: false };
        let cold_attempt = admitted(&f, guard, source, from.type_id).await?;
        tokio::time::timeout(Duration::from_secs(15), weapons::launch_missile(f.db.clone(), cold_world, f.system, from.clone(), target.clone(), f.owner, None, cold_missile, Some(guard), cold_attempt))
            .await.map_err(|_| anyhow!("non-market cold missile admission timed out"))?.map_err(|error| anyhow!("cold missile admission: {error:?}"))?;
        let source_type = from.type_id;
        let prehit = f.world.with(f.system, move |s| s.combat_condition_snapshot(victim_ship)).await.flatten()
            .ok_or_else(|| anyhow!("missing victim condition before launch"))?;
        let original_ship = f.world.ball(f.system, f.ship).await.ok_or_else(|| anyhow!("missing original ship"))?;
        let before = f.world.with(f.system, |s| s.balls().count()).await.ok_or_else(|| anyhow!("actor stopped"))?;
        let attempt = admitted(&f, guard, source, from.type_id).await?;
        weapons::launch_missile(f.db.clone(), f.world.clone(), f.system, from, target, f.owner, None, missile, Some(guard), attempt).await.ok().ok_or_else(|| anyhow!("launch failed"))?;
        ensure!(f.world.with(f.system, |s| s.balls().count()).await == Some(before + 1), "missile was not scheduled");
        if fault {
            ensure!(crate::services::crimewatch::security_status(&f.db, f.owner).await.ok() == Some(security_before), "security update did not fail");
        }
        ensure!(crate::services::crimewatch::weapons_timer(f.owner).is_some()
            && crate::services::crimewatch::retaliation_allowed(victim, f.owner, crate::client::filetime_now()), "accepted launch did not apply aggression");
        let (owner, ship) = (f.owner, f.ship);
        ensure!(f.world.with(f.system, move |s| s.drone_order(source, owner, ship, DroneOrder::ReturnHome)).await == Some(true), "recall failed");
        f.world.remove_ball(f.system, source).await;
        f.world.remove_ball(f.system, ship).await;
        f.world.unregister_pilot_generation(f.owner, None);
        let replacement = crate::dao::items::creation::positioned(&f.db, original_ship.type_id, f.owner, f.system, [0.0; 3]).await?;
        crate::dao::characters::selection::set_ship(&f.db, f.owner, replacement).await?;
        let mut replacement_ball = original_ship;
        replacement_ball.id = replacement;
        f.world.add_ship(f.system, replacement_ball).await?;
        let fit = Fit::load(&f.db, f.sde.clone(), f.owner, replacement).await.map_err(|error| anyhow!("replacement fit: {error:?}"))?;
        let condition = crate::services::combat::prepare_condition_with_fit(&f.db, replacement, &fit).await.map_err(|error| anyhow!("replacement condition: {error:?}"))?;
        f.world.with(f.system, move |s| s.install_condition(replacement, condition.into_condition(), fit)).await.ok_or_else(|| anyhow!("actor stopped"))?;
        ensure!(f.world.with(f.system, move |s| s.combat_condition_snapshot(victim_ship).map(|v| v.contributions)).await.flatten().ok_or_else(|| anyhow!("missing victim condition"))?.is_empty(), "impact occurred before source swap and disconnect");
        tokio::time::timeout(Duration::from_secs(35), async {
            loop {
                let rows = f.world.with(f.system, move |s| s.combat_condition_snapshot(victim_ship).map(|v| v.contributions)).await.flatten().ok_or_else(|| anyhow!("missing victim condition"))?;
                if let Some(row) = rows.iter().find(|r| r.attacker_id == f.owner && r.source_ship_id == Some(source)) {
                    ensure!(row.damage > 0.0 && row.illegal, "missing illegal positive contribution");
                    ensure!(row.source_ship_type == Some(source_type) && row.weapon_type_id == Some(weapon), "launch provenance changed");
                    return Ok::<(), anyhow::Error>(());
                }
                ensure!(f.world.after(f.system, 100).await, "actor stopped awaiting impact");
            }
        }).await.map_err(|_| anyhow!("impact timed out"))??;
        let hit = f.world.with(f.system, move |s| s.combat_condition_snapshot(victim_ship)).await.flatten()
            .ok_or_else(|| anyhow!("missing victim condition after impact"))?;
        ensure!(hit.revision > prehit.revision && hit.condition.now[0] < prehit.condition.now[0],
            "missile impact did not reduce victim shield HP");
        let before_zero = f.world.with(f.system, move |s| s.combat_condition_snapshot(victim_ship).map(|v| v.contributions)).await.flatten().ok_or_else(|| anyhow!("missing victim condition"))?;
        let original = before_zero.iter().find(|r| r.attacker_id == f.owner).ok_or_else(|| anyhow!("missing contribution"))?;
        let saved = (original.source_ship_id, original.source_ship_type, original.weapon_type_id, original.damage, original.last_damage_at, original.illegal);
        let (from, target) = f.world.with(f.system, move |s| Some((s.sampled_ball(replacement)?, s.sampled_ball(victim_ship)?))).await.flatten().ok_or_else(|| anyhow!("missing replacement or victim"))?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(victim, tx);
        let zero = Missile { charge: weapon, raw: [0.0; 4], aoe_cloud: charge.attribute(ATTR_AOE_CLOUD_SIZE).unwrap(),
            aoe_velocity: charge.attribute(ATTR_AOE_VELOCITY).unwrap(), drf: charge.attribute(ATTR_AOE_DAMAGE_REDUCTION_FACTOR).unwrap(),
            velocity: 1_000_000.0, flight_range: 1_000_000.0, weapon: from.type_id, illegal: false };
        let weak = std::sync::Arc::downgrade(&f.world);
        let system = f.system;
        let facts = f.world.with(system, move |s| {
            let generation = s.combat_membership_generation(replacement).unwrap();
            s.prepare_combat_source(replacement, None, None, None, crate::lua::events::resources::FactOrigin::Natural,
                generation, 1, 1, |_| true)
        }).await.ok_or_else(|| anyhow!("actor stopped"))?.map_err(|error| anyhow!("replacement source: {error:?}"))?;
        let attempt = crate::services::effects::space_source::PreparedCombatAttempt::new(facts, weak, system);
        weapons::launch_missile(f.db.clone(), f.world.clone(), f.system, from, target, f.owner, None, zero, None, attempt).await.ok().ok_or_else(|| anyhow!("zero missile launch failed"))?;
        tokio::time::timeout(Duration::from_secs(35), async {
            loop {
                while let Ok((event, args)) = rx.try_recv() {
                    if event == "OnDamageMessage" && has_field(&args, "source", replacement) { return Ok::<(), anyhow::Error>(()); }
                }
                ensure!(f.world.after(f.system, 100).await, "actor stopped awaiting zero impact");
            }
        }).await.map_err(|_| anyhow!("zero impact timed out"))??;
        let after_zero = f.world.with(f.system, move |s| s.combat_condition_snapshot(victim_ship).map(|v| v.contributions)).await.flatten().ok_or_else(|| anyhow!("missing victim condition"))?;
        let current = after_zero.iter().find(|r| r.attacker_id == f.owner).ok_or_else(|| anyhow!("missing contribution after zero hit"))?;
        ensure!((current.source_ship_id, current.source_ship_type, current.weapon_type_id, current.damage, current.last_damage_at, current.illegal) == saved, "zero hit stole provenance");
        Ok(())
    }).await;
    let fault_cleanup = if fault {
        let name = format!("missile_admission_fault_{}", f.owner);
        let trigger = f.db.execute(&format!("DROP TRIGGER IF EXISTS {name} ON game.characters"), &[]).await;
        let function = f.db.execute(&format!("DROP FUNCTION IF EXISTS game.{name}()"), &[]).await;
        let leftovers = clear_faults(&f.db).await;
        trigger.map(|_| ()).map_err(anyhow::Error::from)
            .and(function.map(|_| ()).map_err(anyhow::Error::from)).and(leftovers)
    } else { Ok(()) };
    f.cleanup().await;
    fault_cleanup?;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}
