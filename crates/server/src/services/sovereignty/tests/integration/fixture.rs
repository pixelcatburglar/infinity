//! Real dedicated-database Hub fixture; all mutable rows are owned by recorded IDs.
use std::{collections::BTreeMap, sync::{Arc, Mutex}};
use infinity_core::{
    consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_CARGO, FLAG_HI_SLOT_FIRST, GROUP_SUN, KNOWN_SPACE_SYSTEM_START, ROLE_DIRECTOR, WORMHOLE_SYSTEM_START},
};
use crate::{
    dao::{items::space_ships::FreeShip, sovereignty::sources},
    db::Db,
    sde::Sde,
    services::{identity::Identity, sovereignty::{recovery, Resources}},
    space::World,
};

#[derive(Clone, Copy, Default)]
pub(super) struct Ids {
    pub account: i64,
    pub characters: [i64; 3],
    pub corps: [i64; 3],
    pub alliances: [i64; 2],
    pub systems: [i64; 2],
    pub ships: [i64; 3],
    pub modules: [i64; 3],
    pub cloak: i64,
    pub assistance: i64,
    pub fuel: [i64; 3],
    pub station: i64,
    pub gate: i64,
    pub destination_gate: i64,
    pub destination_system: i64,
    pub sources: [i64; 2],
}

#[derive(Default)]
pub(super) struct Recorded { pub ids: Ids, pub world: Option<Arc<World>> }

pub(super) struct Fixture { pub resources: Resources, pub ids: Ids, pilot_systems: Mutex<[i64; 3]> }

