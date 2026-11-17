//! Permanent regression: a real fitted-module active transition publishes the
//! changed effective attributes to the pilot on `OnModuleAttributeChanges`.
use super::*;
use crate::client::Client;
use crate::services::{Call, combat::{self, targeting_charges::loaded}, effects::tests::fixture, identity::Identity, slash, tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation}};
use crate::dao::items::{combat as item_combat, dogma as item_dogma};
use infinity_core::consts::{FLAG_CARGO, FLAG_HI_SLOT_FIRST, FLAG_MED_SLOT_FIRST, GM_ROLES,
    dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_CPU_OUTPUT, ATTR_FALLOFF, ATTR_MAX_RANGE, ATTR_POWER_OUTPUT, ATTR_RECHARGE_RATE, ATTR_SCAN_RESOLUTION, ATTR_SHIELD_CAPACITY, ATTR_TURRET_SLOTS_LEFT,
    ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, ATTR_TRACKING_SPEED}};
/// Screenshot fit: Tracking Computer, tracking script, six railguns, Javelin XL, hull.
const TC: i32 = 15284; const SCRIPT: i32 = 29001; const GUN: i32 = 41142; const AMMO: i32 = 41326; const HULL: i32 = 42126; const HARDENER: i32 = 2281;
/// SDE gunneryMaxRangeFalloffTrackingSpeedBonus and modifyActiveShieldResonancePostPercent; client dogma.const.attributeMaxGroupActive.
const FX_TC: i32 = 4559; const FX_HARDENER: i32 = 5230; const MAX_GROUP: i32 = 763;
const RESONANCES: [i32; 4] = [ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE];
const TURRET_ATTRS: [i32; 3] = [ATTR_MAX_RANGE, ATTR_FALLOFF, ATTR_TRACKING_SPEED];

