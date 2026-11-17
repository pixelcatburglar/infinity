//! Real regression: a rookie hull provisioned through the character-selection
//! path carries a fitted turret whose type declares no charge group or size.
//! Targeting and activation run through the ordinary `dogmaIM` calls, so a
//! refusal exercises the live admission path rather than a synthetic fixture.
use super::*;
use infinity_core::consts::{dogma::ATTR_CAPACITOR_CAPACITY, dogma::ATTR_CPU_OUTPUT, dogma::ATTR_POWER_OUTPUT, dogma::ATTR_QUANTITY};
use crate::{client::Client, config::deployment::DatabaseConfig, dao::items::dogma,
    services::{character::provisioning::ensure_ship, combat::{self, targeting_charges::loaded}, Call, CallResult}};

async fn call(client: &mut Client, method: &str, args: &[Value]) -> CallResult {
    let kwargs = Value::Dict(vec![]);
    crate::services::dogma::dispatch(client, &Call { service: "dogmaIM", method, args, kwargs: &kwargs, bound: false, bind_params: None }).await
}

/// `OnModuleAttributeChanges` rows carrying a stack-quantity change for one flag.
fn quantity_changes(args: &[Value], ship: i64, flag: i32) -> usize {
    let [Value::List(rows)] = args else { return 0 };
    rows.iter().filter(|row| {
        let Value::Tuple(fields) = row else { return false };
        if fields.get(3).and_then(Value::as_int) != Some(i64::from(ATTR_QUANTITY)) { return false; }
        let Some(Value::Tuple(key)) = fields.get(2) else { return false };
        key.first().and_then(Value::as_int) == Some(ship) && key.get(1).and_then(Value::as_int) == Some(i64::from(flag))
    }).count()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn provisioned_civilian_turret_cycles_without_charges() {
    crate::tests::init_config();
    // The test database may be an older dump; this first connection applies the
    // startup schema, and read-only queries prove the migration before the run.
    let db = crate::db::Db::connect(&DatabaseConfig { schema_on_start: true, ..crate::config::tests::test_database() }).await.unwrap();
    assert!(db.query_one("SELECT to_regclass('game.schema_application_history') IS NOT NULL AS e", &[]).await.unwrap().get::<_, bool>("e"), "schema history missing");
    let history: i64 = db.query_one("SELECT count(*) AS n FROM game.schema_application_history", &[]).await.unwrap().get("n");
    assert!(history > 0, "schema application not recorded");
    let columns: Vec<String> = db.query("SELECT column_name FROM information_schema.columns WHERE table_schema='game' AND table_name='skill_plans' ORDER BY column_name", &[]).await.unwrap().iter().map(|row| row.get("column_name")).collect();
    eprintln!("schema_application_history={history}; game.skill_plans columns: {columns:?}");
    for column in ["corporation_id", "category_id"] {
        assert!(columns.iter().any(|name| name.as_str() == column), "schema migration missing skill_plans.{column}");
    }
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().unwrap().station_id;
    let (race, hull) = sde.characters.races().find_map(|row| {
        let ship = row.ship_type_id.filter(|ship| sde.rookie_fits.get(*ship).is_some())?;
        Some((row.race_id as i32, ship))
    }).expect("no rookie fit among the race ships");
    let world = World::new(db.clone(), sde.clone(), 1);
    let mut lease = (None, None, None, None, None);
    let result = crate::tests::catch_scenario(async {
        let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('civilian-turret-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
        lease.0 = Some(account);
        let character: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id) VALUES ($1,'civilian-turret-' || nextval('game.characters_character_id_seq')::text,$2,1,1,1,$3) RETURNING character_id", &[&account, &race, &station]).await.unwrap().get("character_id");
        lease.1 = Some(character);
        let ship = ensure_ship(&db, &sde, character, race, station).await.expect("rookie provisioning");
        lease.2 = Some(ship);
        let fitted = db.query("SELECT item_id, type_id, flag_id FROM game.items WHERE owner_id=$1 AND location_id=$2 ORDER BY flag_id", &[&character, &ship]).await.unwrap();
        let (gun, flag, effect_name) = fitted.iter().find_map(|row| {
            let type_id: i32 = row.get("type_id");
            sde.types.effects(type_id).iter().find_map(|effect| {
                let name = sde.dogma.effect(effect.effect_id)?.name.clone()?;
                (effect.is_default && registry::resolve(&sde, effect.effect_id).is_some_and(|resolved| resolved.kind == EffectKind::Turret)
                    && sde.dogma.effects_by_name(&name).count() == 1)
                    .then_some((row.get::<_, i64>("item_id"), row.get::<_, i32>("flag_id"), name))
            })
        }).expect("rookie fit carries no default turret");
        lease.3 = Some(gun);
        let target: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,1,0,1,true,0,0,0) RETURNING item_id", &[&hull,&character]).await.unwrap().get("item_id");
        lease.4 = Some(target);
        // The provisioned hull and pilot leave the station for system space.
        db.execute("UPDATE game.characters SET station_id=NULL WHERE character_id=$1", &[&character]).await.unwrap();
        db.execute("UPDATE game.items SET location_id=1 WHERE item_id=$1", &[&ship]).await.unwrap();
        for attribute in [ATTR_CAPACITOR_CAPACITY, ATTR_CPU_OUTPUT, ATTR_POWER_OUTPUT] {
            dogma::set_override(&db, ship, attribute, 1_000_000.0).await.unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let mut client = Client::new(infinity_wire::conn::Conn::new(stream, address), Arc::new(crate::config::Config::default()), db.clone(), sde.clone(),
            crate::chat::Hub::new("localhost", db.clone(), world.clone()), world.clone(), crate::handshake::Login { user_id: account, user_name: "civilian-turret".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
        client.session.apply(vec![("charid", Value::Int(character)), ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(1))]);
        let (notify, mut notifications) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = notify.clone();
        world.register_pilot(character, notify);
        world.system(1).await.unwrap();
        let market = crate::services::market::lock().await;
        let row = crate::dao::items::space_ships::FreeShip { item_id: ship, type_id: hull, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let prepared = crate::services::ship::balls::piloted(&db, world.sde().clone(), &row, character, 0.0).await.ok().expect("prepare fitted ship");
        world.with(1, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        drop(market);
        crate::services::effects::tests::fixture::admit(&db, &world, 1, crate::services::effects::tests::fixture::ball(target, hull, character, None, 0.0)).await;
        call(&mut client, "AddTarget", &[Value::Int(target)]).await.ok().expect("target request");
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            while !combat::targeting_charges::targets_of(character).contains(&target) { assert!(world.after(1, 200).await); }
        }).await.expect("target lock");
        assert_eq!(loaded(&db, ship, flag).await.unwrap(), None, "provisioning loaded a charge");
        call(&mut client, "Activate", &[Value::Int(gun), Value::str(&effect_name), Value::Int(target), Value::Int(2)]).await.expect("activate chargeless turret");
        let mut hits = 0;
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            while hits < 2 {
                let (name, args) = notifications.recv().await.expect("weapon notifications");
                if name == "OnModuleAttributeChanges" { assert_eq!(quantity_changes(&args, ship, flag), 0, "chargeless cycle changed charge quantity"); }
                assert!(!matches!(name.as_str(), "OnChargeBeingLoadedToModule" | "OnGodmaPrimeItem"), "chargeless cycle reloaded");
                if name == "OnDamageMessage" {
                    if let Some(payload) = args.first().filter(|payload| payload.get("target") == Some(&Value::Int(target))) {
                        let Some(Value::Float(amount)) = payload.get("damage") else { panic!("damage amount") };
                        assert!(*amount > 0.0);
                        hits += 1;
                    }
                }
            }
        }).await.expect("two damaging cycles");
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            while state().active.contains_key(&gun) { assert!(world.after(1, 100).await); }
        }).await.expect("finite activation did not retire");
        while let Ok((name, args)) = notifications.try_recv() {
            if name == "OnModuleAttributeChanges" { assert_eq!(quantity_changes(&args, ship, flag), 0, "chargeless cycle changed charge quantity"); }
            assert!(!matches!(name.as_str(), "OnChargeBeingLoadedToModule" | "OnGodmaPrimeItem"), "chargeless cycle reloaded");
        }
        assert_eq!(loaded(&db, ship, flag).await.unwrap(), None, "chargeless cycles created a charge row");
        assert!(db.query("SELECT 1 FROM game.module_charges WHERE ship_id=$1", &[&ship]).await.unwrap().is_empty(), "chargeless cycles wrote charge rows");
        assert!(!state().reloading.contains_key(&gun), "chargeless cycles reloaded");
        combat::reset(&client);
    }).await;
    let (account, character, ship, gun, target) = lease;
    if let Some(gun) = gun {
        let mut st = state();
        if let Some(active) = st.active.remove(&gun) { active.stop.store(true, Ordering::Relaxed); }
        st.activation_generation.remove(&gun);
    }
    if let (Some(character), Some(ship)) = (character, ship) { combat::reset_pilot(&world, character, Some((1, ship))); }
    if let Some(target) = target { combat::forget_gm_spawn(&world, target); }
    if let Some(ship) = ship { world.remove_ball(1, ship).await; }
    if let Some(target) = target { world.remove_ball(1, target).await; }
    if let Some(character) = character { world.unregister_pilot_generation(character, None); }
    world.flush_positions().await.unwrap();
    if let Some(ship) = ship { combat::forget_ship(ship); }
    if let Some(target) = target { combat::forget_ship(target); }
    if let (Some(account), Some(character)) = (account, character) {
        let items = [ship, target];
        db.transaction(move |tx, _: &mut ()| Box::pin(async move {
            let items: Vec<i64> = items.into_iter().flatten().collect();
            crate::services::tests::fixture::cleanup_progress(tx, &[account, character], &[character]).await?;
            let kills: Vec<i64> = tx.query("SELECT kill_id FROM game.killmails WHERE victim_item_id = ANY($1)", &[&items]).await?.iter().map(|row| row.get("kill_id")).collect();
            crate::services::tests::fixture::cleanup_killmail_rewards(tx, &kills).await?;
            Ok::<_, crate::db::Error>(())
        }), |(), ()| async move { Ok(()) }).await.unwrap();
        if let Some(ship) = ship {
            db.execute("DELETE FROM game.weapon_banks WHERE ship_id=$1", &[&ship]).await.unwrap();
            db.execute("DELETE FROM game.module_charges WHERE ship_id=$1", &[&ship]).await.unwrap();
        }
        db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.skills WHERE character_id=$1", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    }
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