impl Fixture {
    pub async fn new(recorded: Arc<Mutex<Recorded>>) -> Self {
        let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        // Recovery intentionally visits all sovereignty rows, so refuse a shared domain.
        let empty: bool = db.query_one("SELECT NOT EXISTS (SELECT 1 FROM game.sov_systems) AND NOT EXISTS (SELECT 1 FROM game.sov_structures) AND NOT EXISTS (SELECT 1 FROM game.sov_capture_targets) AND NOT EXISTS (SELECT 1 FROM game.sov_link_sources) AND NOT EXISTS (SELECT 1 FROM game.sov_link_source_changes) AND NOT EXISTS (SELECT 1 FROM game.sov_alliance_policy) AND NOT EXISTS (SELECT 1 FROM game.sov_history) AS empty", &[]).await.unwrap().get("empty");
        assert!(empty, "integration recovery requires an empty dedicated sovereignty domain");
        let sde = Arc::new(Sde::load(&db).await.unwrap());
        let systems: Vec<_> = sde.map.systems().filter(|s| {
            (KNOWN_SPACE_SYSTEM_START..WORMHOLE_SYSTEM_START).contains(&s.solar_system_id)
                && s.faction_id.is_none() && s.security.is_some_and(|v| v.is_finite() && v <= 0.0 && v <= sde.sovereignty.hub().deploy.max_security_level)
                && s.constellation_id.is_some() && s.region_id.is_some()
                && sde.map.celestials(s.solar_system_id).filter(|c| c.group_id == Some(i64::from(GROUP_SUN)) && c.x.is_some() && c.y.is_some() && c.z.is_some()).count() == 1
        }).map(|s| s.solar_system_id).collect();
        let (system1, station) = systems.iter().find_map(|&system| {
            let station = sde.map.stations().find(|station| station.solar_system_id == Some(system)
                && station.owner_id.is_some() && station.type_id.is_some()
                && [station.x, station.y, station.z].iter().all(|v| v.is_some_and(f64::is_finite))
                && sde.map.celestial(station.station_id).is_some_and(|c| c.solar_system_id == Some(system) && c.type_id.is_some()
                    && [c.x, c.y, c.z].iter().all(|v| v.is_some_and(f64::is_finite))))?;
            Some((system, station.station_id))
        }).expect("eligible null-security system with an NPC station");
        let (system0, gate, destination_gate, destination_system) = systems.iter().filter(|&&system| system != system1).find_map(|&system| {
            let gate = sde.map.celestials(system).filter(|c| c.type_id.is_some() && [c.x, c.y, c.z].iter().all(|v| v.is_some_and(f64::is_finite)))
                .filter_map(|c| sde.map.stargate(c.item_id))
                .find(|gate| gate.solar_system_id == Some(system)
                    && gate.dest_solar_system_id.is_some_and(|id| id != system && sde.map.system(id).is_some())
                    && gate.dest_stargate_id.is_some_and(|id| sde.map.stargate(id).is_some()
                        && sde.map.celestial(id).is_some_and(|c| c.solar_system_id == gate.dest_solar_system_id
                            && c.type_id.is_some() && [c.x, c.y, c.z].iter().all(|v| v.is_some_and(f64::is_finite)))))?;
            Some((system, gate.stargate_id, gate.dest_stargate_id?, gate.dest_solar_system_id?))
        }).expect("distinct eligible null-security system with a connected stargate");
        let systems = [system0, system1];
        let candidates = entosis_candidates(&sde);
        let initial = candidates.iter().find(|c| !c.capital).unwrap_or_else(|| panic!(
            "SDE has no initial noncapital Entosis candidate: total={}, capital={}, capital_with_duration_modifier={}",
            candidates.len(), candidates.iter().filter(|c| c.capital).count(),
            candidates.iter().filter(|c| c.capital && c.duration_modifier).count()));
        let (ship_type, module_type, fuel_type, fuel_quantity) = (initial.ship, initial.module, initial.fuel, initial.quantity);
        let cloaks = cloak_candidates(&sde);
        let initial_cloak = *cloaks.first().expect("SDE high-slot cloak module");
        let assistance = assistance_candidates(&sde);
        let (initial_assistance, initial_assistance_flag) = *assistance.first().expect("SDE projected assistance module");
        let cloak: i64 = db.query_one("SELECT nextval('game.items_item_id_seq') AS cloak", &[]).await.unwrap().get("cloak");
        let assistance_id: i64 = db.query_one("SELECT nextval('game.items_item_id_seq') AS assistance", &[]).await.unwrap().get("assistance");
        let row = db.query_one("SELECT nextval('game.accounts_user_id_seq') AS account, nextval('game.characters_character_id_seq') AS c0, nextval('game.characters_character_id_seq') AS c1, nextval('game.characters_character_id_seq') AS c2, nextval('game.corporations_corporation_id_seq') AS p0, nextval('game.corporations_corporation_id_seq') AS p1, nextval('game.corporations_corporation_id_seq') AS p2, nextval('game.alliances_alliance_id_seq') AS a0, nextval('game.alliances_alliance_id_seq') AS a1, nextval('game.items_item_id_seq') AS s0, nextval('game.items_item_id_seq') AS s1, nextval('game.items_item_id_seq') AS s2, nextval('game.items_item_id_seq') AS h0, nextval('game.items_item_id_seq') AS h1, nextval('game.items_item_id_seq') AS m0, nextval('game.items_item_id_seq') AS m1, nextval('game.items_item_id_seq') AS m2, nextval('game.items_item_id_seq') AS f0, nextval('game.items_item_id_seq') AS f1, nextval('game.items_item_id_seq') AS f2", &[]).await.unwrap();
        let ids = Ids { account: row.get("account"), characters: [row.get("c0"), row.get("c1"), row.get("c2")], corps: [row.get("p0"), row.get("p1"), row.get("p2")], alliances: [row.get("a0"), row.get("a1")], systems, ships: [row.get("s0"), row.get("s1"), row.get("s2")], sources: [row.get("h0"), row.get("h1")], modules: [row.get("m0"), row.get("m1"), row.get("m2")], fuel: [row.get("f0"), row.get("f1"), row.get("f2")], cloak, assistance: assistance_id, station, gate, destination_gate, destination_system };
        recorded.lock().unwrap().ids = ids;
        let hub_type = sde.sovereignty.hub().type_id;
        let positions = [star_position(&sde, ids.systems[0]), star_position(&sde, ids.systems[1])];
        let locations = ids.systems.map(|id| {
            let system = sde.map.system(id).unwrap();
            (system.constellation_id, system.region_id)
        });
        db.transaction(|tx, _: &mut ()| Box::pin(async move {
            tx.execute("INSERT INTO game.accounts (user_id,user_name,password_hash,role) VALUES ($1,'hub-fixture-' || $1::bigint::text,$2,0)", &[&ids.account, &&b"fixture"[..]]).await?;
            // Match account registration: Fit and later gameplay use durable clone authority.
            tx.execute("INSERT INTO game.account_clone_grades (user_id,grade) VALUES ($1,$2)", &[&ids.account, &crate::services::subscriptions::rules::OMEGA]).await?;
            for i in 0..3 {
                let system_index = usize::from(i == 1);
                tx.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id,corp_roles,ship_id,station_id,solar_system_id,constellation_id,region_id) VALUES ($1,$2,'hub-pilot-' || $1::bigint::text,1,1,1,$3,$4,$5,NULL,$6,$7,$8)", &[&ids.characters[i], &ids.account, &ids.corps[i], &ROLE_DIRECTOR, &ids.ships[i], &ids.systems[system_index], &locations[system_index].0, &locations[system_index].1]).await?;
                tx.execute("INSERT INTO game.corporations (corporation_id,name,ticker,ceo_id,creator_id,station_id,created,alliance_id,alliance_joined) VALUES ($1,'hub-corp-' || $1::bigint::text,'H' || $1::bigint::text,$2,$2,1,1,$3,1)", &[&ids.corps[i], &ids.characters[i], &ids.alliances[usize::from(i == 2)]]).await?;
                let p = positions[system_index];
                tx.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,$3,$4,0,1,true,$5,$6,$7)", &[&ids.ships[i], &ship_type, &ids.characters[i], &ids.systems[system_index], &p[0], &p[1], &p[2]]).await?;
                tx.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,$5,1,true,true)", &[&ids.modules[i], &module_type, &ids.characters[i], &ids.ships[i], &FLAG_HI_SLOT_FIRST]).await?;
                tx.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,$6,false)", &[&ids.fuel[i], &fuel_type, &ids.characters[i], &ids.ships[i], &FLAG_CARGO, &i64::from(fuel_quantity)]).await?;
                sources::ensure(tx, ids.characters[i]).await?;
            }
            tx.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,$5,1,true,true)", &[&ids.cloak, &initial_cloak, &ids.characters[0], &ids.ships[0], &(FLAG_HI_SLOT_FIRST + 1)]).await?;
            tx.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,$5,1,true,true)", &[&ids.assistance, &initial_assistance, &ids.characters[0], &ids.ships[0], &initial_assistance_flag]).await?;
            for i in 0..2 {
                let corp = ids.corps[if i == 0 { 0 } else { 2 }];
                let character = ids.characters[if i == 0 { 0 } else { 2 }];
                tx.execute("INSERT INTO game.alliances (alliance_id,name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ($1,'hub-alliance-' || $1::bigint::text,'H' || $1::bigint::text,$2,$2,$3,1)", &[&ids.alliances[i], &corp, &character]).await?;
                let quantity: i64 = if i == 0 { 1 } else { 2 };
                tx.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,$6,false)", &[&ids.sources[i], &hub_type, &ids.characters[i], &ids.ships[i], &FLAG_CARGO, &quantity]).await?;
            }
            Ok::<_, crate::db::Error>(())
        }), |(), ()| async move { Ok(()) }).await.unwrap();
        let (module_type, baseline_duration) = provision_candidate(&db, &sde, ids, 0, &candidates, &cloaks, &assistance, None, false).await;
        let (_, capital_duration) = provision_candidate(&db, &sde, ids, 2, &candidates, &cloaks, &assistance, Some((module_type, baseline_duration)), false).await;
        let (_, baseline_duration) = provision_candidate(&db, &sde, ids, 0, &candidates, &cloaks, &assistance, Some((module_type, baseline_duration)), true).await;
        assert!(capital_duration > baseline_duration, "capital Entosis duration must exceed the final subcapital fit");
        provision_candidate(&db, &sde, ids, 1, &candidates, &cloaks, &assistance, Some((module_type, baseline_duration)), false).await;
        let world = World::new(db.clone(), sde.clone(), 16);
        recorded.lock().unwrap().world = Some(world.clone());
        let chat = crate::chat::Hub::new("test", db.clone(), world.clone());
        let mut config = crate::config::Config::default();
        config.lua.scripts_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lua").to_string_lossy().into_owned();
        crate::lua::start(&config, &db, &sde, &world, &chat).await.unwrap();
        let resources = Resources { db, sde, world };
        recovery::recover(&resources).await.unwrap();
        crate::space::sovereignty::open(&resources.world);
        let fixture = Self { resources, ids, pilot_systems: Mutex::new([ids.systems[0], ids.systems[1], ids.systems[0]]) };
        for (pilot, system) in [(0, 0), (1, 1), (2, 0)] {
            fixture.install_pilot(pilot, system, positions[system]).await;
        }
        fixture
    }

    pub fn identity(&self, index: usize) -> Identity {
        Identity { char_id: self.ids.characters[index], ship_id: Some(self.ids.ships[index]), corp_id: self.ids.corps[index], station_id: None, system_id: Some(self.pilot_systems.lock().unwrap()[index]) }
    }

    pub async fn install_pilot(&self, index: usize, system_index: usize, position: [f64; 3]) {
        let ids = self.ids;
        let system = ids.systems[system_index];
        let map = self.resources.sde.map.system(system).unwrap();
        for old in ids.systems { self.resources.world.remove_ball(old, ids.ships[index]).await; }
        self.resources.db.execute("UPDATE game.characters SET solar_system_id=$2,constellation_id=$3,region_id=$4,station_id=NULL,ship_id=$5 WHERE character_id=$1", &[&ids.characters[index], &system, &map.constellation_id, &map.region_id, &ids.ships[index]]).await.unwrap();
        self.resources.db.execute("UPDATE game.items SET location_id=$2,x=$3,y=$4,z=$5 WHERE item_id=$1 AND owner_id=$6", &[&ids.ships[index], &system, &position[0], &position[1], &position[2], &ids.characters[index]]).await.unwrap();
        let ty: i32 = self.resources.db.query_one("SELECT type_id FROM game.items WHERE item_id=$1", &[&ids.ships[index]]).await.unwrap().get("type_id");
        let row = FreeShip { item_id: ids.ships[index], type_id: ty, owner_id: ids.characters[index], corporation_id: ids.corps[index], alliance_id: ids.alliances[usize::from(index == 2)], pos: position };
        let prepared = crate::services::ship::balls::piloted(&self.resources.db, self.resources.sde.clone(), &row, ids.characters[index], 0.0).await.unwrap();
        self.resources.world.system(system).await.unwrap();
        self.resources.world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        self.pilot_systems.lock().unwrap()[index] = system;
    }
}

