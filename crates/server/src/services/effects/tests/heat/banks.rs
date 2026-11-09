//! Persisted weapon-bank burnout through real activation and heat injury.
use super::*;
use infinity_core::{consts::{dogma::ATTR_MAX_LOCKED_TARGETS, dogma::ATTR_MAX_TARGET_RANGE, dogma::ATTR_SCAN_RESOLUTION, CATEGORY_CHARGE, FLAG_HI_SLOT_FIRST, GROUPABLE_GROUP_IDS}};
use crate::{dao::items::{charges, combat as item_combat}, services::{combat::targeting_charges::{add_target_ctx, clear_locks_on, loaded, targets_of}, effects::EffectKind}};

/// SDE high-slot heat capacity and attenuation attributes.
const HIGH_CAPACITY: i32 = 1178;
const HIGH_ATTENUATION: i32 = 1259;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn slave_overload_burns_one_weapon_and_stops_entire_bank() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(HIGH_CAPACITY).is_some_and(|v| v > 0.0)).expect("heat-capable hull").type_id;
    let size = sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
    let (gun, ammo, overload, effect_name, duration) = sde.types.iter()
        .filter(|m| m.published == Some(true) && m.category_id == Some(CATEGORY_MODULE)
            && m.group_id.is_some_and(|g| GROUPABLE_GROUP_IDS.contains(&i64::from(g))))
        .find_map(|m| {
            let overload = m.effects().iter().find(|e| sde.dogma.effect(e.effect_id).is_some_and(|r| r.effect_category == Some(5)))?.effect_id;
            let (effect, name) = m.effects().iter().find_map(|e| {
                let effect = registry::resolve(&sde, e.effect_id)?;
                let name = sde.dogma.effect(e.effect_id)?.name.as_ref()?;
                (effect.kind == EffectKind::Turret && effect.guid.as_deref().is_some_and(|v| !v.is_empty())
                    && sde.dogma.effects_by_name(name).count() == 1).then_some((effect, name.clone()))
            })?;
            let groups: Vec<_> = ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"].iter()
                .flat_map(|name| sde.dogma.attributes_by_name(name)).filter_map(|a| m.attribute(a.attribute_id)).collect();
            let module_size = size.and_then(|id| m.attribute(id));
            let capacity = m.capacity.or_else(|| m.attribute(infinity_core::consts::dogma::ATTR_CAPACITY))?;
            let ammo = sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE)
                && c.group_id.is_some_and(|g| groups.contains(&f64::from(g)))
                && (module_size.is_none_or(|v| v <= 0.0) || size.and_then(|id| c.attribute(id)) == module_size)
                && c.volume.is_some_and(|v| v > 0.0 && capacity >= 3.0 * v))?;
            Some((m.type_id, ammo.type_id, overload, name, effect.attribute(EffectAttribute::Duration)?))
        }).expect("groupable overloadable turret and compatible ammunition");
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, gun, FLAG_HI_SLOT_FIRST).await.unwrap();
    let slave = fixture::item(&db, gun, f.character, f.ship, FLAG_HI_SLOT_FIRST + 1, 1, true, true).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let w = world.clone();
    let d = db.clone();
    let target = -f.ship;
    let result = tokio::spawn(async move {
        fixture::position(&d, f.ship, SYSTEM, [0.0; 3]).await.unwrap();
        seed_module_prerequisites(&d, &sde, f.character, gun).await;
        for (item, attr, value) in [
            (f.ship, ATTR_HI_SLOTS, 2.0), (f.ship, ATTR_MED_SLOTS, 0.0), (f.ship, ATTR_LOW_SLOTS, 0.0), (f.ship, ATTR_RIG_SLOTS, 0.0),
            (f.ship, ATTR_CPU_OUTPUT, 1_000_000.0), (f.ship, ATTR_POWER_OUTPUT, 1_000_000.0), (f.ship, ATTR_CAPACITOR_CAPACITY, 1_000_000.0),
            (f.ship, ATTR_MAX_TARGET_RANGE, 100_000.0), (f.ship, ATTR_MAX_LOCKED_TARGETS, 1.0), (f.ship, ATTR_SCAN_RESOLUTION, 1_000_000.0),
            (f.ship, HEAT_GENERATION, 1000.0), (f.ship, HIGH_ATTENUATION, 0.0),
            (f.module, ATTR_STRUCTURE_HP, 1.0), (slave, ATTR_STRUCTURE_HP, 1.0),
            (f.module, HEAT_DAMAGE, 10.0), (slave, HEAT_DAMAGE, 0.0),
        ] { dogma::set_override(&d, item, attr, value).await.unwrap(); }
        for id in [f.module, slave] {
            for (attr, value) in [(ATTR_CPU, 0.0), (ATTR_POWER, 0.0), (ATTR_CAPACITOR_NEED, 0.0),
                (REQUIRED_THERMODYNAMICS, 0.0), (HEAT_ABSORPTION, 1000.0), (duration, 2000.0)] {
                dogma::set_override(&d, id, attr, value).await.unwrap();
            }
            for attr in ATTR_REQUIRED_SKILL_LEVELS { dogma::set_override(&d, id, attr, 0.0).await.unwrap(); }
        }
        for flag in [FLAG_HI_SLOT_FIRST, FLAG_HI_SLOT_FIRST + 1] {
            d.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,$3,3)", &[&f.ship, &flag, &ammo]).await.unwrap();
        }
        item_combat::move_bank(&d, f.ship, f.module, slave).await.unwrap();
        assert_eq!(charges::banks(&d, f.ship).await.unwrap().len(), 1);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, addr) = listener.accept().await.unwrap();
        let login = Login { user_id: f.account, user_name: f.account.to_string(), role: 0, client_id: f.character, session_id: f.character, language_id: "EN".into() };
        let mut c = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), d.clone(), sde.clone(), crate::chat::Hub::new("localhost", d.clone(), w.clone()), w.clone(), login);
        c.session.apply(vec![("charid", Value::Int(f.character)), ("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(SYSTEM))]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = tx.clone();
        w.register_pilot(f.character, tx.clone());
        fixture::admit(&d, &w, SYSTEM, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
        fixture::admit_npc(&w, SYSTEM, fixture::ball(target, hull, f.character, None, 1000.0)).await;
        let cx = combat::Ctx { db: d.clone(), world: w.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: SYSTEM };
        add_target_ctx(&cx, target, false).await.ok().expect("target request");
        tokio::time::timeout(Duration::from_secs(20), async {
            while !targets_of(f.character).contains(&target) { assert!(w.after(SYSTEM, 100).await); }
        }).await.expect("target lock");
        let args = [Value::Int(slave), Value::Int(i64::from(overload))];
        call(&mut c, "dogmaIM", "Overload", &args, None).await.ok().expect("slave overload");
        let primed = heat::overloaded_modules();
        assert!(primed.contains(&f.module) && primed.contains(&slave));
        let activation = [Value::Int(slave), Value::str(&effect_name), Value::Int(target), Value::Int(10)];
        call(&mut c, "dogmaIM", "Activate", &activation, None).await.ok().expect("activate bank");
        tokio::time::timeout(Duration::from_secs(10), async {
            while !heat::ship_state(&w, SYSTEM, f.ship).await.is_some_and(|r| r.iter().any(|r| r.incoming >= 2000.0)) { assert!(w.after(SYSTEM, 100).await); }
        }).await.expect("both heat sources connected");
        let stop = combat::state().active.get(&f.module).expect("master owns activation").stop.clone();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if charges::banks(&d, f.ship).await.unwrap().is_empty() && !heat::overloaded_modules().contains(&f.module)
                    && !heat::overloaded_modules().contains(&slave) && stop.load(std::sync::atomic::Ordering::Relaxed) { break; }
                assert!(w.after(SYSTEM, 100).await);
            }
        }).await.expect("burnout terminates bank");
        let fit = Fit::load(&d, sde, f.character, f.ship).await.ok().unwrap();
        assert_eq!(fit.value(Key::Module(f.module), ATTR_HULL_DAMAGE), fit.value(Key::Module(f.module), ATTR_STRUCTURE_HP));
        assert_eq!(fit.value(Key::Module(slave), ATTR_HULL_DAMAGE), 0.0);
        let online = dogma::online(&d, f.ship).await.unwrap();
        assert!(!online.iter().any(|m| m.item_id == f.module));
        assert!(online.iter().any(|m| m.item_id == slave), "slave unexpectedly offline; online ids={:?}", online.iter().map(|m| m.item_id).collect::<Vec<_>>());
        assert!(w.after(SYSTEM, 2500).await);
        for flag in [FLAG_HI_SLOT_FIRST, FLAG_HI_SLOT_FIRST + 1] { assert_eq!(loaded(&d, f.ship, flag).await.ok().unwrap(), Some((ammo, 2))); }
        let mut counts = [(0, 0); 2];
        while let Ok((name, payload)) = rx.try_recv() {
            if let Some(index) = [f.module, slave].iter().position(|id| payload.get(1).and_then(Value::as_int) == Some(*id)) {
                if name == "OnHeatAdded" { counts[index].0 += 1; }
                if name == "OnHeatRemoved" { counts[index].1 += 1; }
            }
        }
        assert_eq!(counts, [(1, 1), (1, 1)]);
    }).await;
    { let mut st = combat::state(); if let Some(active) = st.active.remove(&f.module) { active.stop.store(true, std::sync::atomic::Ordering::Relaxed); } st.activation_generation.remove(&f.module); }
    heat::clear_character(f.character);
    clear_locks_on(&world, SYSTEM, f.ship, f.character);
    for id in [f.ship, target] { world.remove_ball(SYSTEM, id).await; combat::forget_ship(id); }
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    drop(world);
    db.execute("DELETE FROM game.weapon_banks WHERE ship_id=$1", &[&f.ship]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}