/// `(item, attribute, old, new)` rows this event window published to `owner`.
fn macs(ev: &[(String, Vec<Value>)], owner: i64) -> Vec<(i64, i64, f64, f64)> {
    ev.iter().filter(|(n, _)| n == "OnModuleAttributeChanges").flat_map(|(_, a)| a.first().and_then(Value::items).unwrap_or(&[]).iter()
        .filter_map(move |r| { let f = r.items()?;
            if f.first() != Some(&Value::str("OnModuleAttributeChange")) || f.get(1) != Some(&Value::Int(owner)) { return None; }
            Some((f.get(2)?.as_int()?, f.get(3)?.as_int()?, f.get(6)?.as_float()?, f.get(5)?.as_float()?)) })).collect()
}
/// The (item, effect, start) `OnGodmaShipEffect` precedes the first MAC event.
fn ordered(ev: &[(String, Vec<Value>)], item: i64, effect: i64, start: i64) -> bool {
    let g = ev.iter().position(|(n, a)| n == "OnGodmaShipEffect" && a.first() == Some(&Value::Int(item)) && a.get(1).and_then(Value::as_int) == Some(effect) && a.get(3).and_then(Value::as_int) == Some(start));
    g.zip(ev.iter().position(|(n, _)| n == "OnModuleAttributeChanges")).is_some_and(|(g, m)| g < m)
}
/// A published turret type's turret effect name.
fn turret_fx(sde: &Sde, t: i32) -> String {
    let id = sde.types.get(t).expect("turret type").effects().iter().map(|e| e.effect_id)
        .find(|&id| registry::resolve(sde, id).is_some_and(|r| r.kind == EffectKind::Turret)).expect("turret effect");
    sde.dogma.effect(id).and_then(|e| e.name.clone()).expect("effect name")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn active_transitions_publish_effective_attribute_rows() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let (station, system) = sde.map.stations().find(|s| s.solar_system_id.is_some()
        && sde.map.celestial(s.station_id).is_some_and(|c| c.x.is_some() && c.y.is_some() && c.z.is_some())).map(|s| (s.station_id, s.solar_system_id.unwrap())).expect("station with celestial");
    for (t, e) in [(TC, FX_TC), (HARDENER, FX_HARDENER)] { assert!(sde.types.get(t).expect("fixture type").effects().iter().any(|x| x.effect_id == e), "fixture type missing effect {e}"); }
    let fx_name = |e: i32| sde.dogma.effect(e).and_then(|r| r.name.clone()).expect("effect name");
    let mut f = create_ship(&db, HULL, ShipLocation::Docked { station: 1 }, &[ShipItemSpec { type_id: TC, flag_id: FLAG_MED_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = f.module();
    let guns: Vec<i64> = db.query("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) SELECT $1,$2,$3,$4+g,1,true,true FROM generate_series(0,5) g RETURNING item_id", &[&GUN, &f.character, &f.ship, &FLAG_HI_SLOT_FIRST]).await.unwrap().iter().map(|r| r.get("item_id")).collect();
    let one = |t: i32, flag: i32, single: bool| { let (db, character, ship) = (&db, f.character, f.ship); async move { db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,$5,$6) RETURNING item_id", &[&t, &character, &ship, &flag, &single, &true]).await.unwrap().get("item_id") } };
    let (hardener, script, target) = (one(HARDENER, FLAG_MED_SLOT_FIRST + 1, true).await, one(SCRIPT, FLAG_CARGO, false).await,
        db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,1,0,1,true,0,0,0) RETURNING item_id", &[&HULL, &f.character]).await.unwrap().get("item_id"));
    f.items.extend(guns.iter().copied().chain([hardener, script, target]));
    let (hull_radius, clearance) = (f64::from(crate::services::ship::balls::radius(&sde, HULL).expect("fixture hull radius")), 2_000.0);
    let source_x = sde.map.celestials(system).map(|c| c.x.expect("celestial x") + f64::from(sde.celestial_radius(c))).reduce(f64::max).expect("system celestials") + hull_radius + clearance;
    let target_x = source_x + 2.0 * hull_radius + clearance;
    db.execute("UPDATE game.items SET location_id=$2,x=CASE WHEN item_id=$3 THEN $4::double precision ELSE $5::double precision END,y=0,z=0 WHERE item_id = ANY($1)", &[&&[f.ship, target][..], &system, &f.ship, &source_x, &target_x]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=$2, solar_system_id=$3, station_id=NULL WHERE character_id=$1", &[&f.character, &f.ship, &system]).await.unwrap();
    let mag = |t: i32, c: i32| (sde.types.get(t).and_then(|d| d.capacity.or_else(|| d.attribute(infinity_core::consts::dogma::ATTR_CAPACITY))).unwrap_or(0.0)
        / sde.types.get(c).and_then(|d| d.volume).unwrap_or(1.0)).floor().max(1.0) as i64;
    db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) SELECT $1,$2+g,$3,$4 FROM generate_series(0,5) g", &[&f.ship, &FLAG_HI_SLOT_FIRST, &AMMO, &mag(GUN, AMMO)]).await.unwrap();
    let mut skills: std::collections::BTreeMap<i32, i32> = std::collections::BTreeMap::new();
    for t in [GUN, TC, HARDENER] { for (s, l) in crate::services::skills::requirements::requirements(&sde, t) { skills.entry(s).and_modify(|v| *v = (*v).max(l)).or_insert(l); }
        for a in infinity_core::consts::dogma::ATTR_REQUIRED_SKILLS { if let Some(s) = sde.types.get(t).and_then(|d| d.attribute(a)).filter(|v| *v > 0.0) { skills.entry(s as i32).and_modify(|v| *v = (*v).max(5)).or_insert(5); } } }
    for (s, l) in skills { let rank = sde.types.get(s).and_then(|t| t.fit_attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT)).expect("skill rank");
        db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,$4)", &[&f.character, &s, &l, &crate::services::skills::formulas::sp_for_level(rank, i64::from(l))]).await.unwrap(); }
    for (i, a, v) in [(f.ship, ATTR_CAPACITOR_CAPACITY, 1e9), (f.ship, ATTR_RECHARGE_RATE, 1e6), (f.ship, ATTR_SCAN_RESOLUTION, 9e3), (f.ship, ATTR_CPU_OUTPUT, 1e9), (f.ship, ATTR_POWER_OUTPUT, 1e9), (f.ship, ATTR_TURRET_SLOTS_LEFT, 7.0)] { item_dogma::set_override(&db, i, a, v).await.unwrap(); }
    for g in &guns { item_dogma::set_override(&db, *g, MAX_GROUP, 8.0).await.unwrap(); }
    for g in &guns[1..] { item_combat::move_bank(&db, f.ship, guns[0], *g).await.unwrap(); }
    item_dogma::set_override(&db, target, ATTR_SHIELD_CAPACITY, 1e9).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), f.character).await;
    client.session.apply(vec![("userid", Value::Int(f.account)), ("role", Value::Int(GM_ROLES)), ("corpid", Value::Int(1)), ("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
    client.login.user_id = f.account; client.login.role = GM_ROLES;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = tx.clone(); client.world.register_pilot(f.character, tx);
    client.world.set_online_session(f.character, infinity_core::session::OnlineSession::from_session(&client.session));
    fixture::admit(&db, &client.world, system, fixture::ball(f.ship, HULL, f.character, Some(f.character), source_x)).await;
    fixture::admit(&db, &client.world, system, fixture::ball(target, HULL, f.character, None, target_x)).await;
    let world = client.world.clone();
    let result = crate::tests::catch_scenario(async {
        let drain = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>| std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
        let act = |m: i64, n: &str, t: Value| vec![Value::Int(m), Value::str(n), t, Value::Int(100)];
        let dec = |m: i64, n: &str| vec![Value::Int(m), Value::str(n)];
        let rows = |ev: &[(String, Vec<Value>)]| macs(ev, f.character);
        let retired = |m: i64| { let w = &world; tokio::time::timeout(std::time::Duration::from_secs(60), async move { while state().active.contains_key(&m) { assert!(w.after(system, 200).await); } }) };
        assert!(world.with(system, move |s| {
            [(f.ship, source_x), (target, target_x)].into_iter().all(|(id, x)| {
                let Some(b) = s.ball(id) else { return false }; let pos = b.position_at(s.now_ms());
                crate::space::dist(pos, [x, 0.0, 0.0]) < 1.0 && s.balls().all(|other| other.id == id
                    || crate::space::dist(pos, other.position_at(s.now_ms())) > f64::from(b.radius) + f64::from(other.radius))
            })
        }).await.expect("fixture geometry"), "fixture positions must match DB and not overlap");
        fixture::dogma_call(&mut client, "AddTarget", &[Value::Int(target)]).await.ok().expect("target request");
        tokio::time::timeout(std::time::Duration::from_secs(60), async { while !combat::targeting_charges::targets_of(f.character).contains(&target) { assert!(world.after(system, 200).await); } }).await.expect("target lock");
        fixture::module_online(&client, f.ship, hardener, false).await.ok().expect("offline hardener");
        fixture::module_online(&client, f.ship, hardener, true).await.ok().expect("online hardener");
        assert!(world.after(system, 300).await);
        let _ = drain(&mut rx);
        fixture::dogma_call(&mut client, "Activate", &act(module_id, &fx_name(FX_TC), Value::None)).await.ok().expect("activate TC");
        assert!(world.after(system, 300).await);
        let on = drain(&mut rx); let on_rows = rows(&on);
        let baseline = |g: i64, a: i32| on_rows.iter().find(|r| r.0 == g && r.1 == i64::from(a)).expect("missing TC start row").2;
        for g in &guns { for a in TURRET_ATTRS {
            let hits: Vec<_> = on_rows.iter().filter(|r| r.0 == *g && r.1 == i64::from(a)).collect();
            assert!(hits.len() == 1 && hits[0].3 > hits[0].2, "TC start row for gun {g} attr {a}: {hits:?}"); } }
        assert!(ordered(&on, module_id, i64::from(FX_TC), 1), "start GSF must precede the delta");
        fixture::dogma_call(&mut client, "Activate", &act(module_id, &fx_name(FX_TC), Value::None)).await.ok().expect("repeat activate");
        assert!(world.after(system, 200).await);
        assert!(rows(&drain(&mut rx)).is_empty(), "same-state activate republished rows");
        fixture::dogma_call(&mut client, "Activate", &act(guns[0], &turret_fx(&sde, GUN), Value::Int(target))).await.ok().expect("activate bank");
        let mut cycling: Vec<(String, Vec<Value>)> = Vec::new();
        // Each member still consumes one charge per volley; those quantity rows
        // bound the round inside which one grouped volley publishes one report.
        let consumed = |ev: &[(String, Vec<Value>)]| ev.iter().filter(|(n, _)| n == "OnModuleAttributeChanges")
            .flat_map(|(_, a)| a.first().and_then(Value::items).unwrap_or(&[]).iter())
            .filter(|r| r.items().is_some_and(|row| row.get(3).and_then(Value::as_int) == Some(i64::from(infinity_core::consts::dogma::ATTR_QUANTITY))
                && matches!(row.get(2), Some(Value::Tuple(key)) if key.first().and_then(Value::as_int) == Some(f.ship)
                    && key.get(1).and_then(Value::as_int).is_some_and(|flag| (i64::from(FLAG_HI_SLOT_FIRST)..i64::from(FLAG_HI_SLOT_FIRST) + guns.len() as i64).contains(&flag)))))
            .count();
        let reports = |ev: &[(String, Vec<Value>)]| ev.iter()
            .filter(|(n, a)| n == "OnDamageMessage" && a.first().is_some_and(|p| p.get("target") == Some(&Value::Int(target)))).count();
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            while consumed(&cycling) < 2 * guns.len() || reports(&cycling) < 2 {
                cycling.extend(drain(&mut rx)); assert!(world.after(system, 100).await); } }).await.expect("bank volleys");
        cycling.extend(drain(&mut rx));
        assert_eq!(reports(&cycling), 2, "one report per banked volley");
        assert!(cycling.iter().all(|(n, a)| n != "OnDamageMessage"
            || !a.first().is_some_and(|p| p.get("target") == Some(&Value::Int(target)))
            || a[0].get("isBanked") == Some(&Value::Bool(true))), "banked volley reported unbanked");
        assert!(!rows(&cycling).iter().any(|r| guns.contains(&r.0) && TURRET_ATTRS.contains(&(r.1 as i32))), "repeat cycles republished attributes");
        fixture::dogma_call(&mut client, "Deactivate", &dec(module_id, &fx_name(FX_TC))).await.ok().expect("deactivate TC");
        retired(module_id).await.expect("TC retired");
        let off = drain(&mut rx); let off_rows = rows(&off);
        for g in &guns { for a in TURRET_ATTRS {
            let hits: Vec<_> = off_rows.iter().filter(|r| r.0 == *g && r.1 == i64::from(a)).collect();
            assert!(hits.len() == 1 && hits[0].3 < hits[0].2, "TC stop row for gun {g} attr {a}: {hits:?}"); } }
        assert!(ordered(&off, module_id, i64::from(FX_TC), 0), "stop GSF must precede the delta");
        assert_eq!(fixture::fitted_attr(&mut client, guns[0], ATTR_TRACKING_SPEED).await, baseline(guns[0], ATTR_TRACKING_SPEED), "stopped ShipGetInfo baseline");
        // The 29001 script zeroes range and falloff: only tracking changes.
        combat::targeting_charges::load_ammo_core(&mut client, f.ship, vec![module_id], vec![script], Some(f.ship)).await.ok().expect("load script");
        tokio::time::timeout(std::time::Duration::from_secs(60), async { while loaded(&db, f.ship, FLAG_MED_SLOT_FIRST).await.ok().flatten() != Some((SCRIPT, 1)) { assert!(world.after(system, 200).await); } }).await.expect("script loaded");
        fixture::dogma_call(&mut client, "Activate", &act(module_id, &fx_name(FX_TC), Value::None)).await.ok().expect("activate scripted TC");
        assert!(world.after(system, 300).await);
        let sc_rows = rows(&drain(&mut rx));
        for g in &guns {
            let hits: Vec<_> = sc_rows.iter().filter(|r| r.0 == *g && r.1 == i64::from(ATTR_TRACKING_SPEED)).collect();
            assert_eq!(hits.len(), 1); assert!(hits[0].3 > hits[0].2, "scripted TC did not raise tracking on {g}");
            for a in [ATTR_MAX_RANGE, ATTR_FALLOFF] { assert!(!sc_rows.iter().any(|r| r.0 == *g && r.1 == i64::from(a)), "scripted TC still moved {a}"); } }
        assert_eq!(fixture::fitted_attr(&mut client, guns[0], ATTR_TRACKING_SPEED).await, sc_rows.iter().find(|r| r.0 == guns[0] && r.1 == i64::from(ATTR_TRACKING_SPEED)).unwrap().3, "ShipGetInfo prime disagrees");
        // The 5230 hardener moves the ship's four shield resonances.
        fixture::dogma_call(&mut client, "Activate", &act(hardener, &fx_name(FX_HARDENER), Value::None)).await.ok().expect("activate hardener");
        assert!(world.after(system, 300).await);
        let hd_rows = rows(&drain(&mut rx));
        for a in RESONANCES { let hits: Vec<_> = hd_rows.iter().filter(|r| r.0 == f.ship && r.1 == i64::from(a)).collect();
            assert_eq!(hits.len(), 1); assert!(hits[0].3 < hits[0].2, "hardener did not lower resonance {a}"); }
        // The live scanResolution override is the positive channel control.
        for line in [format!("/dogma ship {ATTR_SCAN_RESOLUTION} 4000"), format!("/dogma ship {ATTR_CPU_OUTPUT} 1000001000")] {
            let c = Call { service: "slash", method: "SlashCmd", args: &[Value::str(&line)], kwargs: &Value::Dict(vec![]), bound: false, bind_params: None };
            slash::dispatch(&mut client, &c).await.unwrap().unwrap(); }
        assert!(world.after(system, 200).await);
        assert_eq!(rows(&drain(&mut rx)).iter().filter(|r| r.0 == f.ship && r.1 == i64::from(ATTR_SCAN_RESOLUTION)).count(), 1, "scanResolution positive control");
        // Real dock and undock resurrect no activation; a fresh one publishes again.
        for (m, n) in [(module_id, fx_name(FX_TC)), (hardener, fx_name(FX_HARDENER)), (guns[0], turret_fx(&sde, GUN))] { fixture::dogma_call(&mut client, "Deactivate", &dec(m, &n)).await.ok().expect("deactivate"); }
        tokio::time::timeout(std::time::Duration::from_secs(60), async { while !state().active.is_empty() { assert!(world.after(system, 200).await); } }).await.expect("retirements");
        let end_rows = rows(&drain(&mut rx));
        for a in RESONANCES { let hits: Vec<_> = end_rows.iter().filter(|r| r.0 == f.ship && r.1 == i64::from(a)).collect();
            assert_eq!(hits.len(), 1); assert!(hits[0].3 > hits[0].2, "hardener stop did not restore resonance {a}"); }
        let (docked, in_space) = (Identity { char_id: f.character, ship_id: Some(f.ship), corp_id: 1, station_id: Some(station), system_id: Some(system) },
            Identity { char_id: f.character, ship_id: Some(f.ship), corp_id: 1, station_id: None, system_id: Some(system) });
        let (sent, received) = tokio::sync::oneshot::channel();
        world.with(system, move |s| { let p = s.ball(station).expect("station ball").position_at(s.now_ms());
            let off = f64::from(s.ball(station).unwrap().radius) + f64::from(s.ball(f.ship).unwrap().radius) + 500.0;
            s.command(f.ship, move |b, _, _| { b.pos = [p[0] + off, p[1], p[2]]; b.vel = [0.0; 3]; b.stop(); let _ = sent.send(()); Vec::new() }); }).await.expect("station approach");
        received.await.expect("approach action");
        let change = crate::services::ship::dock(&in_space, &db, &world, Some(system), station, None::<&mut Client>).await.expect("dock");
        client.change_session(change.updates);
        let undocked = crate::services::ship::ship::undock(&docked, &db, &world, &client.sde, None, None).await.expect("undock");
        client.change_session(undocked.change.updates); drop(undocked.economy);
        let _ = drain(&mut rx);
        assert_eq!(fixture::fitted_attr(&mut client, guns[0], ATTR_TRACKING_SPEED).await, baseline(guns[0], ATTR_TRACKING_SPEED), "undock resurrected the bonus");
        fixture::dogma_call(&mut client, "Activate", &act(module_id, &fx_name(FX_TC), Value::None)).await.ok().expect("reactivate TC");
        assert!(world.after(system, 300).await);
        let re_rows = rows(&drain(&mut rx));
        let hits: Vec<_> = re_rows.iter().filter(|r| r.0 == guns[0] && r.1 == i64::from(ATTR_TRACKING_SPEED)).collect();
        assert_eq!(hits.len(), 1); assert!(hits[0].3 > hits[0].2, "post-undock activation did not publish");
    }).await;
    combat::reset(&client);
    { let mut st = state(); for m in guns.iter().chain([module_id, hardener].iter()) { if let Some(a) = st.active.remove(m) { a.stop.store(true, Ordering::Relaxed); } st.activation_generation.remove(m); st.reloading.remove(m); } }
    world.remove_ball(system, f.ship).await; world.remove_ball(system, target).await;
    world.unregister_pilot_generation(f.character, None); world.flush_positions().await.unwrap();
    combat::forget_ship(f.ship); combat::forget_gm_spawn(&world, target);
    cleanup_ship(&db, None, f).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