/// Sixteen paid cycles allow lifecycle scenarios to exercise repeated links.
const FIXTURE_FUEL_CYCLES: i32 = 16;

async fn provision_candidate(db: &Db, sde: &Arc<Sde>, ids: Ids, index: usize, candidates: &[Candidate], cloaks: &[i32], assistance: &[(i32, i32)], reference: Option<(i32, f64)>, check_projection: bool) -> (i32, f64) {
    use infinity_core::consts::dogma::*;
    use crate::services::{effects::{fitting, registry::{self, EffectAttribute}}, fit::{Fit, Key}};
    let effect = registry::resolve(sde, EFFECT_ENTOSIS_LINK).unwrap();
    let capital = index == 2;
    let can_cloak = sde.dogma.attributes_by_name("canCloak").next().expect("SDE canCloak attribute").attribute_id;
    let disallow_cloaking = sde.dogma.attributes_by_name("disallowCloaking").next().expect("SDE disallowCloaking attribute").attribute_id;
    let early_deactivation = sde.dogma.attributes_by_name("disallowEarlyDeactivation").next().expect("SDE disallowEarlyDeactivation attribute").attribute_id;
    let mut rejections = BTreeMap::<String, (usize, String)>::new();
    let mut eligible = 0_usize;
    for candidate in candidates.iter().filter(|candidate| candidate.capital == capital
        && candidate.hub_room >= candidate.hub_volume * match index { 0 => 1.0, 1 => 2.0, _ => 0.0 }
        && (!capital || candidate.duration_modifier)
        && reference.is_none_or(|(module, _)| candidate.module == module)
        && (index != 0 || candidates.iter().any(|other| other.capital && other.duration_modifier && other.module == candidate.module))) {
        eligible += 1;
        let (ship_type, module_type, fuel_type, quantity) = (candidate.ship, candidate.module, candidate.fuel, candidate.quantity);
        let options: Vec<Option<i32>> = if index == 0 { cloaks.iter().copied().map(Some).collect() } else { vec![None] };
        for cloak in options {
        let repairs: Vec<Option<(i32, i32)>> = if index == 0 { assistance.iter().copied().map(Some).collect() } else { vec![None] };
        'candidate_fit: for repair in repairs {
        macro_rules! reject {
            ($reason:expr) => {{
                let reason = $reason;
                let entry = rejections.entry(reason).or_insert_with(|| (0, format!("ship={} module={} cloak={cloak:?} assistance={repair:?}", candidate.ship, candidate.module)));
                entry.0 += 1;
                continue 'candidate_fit;
            }};
        }
        let mut types = vec![ship_type, module_type];
        types.extend(cloak);
        types.extend(repair.map(|(ty, _)| ty));
        let Some(skills) = fixture_skills(sde, &types) else { reject!("invalid SDE skill prerequisites".to_string()) };
        db.transaction(|tx, _: &mut ()| {
            let skills = skills.clone();
            Box::pin(async move {
                let existing = crate::dao::skills::levels::levels(tx, ids.characters[index]).await?;
                let skill_ids: Vec<_> = existing.iter().map(|row| row.type_id).collect();
                crate::dao::skills::training::remove(tx, ids.characters[index], &skill_ids).await?;
                for (skill, level, points) in skills {
                    crate::dao::skills::training::starting_skill(tx, ids.characters[index], skill, level, points).await?;
                }
                tx.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1 AND owner_id=$3", &[&ids.ships[index], &ship_type, &ids.characters[index]]).await?;
                tx.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1 AND owner_id=$3", &[&ids.modules[index], &module_type, &ids.characters[index]]).await?;
                tx.execute("UPDATE game.items SET type_id=$2,quantity=$3 WHERE item_id=$1 AND owner_id=$4", &[&ids.fuel[index], &fuel_type, &i64::from(quantity), &ids.characters[index]]).await?;
                if let Some(cloak) = cloak {
                    tx.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1 AND owner_id=$3", &[&ids.cloak, &cloak, &ids.characters[index]]).await?;
                }
                if let Some((repair, flag)) = repair {
                    tx.execute("UPDATE game.items SET type_id=$2,flag_id=$3 WHERE item_id=$1 AND owner_id=$4", &[&ids.assistance, &repair, &flag, &ids.characters[index]]).await?;
                }
                Ok::<_, crate::db::Error>(())
            })
        }, |(), ()| async move { Ok(()) }).await.unwrap();
        for (label, item) in [("Entosis", Some(ids.modules[index])), ("cloak", cloak.map(|_| ids.cloak)), ("assistance", repair.map(|_| ids.assistance))] {
            let Some(item) = item else { continue };
            for (stage, result) in [
                ("fit", fitting::validate_fit(db, sde.clone(), ids.characters[index], ids.ships[index], item).await),
                ("online", fitting::validate_online(db, sde.clone(), ids.characters[index], ids.ships[index], item).await),
            ] {
                if let Err(error) = result {
                    match error {
                        crate::services::CallError::Internal(error) => panic!("{label} {stage} validation failed internally: {error:#}"),
                        error => reject!(format!("{label} {stage}: {error:?}")),
                    }
                }
            }
        }
        let fit = Fit::load(db, sde.clone(), ids.characters[index], ids.ships[index]).await.expect("load Entosis fit");
        if skills.iter().any(|&(skill, level, _)| fit.value(Key::Skill(skill), ATTR_SKILL_LEVEL) < f64::from(level)) { reject!("effective skill level below staged requirement".to_string()); }
        if let Some((ty, _)) = repair {
            use crate::services::effects::{EffectKind, registry::EffectFlag};
            let usable = sde.types.get(ty).unwrap().effects().iter().filter_map(|row| registry::resolve(sde, row.effect_id))
                .filter(|effect| matches!(effect.kind, EffectKind::Projected { .. }) && effect.has(EffectFlag::Assistance))
                .any(|effect| effect.attribute(EffectAttribute::Duration).is_some_and(|id| {
                    let v = fit.value(Key::Module(ids.assistance), id);
                    v.is_finite() && v > 0.0
                }) && effect.attribute(EffectAttribute::Range).is_some_and(|id| {
                    let v = fit.value(Key::Module(ids.assistance), id); v.is_finite() && v > 0.0
                }));
            if !usable { reject!("assistance has no usable projected effect".to_string()); }
            if check_projection && !projection_changes_capital(db, sde, ids, ty, &fit).await { reject!("assistance does not change capital target attributes".to_string()); }
        }
        let module = Key::Module(ids.modules[index]);
        let duration = fit.value(module, effect.attribute(EffectAttribute::Duration).unwrap());
        let range = fit.value(module, effect.attribute(EffectAttribute::Range).unwrap());
        let fuel = fit.value(module, ATTR_CONSUMPTION_TYPE);
        let units = fit.value(module, ATTR_CONSUMPTION_QUANTITY);
        let need = fit.value(module, effect.attribute(EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED));
        let positive = [ATTR_MAX_TARGET_RANGE, ATTR_SCAN_RESOLUTION, ATTR_BASE_WARP_SPEED, ATTR_WARP_SPEED_MULTIPLIER];
        if !duration.is_finite() || duration <= 0.0 || !range.is_finite() || range < 0.0
            || fuel != f64::from(fuel_type) || !units.is_finite() || units <= 0.0 || units.ceil() * 3.0 > f64::from(quantity)
            || !need.is_finite() || need < 0.0 || fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY) < need * 3.0
            || (fit.value(Key::Ship, ATTR_IS_CAPITAL_SIZE) > 0.0) != capital
            || !positive.iter().all(|&attr| { let v = fit.value(Key::Ship, attr); v.is_finite() && v > 0.0 })
            || (index == 0 && (fit.value(Key::Ship, can_cloak) <= 0.0 || fit.value(Key::Ship, disallow_cloaking) > 0.0 || fit.value(module, early_deactivation) > 0.0))
            || (capital && reference.is_some_and(|(_, baseline)| duration <= baseline)) {
            reject!(format!("effective fit rejected: duration={duration} range={range} fuel={fuel}/{fuel_type} units={units}/{quantity} need={need} capacity={} capital={} ship_positive={:?} can_cloak={} disallow_cloaking={} early_deactivation={} reference={reference:?}",
                fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY), fit.value(Key::Ship, ATTR_IS_CAPITAL_SIZE),
                positive.map(|attr| (attr, fit.value(Key::Ship, attr))), fit.value(Key::Ship, can_cloak),
                fit.value(Key::Ship, disallow_cloaking), fit.value(module, early_deactivation)));
        }
        return (module_type, duration);
        }
        }
    }
    panic!("SDE contains no viable {} Entosis fit (pilot {index}, projection={check_projection}, reference={reference:?}); candidates total={} noncapital={} capital={} capital_with_duration_modifier={} eligible={eligible} cloaks={} assistance={}; rejection counts and example candidates: {rejections:#?}",
        if capital { "capital" } else { "noncapital" }, candidates.len(),
        candidates.iter().filter(|c| !c.capital).count(), candidates.iter().filter(|c| c.capital).count(),
        candidates.iter().filter(|c| c.capital && c.duration_modifier).count(), cloaks.len(), assistance.len());
}

async fn projection_changes_capital(db: &Db, sde: &Arc<Sde>, ids: Ids, ty: i32, source: &crate::services::fit::Fit) -> bool {
    use infinity_core::consts::dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_CAPACITOR_NEED, ATTR_CONSUMPTION_QUANTITY, ATTR_MAX_TARGET_RANGE, ATTR_SCAN_RESOLUTION};
    use crate::services::{effects::{EffectKind, projection::Projection, registry::{self, EffectAttribute, EffectFlag}}, fit::{Fit, Key}};
    let target = Fit::load(db, sde.clone(), ids.characters[2], ids.ships[2]).await.expect("load capital fit");
    for entry in sde.types.get(ty).unwrap().effects() {
        let Some(effect) = registry::resolve(sde, entry.effect_id) else { continue };
        if !matches!(effect.kind, EffectKind::Projected { .. }) || !effect.has(EffectFlag::Assistance) { continue; }
        if ![EffectAttribute::Duration, EffectAttribute::Range].iter().all(|&key| effect.attribute(key).is_some_and(|attr| {
            let value = source.value(Key::Module(ids.assistance), attr); value.is_finite() && value > 0.0
        })) { continue; }
        let key = Key::Module(ids.assistance);
        let duration = source.value(key, effect.attribute(EffectAttribute::Duration).unwrap());
        let window = std::time::Duration::from_secs(10).as_millis() as f64;
        let need = source.value(key, effect.attribute(EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED));
        let cycles = (window / duration).ceil().max(1.0);
        if !need.is_finite() || need < 0.0 || source.value(key, ATTR_CONSUMPTION_QUANTITY) != 0.0
            || source.value(Key::Ship, ATTR_CAPACITOR_CAPACITY) < need * cycles
            || (duration < window && effect.has(EffectFlag::DisallowAutoRepeat)) { continue; }
        let Some(row) = sde.dogma.effect(entry.effect_id) else { continue };
        let mut values = Vec::new();
        let mut attributes = Vec::new();
        for modifier in row.modifiers().iter().filter(|m| m.domain == "targetID") {
            let source_attr = crate::sde::dogma::python_effects::projected_interim_source(modifier.modifying).unwrap_or(modifier.modifying);
            let value = source.value(Key::Module(ids.assistance), source_attr);
            if !value.is_finite() { continue; }
            if !values.iter().any(|&(attr, _)| attr == modifier.modifying) { values.push((modifier.modifying, value)); }
            if modifier.func == "ItemModifier" && [ATTR_MAX_TARGET_RANGE, ATTR_SCAN_RESOLUTION].contains(&modifier.modified) {
                let before = target.value(Key::Ship, modifier.modified);
                if before.is_finite() && before > 0.0 { attributes.push(modifier.modified); }
            }
        }
        if attributes.is_empty() { continue; }
        let projection = Projection { source_ship: ids.ships[0], source_module: ids.assistance, source_type: ty,
            effect_id: entry.effect_id, kind: effect.kind, until_ms: i64::MAX, values, charge_values: Vec::new(), charge_type: None, scramble: None };
        let projected = Fit::load_projected(db, sde.clone(), ids.characters[2], ids.ships[2], vec![projection]).await.expect("evaluate capital projection");
        if attributes.iter().any(|&attr| {
            let before = target.value(Key::Ship, attr);
            let after = projected.value(Key::Ship, attr);
            after.is_finite() && after != before
        }) { return true; }
    }
    false
}

struct Candidate {
    ship: i32, module: i32, fuel: i32, quantity: i32,
    capital: bool, duration_modifier: bool, period: f64, capacitor: f64, hub_room: f64, hub_volume: f64,
}

fn entosis_candidates(sde: &Sde) -> Vec<Candidate> {
    use infinity_core::consts::dogma::*;
    use crate::services::{effects::registry::{self, EffectAttribute}, fit::base_attribute};
    let base = |ty, attr| base_attribute(sde.types.get(ty), sde.dogma.attribute(attr), attr);
    let effect = registry::resolve(sde, EFFECT_ENTOSIS_LINK).expect("Entosis effect metadata");
    let duration = effect.attribute(EffectAttribute::Duration).expect("Entosis duration attribute");
    let range = effect.attribute(EffectAttribute::Range).expect("Entosis range attribute");
    let discharge = effect.attribute(EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED);
    let multiplier = sde.dogma.attributes_by_name("entosisDurationMultiplier").next().expect("SDE Entosis duration multiplier").attribute_id;
    let hi_power: Vec<_> = sde.dogma.effects_by_name("hiPower").map(|e| e.effect_id).collect();
    let hub_volume = sde.types.get(sde.sovereignty.hub().type_id).and_then(|t| t.volume).expect("Hub volume");
    assert!(hub_volume.is_finite() && hub_volume >= 0.0);
    let mut candidates = Vec::new();
    for module in sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_ENTOSIS_LINK)
        && t.effects().iter().any(|e| hi_power.contains(&e.effect_id))
        && t.effects().iter().any(|e| e.effect_id == EFFECT_ONLINE as i32)) {
        let period = base(module.type_id, duration);
        let distance = base(module.type_id, range);
        let need = base(module.type_id, discharge);
        let fuel = base(module.type_id, ATTR_CONSUMPTION_TYPE);
        let units = base(module.type_id, ATTR_CONSUMPTION_QUANTITY);
        if !period.is_finite() || period <= 0.0 || !distance.is_finite() || distance < 0.0
            || !need.is_finite() || need < 0.0 || !fuel.is_finite() || fuel <= 0.0
            || fuel.fract() != 0.0 || fuel > f64::from(i32::MAX)
            || !units.is_finite() || units <= 0.0 || units.ceil() > f64::from(i32::MAX / FIXTURE_FUEL_CYCLES) { continue; }
        let fuel_type = fuel as i32;
        let Some(fuel_volume) = sde.types.get(fuel_type).and_then(|t| t.volume).filter(|v| v.is_finite() && *v > 0.0) else { continue };
        let quantity = (units.ceil() as i32) * FIXTURE_FUEL_CYCLES;
        for ship in sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP)) {
            let id = ship.type_id;
            let positive = [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_MASS, ATTR_MAX_VELOCITY, ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE, ATTR_MAX_TARGET_RANGE, ATTR_SCAN_RESOLUTION, ATTR_BASE_WARP_SPEED, ATTR_WARP_SPEED_MULTIPLIER];
            if !positive.iter().all(|&a| { let v = base(id, a); v.is_finite() && v > 0.0 })
                || base(id, ATTR_HI_SLOTS) < 1.0
                || base(id, ATTR_CPU_OUTPUT) < base(module.type_id, ATTR_CPU)
                || base(id, ATTR_POWER_OUTPUT) < base(module.type_id, ATTR_POWER)
                || base(id, ATTR_CAPACITOR_CAPACITY) < need * 3.0
                || !base(id, ATTR_CAPACITY).is_finite()
                || base(id, ATTR_CAPACITY) < fuel_volume * f64::from(quantity) { continue; }
            if fixture_skills(sde, &[id, module.type_id]).is_none() { continue; }
            candidates.push(Candidate { ship: id, module: module.type_id, fuel: fuel_type, quantity,
                capital: base(id, ATTR_IS_CAPITAL_SIZE) > 0.0,
                duration_modifier: ship.modifiers(&sde.dogma, duration).any(|(effect, modifier)|
                    effect.effect_category == Some(0) && effect.name.as_deref() == Some("entosisDurationMultiply")
                    && modifier.func == "LocationRequiredSkillModifier" && modifier.domain == "shipID"
                    && modifier.op == crate::services::fit::OP_POST_MUL && modifier.modifying == multiplier
                    && modifier.skill.is_some_and(|skill| ATTR_REQUIRED_SKILLS.iter().any(|&attr| base(module.type_id, attr) == f64::from(skill)))),
                period, capacitor: base(id, ATTR_CAPACITOR_CAPACITY),
                hub_room: base(id, ATTR_CAPACITY) - fuel_volume * f64::from(quantity), hub_volume });
        }
    }
    candidates.sort_by(|a, b| a.period.total_cmp(&b.period)
        .then(b.duration_modifier.cmp(&a.duration_modifier))
        .then(a.capacitor.total_cmp(&b.capacitor)).then(a.ship.cmp(&b.ship)).then(a.module.cmp(&b.module)));
    candidates
}

fn cloak_candidates(sde: &Sde) -> Vec<i32> {
    use infinity_core::consts::dogma::{EFFECT_CLOAKING, EFFECT_CLOAKING_PROTOTYPE, EFFECT_CLOAKING_WARP_SAFE, EFFECT_ONLINE};
    let high: Vec<_> = sde.dogma.effects_by_name("hiPower").map(|e| e.effect_id).collect();
    let mut result: Vec<_> = sde.types.iter().filter(|ty| ty.category_id == Some(CATEGORY_MODULE)
        && ty.effects().iter().any(|e| [EFFECT_CLOAKING, EFFECT_CLOAKING_PROTOTYPE, EFFECT_CLOAKING_WARP_SAFE].contains(&e.effect_id))
        && ty.effects().iter().any(|e| e.effect_id == EFFECT_ONLINE as i32)
        && ty.effects().iter().any(|e| high.contains(&e.effect_id)))
        .map(|ty| ty.type_id).collect();
    result.sort_unstable();
    result
}

fn assistance_candidates(sde: &Sde) -> Vec<(i32, i32)> {
    use infinity_core::{consts::{FLAG_FITTING_FIRST, FLAG_MED_SLOT_FIRST, dogma::EFFECT_ONLINE}};
    use crate::{services::effects::{EffectKind, registry::{self, EffectAttribute, EffectFlag}}};
    let racks = [("hiPower", FLAG_HI_SLOT_FIRST + 2), ("medPower", FLAG_MED_SLOT_FIRST), ("loPower", FLAG_FITTING_FIRST)];
    let mut result: Vec<_> = sde.types.iter().filter(|ty| ty.category_id == Some(CATEGORY_MODULE)
        && ty.effects().iter().any(|e| e.effect_id == EFFECT_ONLINE as i32)
        && ty.effects().iter().any(|e| registry::resolve(sde, e.effect_id).is_some_and(|effect|
            matches!(effect.kind, EffectKind::Projected { .. }) && effect.has(EffectFlag::Assistance)
                && sde.dogma.effect(e.effect_id).is_some_and(|row| row.modifiers().iter().any(|modifier|
                    modifier.domain == "targetID" && modifier.func == "ItemModifier"
                    && [infinity_core::consts::dogma::ATTR_MAX_TARGET_RANGE, infinity_core::consts::dogma::ATTR_SCAN_RESOLUTION].contains(&modifier.modified)))
                && effect.attribute(EffectAttribute::Duration).is_some_and(|attr| ty.fit_attribute(attr).is_some_and(|v| v.is_finite() && v > 0.0))
                && effect.attribute(EffectAttribute::Range).is_some_and(|attr| ty.fit_attribute(attr).is_some_and(|v| v.is_finite() && v > 0.0)))))
        .filter_map(|ty| racks.iter().find(|(name, _)| sde.dogma.effects_by_name(name).any(|effect| ty.effects().iter().any(|e| e.effect_id == effect.effect_id)))
            .map(|(_, flag)| (ty.type_id, *flag))).collect();
    result.sort_unstable();
    result
}

fn fixture_skills(sde: &Sde, types: &[i32]) -> Option<Vec<(i32, i32, i64)>> {
    use infinity_core::consts::{CATEGORY_SKILL, dogma::{ATTR_REQUIRED_SKILLS, ATTR_REQUIRED_SKILL_LEVELS, ATTR_SKILL_TIME_CONSTANT, MAX_SKILL_LEVEL}};
    let mut pending = types.to_vec();
    let mut levels = BTreeMap::<i32, i32>::new();
    while let Some(ty) = pending.pop() {
        let data = sde.types.get(ty)?;
        for (&skill_attr, &level_attr) in ATTR_REQUIRED_SKILLS.iter().zip(&ATTR_REQUIRED_SKILL_LEVELS) {
            let id = data.attribute(skill_attr).unwrap_or(0.0);
            let level = data.attribute(level_attr).unwrap_or(0.0);
            if id == 0.0 { continue; }
            if !id.is_finite() || id <= 0.0 || id.fract() != 0.0 || id > f64::from(i32::MAX)
                || !level.is_finite() || level.fract() != 0.0 || level < 1.0 || level > f64::from(MAX_SKILL_LEVEL) { return None; }
            let (id, level) = (id as i32, level as i32);
            if sde.types.get(id)?.category_id != Some(CATEGORY_SKILL) { return None; }
            let entry = levels.entry(id).or_default();
            if *entry < level { *entry = level; pending.push(id); }
        }
    }
    levels.into_iter().map(|(id, level)| {
        let rank = sde.types.get(id)?.fit_attribute(ATTR_SKILL_TIME_CONSTANT)?;
        if !rank.is_finite() || rank <= 0.0 { return None; }
        let mut points = 1_i64;
        while crate::services::skills::formulas::level_from_points(points, rank) < level {
            points = points.checked_mul(2)?;
        }
        Some((id, level, points))
    }).collect()
}

fn star_position(sde: &Sde, system: i64) -> [f64; 3] {
    let star = sde.map.celestials(system).find(|c| c.group_id == Some(i64::from(GROUP_SUN))).unwrap();
    [star.x.unwrap() + sde.sovereignty.hub().deploy.fixed_star_distance_m as f64, star.y.unwrap(), star.z.unwrap()]
}

pub(super) async fn cleanup(ids: Ids) {
    if ids.account == 0 { return; }
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let _market = crate::services::market::lock().await;
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let mut items: Vec<i64> = tx.query("SELECT hub_item_id FROM game.sov_history WHERE system_id=ANY($1) UNION SELECT item_id FROM game.sov_structures WHERE system_id=ANY($1)", &[&&ids.systems[..]]).await?.iter().map(|r| r.get("hub_item_id")).collect();
        items.extend(ids.ships); items.extend(ids.sources); items.extend(ids.modules); items.extend(ids.fuel); items.push(ids.cloak); items.push(ids.assistance); items.sort_unstable(); items.dedup();
        for item in &items { crate::scheduler::cancel(tx, &crate::services::sovereignty::jobs::destruct_id(*item)).await?; }
        tx.execute("DELETE FROM game.sov_link_source_changes WHERE character_id=ANY($1)", &[&&ids.characters[..]]).await?;
        tx.execute("DELETE FROM game.sov_link_sources WHERE character_id=ANY($1)", &[&&ids.characters[..]]).await?;
        tx.execute("DELETE FROM game.sov_capture_targets WHERE system_id=ANY($1)", &[&&ids.systems[..]]).await?;
        tx.execute("DELETE FROM game.sov_structures WHERE system_id=ANY($1)", &[&&ids.systems[..]]).await?;
        tx.execute("DELETE FROM game.sov_history WHERE system_id=ANY($1)", &[&&ids.systems[..]]).await?;
        tx.execute("DELETE FROM game.sov_systems WHERE system_id=ANY($1)", &[&&ids.systems[..]]).await?;
        tx.execute("DELETE FROM game.sov_alliance_policy WHERE alliance_id=ANY($1)", &[&&ids.alliances[..]]).await?;
        tx.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1)", &[&&ids.characters[..]]).await?;
        tx.execute("DELETE FROM game.alliance_history WHERE corporation_id=ANY($1)", &[&&ids.corps[..]]).await?;
        tx.execute("DELETE FROM game.alliance_applications WHERE corporation_id=ANY($1) OR alliance_id=ANY($2)", &[&&ids.corps[..], &&ids.alliances[..]]).await?;
        tx.execute("DELETE FROM game.corp_wallets WHERE corporation_id=ANY($1)", &[&&ids.corps[..]]).await?;
        tx.execute("DELETE FROM game.items WHERE item_id=ANY($1)", &[&items]).await?;
        tx.execute("DELETE FROM game.skills WHERE character_id=ANY($1)", &[&&ids.characters[..]]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id=ANY($1) AND user_id=$2", &[&&ids.characters[..], &ids.account]).await?;
        tx.execute("DELETE FROM game.corporations WHERE corporation_id=ANY($1)", &[&&ids.corps[..]]).await?;
        tx.execute("DELETE FROM game.alliances WHERE alliance_id=ANY($1)", &[&&ids.alliances[..]]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&ids.account]).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
}
