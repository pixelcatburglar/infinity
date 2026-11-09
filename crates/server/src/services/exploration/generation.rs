//! Generates exploration sites and wormhole pairs using server selection and placement
//! policy. The shared SDE supplies map, type, dungeon and localization candidates;
//! generated sites are persisted as game state. `eve/client/script/parklife/scanSvc.py:253-260` consumes
//! scan results, not a position generator. No usable general live-server dungeon-layout
//! source was validated in the decoded client tree; full layouts are not reproduced.
//! Mining-site chances and missing wormhole attributes use operator policy.

use super::*;

/// SDE inventory group for wormhole types.
const GROUP_WORMHOLE: i32 = 988;

pub(super) fn system_info(sde: &Sde, system: i64) -> Result<SystemInfo, CallError> {
    let r = crate::sde::dungeons::system_info(&sde.map, system)
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("no exploration system row for {system}")))?;
    Ok(SystemInfo { security: r.security, faction: r.faction, region: r.region, class: r.class })
}

/// The pirates of an empire's space (or of a pirate-held system).
pub(super) fn pirate_faction(faction: Option<i64>, region: i64) -> i64 {
    match faction {
        Some(FACTION_CALDARI) => FACTION_GURISTAS,                  // Caldari: Guristas
        Some(FACTION_MINMATAR) => FACTION_ANGEL_CARTEL,             // Minmatar: Angel Cartel
        Some(FACTION_AMARR) if region % 2 == 0 => 500019,           // Amarr: Sansha's Nation
        Some(FACTION_AMARR | 500007 | 500008) => 500012,            // Amarr, Ammatar, Khanid: Blood Raiders
        Some(FACTION_GALLENTE) => 500020,                           // Gallente: Serpentis
        Some(f @ (FACTION_GURISTAS | FACTION_ANGEL_CARTEL | 500012 | 500019 | 500020)) => f,
        _ => [FACTION_GURISTAS, FACTION_ANGEL_CARTEL, 500012, 500019, 500020][(region % 5) as usize],
    }
}


pub(super) fn site_distance() -> f64 {
    SITE_OFFSET.0 + rand01() * (SITE_OFFSET.1 - SITE_OFFSET.0)
}

/// A random point well off the surface of a random planet of the system.
pub(super) fn exploration_site_position(sde: &Sde, system: i64) -> V3 {
    let (base, radius) = crate::services::site_position(sde, system).unwrap_or(([AU, 0.0, 0.0], 0.0));
    off_surface(base, radius, site_distance())
}
/// A combat dungeon of `faction` within the difficulty range: acceleration-gated
/// complexes are scanned down as signatures, open sites show as anomalies.
fn sleeper_name(name: &str) -> bool {
    let name = name.strip_prefix("Unsecured ").unwrap_or(name);
    ["Perimeter", "Frontier", "Core", "Outpost Frontier", "Fortification Frontier", "Sleeper"]
        .iter().any(|prefix| name.starts_with(prefix))
}

fn combat_candidate(d: &crate::sde::dungeons::Dungeon, name: Option<&str>, faction: i64, difficulty: (i64, i64), gated: bool, sleeper: bool) -> bool {
    d.archetype.as_deref() == Some(ARCHETYPE_COMBAT) && d.faction_id == Some(faction)
        && (difficulty.0..=difficulty.1).contains(&d.difficulty.unwrap_or(1))
        && d.gated == gated && d.has_name && (!sleeper || name.is_some_and(sleeper_name))
}

pub(super) fn pick_combat_dungeon(sde: &Sde, faction: i64, difficulty: (i64, i64), gated: bool, sleeper: bool) -> Option<DungeonPick> {
    let candidates: Vec<_> = sde.dungeons.iter().filter(|d| {
        let name = d.name_id.and_then(|id| sde.localization.english(id));
        combat_candidate(d, name, faction, difficulty, gated, sleeper)
    }).collect();
    pick(&candidates).map(|d| DungeonPick { id: d.id, name_id: d.name_id, difficulty: d.difficulty.unwrap_or(1) })
}
pub(super) fn new_site(system: i64, pos: V3, kind: Kind, strength_attr: i64, ease: f64, taken: &mut HashSet<String>) -> Site {
    let target = target_id(taken);
    taken.insert(target.clone());
    // Overlay deviation: 4 AU for strong signals, up to 8 AU for weak ones
    // (explorationscanner.signalTracker.CalculateDeviationForSignal).
    let deviation = 4.0 * AU + (1.0 - ease.clamp(0.0, 1.0)) * 4.0 * AU;
    let overlay_pos = if kind == Kind::Signature { offset(pos, rand01() * deviation) } else { pos };
    Site {
        id: NEXT_SITE_ID.fetch_add(1, Ordering::Relaxed),
        target,
        system,
        pos,
        kind,
        strength_attr,
        ease,
        dungeon_id: None,
        name_id: None,
        archetype: None,
        faction: None,
        difficulty: 1,
        overlay_pos,
        deviation: if kind == Kind::Signature { deviation } else { 0.0 },
        wormhole: None,
        npcs: None,
        in_space: false,
        containers: None,
        hacked: vec![],
    }
}

/// One combat site for the system: an anomaly or a (gated) signature.
pub(super) fn combat_site(sde: &Sde, system: i64, info: &SystemInfo, kind: Kind, taken: &mut HashSet<String>) -> Result<Option<Site>, CallError> {
    let jspace = (1..=6).contains(&info.class);
    let (faction, range) = if jspace {
        let lo = (info.class + 1) / 2 + 1; // C1-2: 2, C3-4: 3, C5-6: 4
        (500021, (lo, lo + 1))
    } else {
        let range = match info.class {
            7 => (1, 2),
            8 => (2, 4),
            _ => (3, 5),
        };
        (pirate_faction(info.faction, info.region), range)
    };
    let gated = kind == Kind::Signature && !jspace;
    let Some(d) = pick_combat_dungeon(sde, faction, range, gated, jspace) else { return Ok(None) };
    let pos = exploration_site_position(sde, system);
    let ease = [1.0, 0.8, 0.6, 0.5, 0.4][(d.difficulty.clamp(1, 5) - 1) as usize];
    let mut site = new_site(system, pos, kind, STRENGTH_ALL, ease, taken);
    site.dungeon_id = Some(d.id);
    site.name_id = d.name_id;
    site.archetype = Some(24);
    site.faction = Some(faction);
    site.difficulty = d.difficulty;
    Ok(Some(site))
}

/// A wormhole out of `system` (of `class`) to a class drawn from `targets`, with its K162
/// in the destination. Returns (wormhole site, K162 site).
pub(super) async fn wormhole_pair(
    db: &Db,
    sde: &Sde,
    system: i64,
    class: i64,
    targets: &[i64],
    taken: &mut HashSet<String>,
    destination_targets: &mut HashMap<i64, HashSet<String>>,
) -> Result<Option<(Site, Site)>, CallError> {
    let mut types: Vec<(i32, i64)> = sde.types.group_types(GROUP_WORMHOLE).iter().filter_map(|&type_id| {
        if type_id == TYPE_K162 { return None; }
        let class = sde.types.attribute(type_id, ATTR_WH_TARGET_CLASS)?.round_ties_even() as i64;
        targets.contains(&class).then_some((type_id, class))
    }).collect();
    for i in (1..types.len()).rev() {
        types.swap(i, (rand01() * (i + 1) as f64) as usize % (i + 1));
    }
    // The first type (in random order) whose class has somewhere to lead.
    let mut chosen = None;
    for (type_id, dest_class) in types {
        let candidates: Vec<i64> = sde.dungeons.systems_of_class(dest_class).iter().copied().filter(|s| *s != system).collect();
        if let Some(dest) = pick(&candidates) {
            chosen = Some((type_id, dest_class, dest));
            break;
        }
    }
    let Some((type_id, dest_class, dest_system)) = chosen else {
        warn!("exploration: no wormhole destination of class {targets:?} from {system}");
        return Ok(None);
    };
    let policy = &crate::config::game().exploration;
    let life = sde.types.attribute(type_id, ATTR_WH_LIFE_MINUTES).unwrap_or(policy.wormhole_lifetime_min as f64);
    let mass = sde.types.attribute(type_id, ATTR_WH_MASS).unwrap_or(policy.wormhole_total_mass_kg as f64);
    let jump = sde.types.attribute(type_id, ATTR_WH_JUMP_MASS).unwrap_or(policy.wormhole_jump_mass_kg as f64);
    let nebula = |id| sde.map.system(id).and_then(|s| s.region_id)
        .and_then(|id| sde.map.region(id)).and_then(|r| r.nebula_id).unwrap_or(0);
    let born = filetime_now();
    let mut here = new_site(system, exploration_site_position(sde, system), Kind::Signature, STRENGTH_WORMHOLE, WORMHOLE_SCAN_EASE, taken);
    if !destination_targets.contains_key(&dest_system) {
        let mut used: HashSet<String> = crate::dao::exploration::sites(db, dest_system).await.map_err(db_err)?
            .into_iter().map(|s| s.target).collect();
        used.extend(system_sites(dest_system).into_iter().map(|s| s.target));
        destination_targets.insert(dest_system, used);
    }
    let mut there = new_site(dest_system, exploration_site_position(sde, dest_system), Kind::Signature, STRENGTH_WORMHOLE, WORMHOLE_SCAN_EASE, destination_targets.get_mut(&dest_system).unwrap());
    let base = Wormhole {
        type_id,
        dest_system,
        dest_class,
        other: there.id,
        born,
        life_minutes: life,
        mass_max: mass,
        mass_used: 0.0,
        jump_mass: jump,
        nebula: nebula(dest_system),
    };
    there.wormhole = Some(Wormhole {
        type_id: TYPE_K162,
        dest_system: system,
        dest_class: class,
        other: here.id,
        nebula: nebula(system),
        ..base.clone()
    });
    here.wormhole = Some(base);
    Ok(Some((here, there)))
}
/// Serialises loading and generation (so two pilots entering a system generate it once).
pub(super) fn load_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default)
}

/// Bring a system's sites into memory: load what is stored (closing wormholes past their
/// lifetime), generate the system's sites on its first visit ever, and put wormholes and
/// site containers that were in space back into space.
pub(super) async fn ensure_loaded(db: &Db, world: &Arc<World>, system: i64) -> Result<(), CallError> {
    if state().loaded.contains(&system) {
        return Ok(());
    }
    let _guard = load_lock().lock().await;
    if state().loaded.contains(&system) {
        return Ok(());
    }
    if NEXT_SITE_ID.load(Ordering::Relaxed) == 0 {
        let next = crate::dao::exploration::next_site_id(db).await.map_err(db_err)?;
        NEXT_SITE_ID.store(next, Ordering::Relaxed);
    }
    // Wormholes past their lifetime close at both ends.
    crate::dao::exploration::delete_expired(db, filetime_now()).await.map_err(db_err)?;
    let stored: Vec<Site> = crate::dao::exploration::sites(db, system).await.map_err(db_err)?.into_iter().map(site_from_row).collect();
    let generated = crate::dao::exploration::is_generated(db, system).await.map_err(db_err)?;
    let new = if generated {
        // Systems generated before mining sites existed get theirs now.
        if stored.iter().any(|s| s.mining_field().is_some()) {
            vec![]
        } else {
            let info = system_info(world.sde(), system)?;
            let mut taken = stored.iter().map(|s| s.target.clone()).collect();
            mining_sites(world.sde(), system, &info, &mut taken)?
        }
    } else {
        let taken = stored.iter().map(|s| s.target.clone()).collect();
        generate(db, world.sde(), system, taken).await?
    };
    for s in &new {
        save_site(db, s).await?;
    }
    if !generated {
        crate::dao::exploration::mark_generated(db, system, filetime_now()).await.map_err(db_err)?;
    }
    {
        let mut st = state();
        for s in stored.iter().chain(new.iter()) {
            st.sites.entry(s.id).or_insert_with(|| s.clone());
        }
        st.loaded.insert(system);
    }
    // Back into space: wormholes someone had found, and spawned site containers.
    for s in &stored {
        if let (true, Some(w)) = (s.in_space, &s.wormhole) {
            world.add_ship(system, wormhole_ball(s, w)).await.map_err(CallError::Internal)?;
        }
        match (&s.containers, s.mining_field()) {
            // A mined field's rocks come back with the system's (services/mining); its
            // watch resumes.
            (Some(rocks), Some(_)) => {
                track_site_clearance(db.clone(), world.clone(), s.id, system, rocks.clone());
            }
            (Some(containers), None) => restore_containers(db, world, s, containers).await?,
            _ => {}
        }
    }
    Ok(())
}

/// Wormhole space always has a connection out to known space: if the system has none (it
/// collapsed, or was never made), a new one opens.
pub(super) async fn ensure_static_exit(db: &Db, world: &World, system: i64) -> Result<(), CallError> {
    let _guard = load_lock().lock().await;
    let has_exit = system_sites(system).iter().any(|s| s.wormhole.as_ref().is_some_and(|w| (7..=9).contains(&w.dest_class)));
    if has_exit {
        return Ok(());
    }
    let info = system_info(world.sde(), system)?;
    if !(1..=6).contains(&info.class) {
        return Ok(());
    }
    let mut taken: HashSet<String> = system_sites(system).into_iter().map(|s| s.target).collect();
    let Some((here, there)) = wormhole_pair(db, world.sde(), system, info.class, &[7, 8, 9], &mut taken, &mut HashMap::new()).await? else { return Ok(()) };
    save_site(db, &here).await?;
    save_site(db, &there).await?;
    info!("exploration: {system} gets a new exit {} -> {}", here.target, there.system);
    {
        let mut st = state();
        st.sites.insert(here.id, here.clone());
        st.sites.insert(there.id, there.clone());
    }
    broadcast_sites(world, system, std::slice::from_ref(&here), &[]).await;
    Ok(())
}

/// A system's sites, generated the first time anyone looks. Wormholes also create their
/// far end in the destination system.
pub(super) async fn generate(db: &Db, sde: &Sde, system: i64, mut taken: HashSet<String>) -> Result<Vec<Site>, CallError> {
    let info = system_info(sde, system)?;
    let mut new = vec![];
    let jspace = (1..=6).contains(&info.class);
    let (anomalies, combat_sigs) = match info.class {
        1..=6 => (2 + info.class / 2, 0),
        7 => (2, 1),
        8 => (3, 1),
        _ => (4, 2),
    };
    for _ in 0..anomalies {
        new.extend(combat_site(sde, system, &info, Kind::Anomaly, &mut taken)?);
    }
    for _ in 0..combat_sigs {
        new.extend(combat_site(sde, system, &info, Kind::Signature, &mut taken)?);
    }
    new.extend(mining_sites(sde, system, &info, &mut taken)?);
    // A data and a relic site (two of each in null-sec).
    let hack_sites = if info.class == 9 { 2 } else { 1 };
    for _ in 0..hack_sites {
        for relic in [false, true] {
            new.extend(hack_site(sde, system, &info, relic, &mut taken)?);
        }
    }
    // Wormholes: a static out of wormhole space to known space or deeper, and in known
    // space a connection or two into wormhole space.
    let wormholes: Vec<&[i64]> = if jspace {
        let deeper: &[i64] = if info.class >= 4 { &[4, 5, 6] } else { &[1, 2, 3] };
        vec![&[7, 8, 9], deeper]
    } else {
        let into: &[i64] = &[1, 2, 3, 4, 5, 6];
        if info.class == 7 { vec![into] } else { vec![into, &[7, 8, 9]] }
    };
    let mut destination_targets = HashMap::new();
    for targets in wormholes {
        if let Some((here, there)) = wormhole_pair(db, sde, system, info.class, targets, &mut taken, &mut destination_targets).await? {
            new.push(here);
            new.push(there);
        }
    }
    info!("exploration: {system} (class {}, {:.2}) has {} new sites", info.class, info.security, new.len());
    Ok(new)
}

/// Ore anomalies everywhere, ice fields in some of known space, gas clouds to scan for.
pub(super) fn mining_sites(sde: &Sde, system: i64, info: &SystemInfo, taken: &mut HashSet<String>) -> Result<Vec<Site>, CallError> {
    use crate::space::asteroids::Field;
    let jspace = (1..=6).contains(&info.class);
    let mut out = vec![];
    for _ in 0..if info.class == 9 { 2 } else { 1 } {
        out.extend(mining_site(sde, system, info, Field::Ore, taken)?);
    }
    if !jspace && rand01() < crate::config::game().exploration.ice_site_chance {
        out.extend(mining_site(sde, system, info, Field::Ice, taken)?);
    }
    if jspace || info.class != 7 || rand01() < crate::config::game().exploration.gas_site_chance {
        out.extend(mining_site(sde, system, info, Field::Gas, taken)?);
    }
    Ok(out)
}

fn mining_name(name: &str, needle: &str, suffix: bool) -> bool {
    !name.contains("OLD") && if suffix { name.ends_with(needle) } else { name.contains(needle) }
}

fn hack_name(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|rest| rest.starts_with(' ')) && !name.contains(':')
}

fn hack_candidates<'a>(
    dungeons: impl Iterator<Item = &'a crate::sde::dungeons::Dungeon>,
    name: impl Fn(i64) -> Option<&'a str>,
    faction: i64, archetype: &str, prefix: &str,
) -> Vec<&'a crate::sde::dungeons::Dungeon> {
    let mut eligible = Vec::new();
    let mut preferred = Vec::new();
    let mut fallback = Vec::new();
    for d in dungeons.filter(|d| d.faction_id == Some(faction)) {
        let is_preferred = d.archetype.as_deref() == Some(archetype);
        if is_preferred && d.has_name { fallback.push(d); }
        if matches!(d.archetype.as_deref(), Some("24" | "44" | "45"))
            && d.name_id.and_then(&name).is_some_and(|text| hack_name(text, prefix)) {
            eligible.push(d);
            if is_preferred { preferred.push(d); }
        }
    }
    if !preferred.is_empty() { preferred } else if !eligible.is_empty() { eligible } else { fallback }
}

/// An ore or ice anomaly, or a gas signature, named after one of the build's sites.
pub(super) fn mining_site(sde: &Sde, system: i64, info: &SystemInfo, field: crate::space::asteroids::Field, taken: &mut HashSet<String>) -> Result<Option<Site>, CallError> {
    use crate::space::asteroids::Field;
    let jspace = (1..=6).contains(&info.class);
    let (archetype, needle, suffix) = match field {
        Field::Ore => (ARCHETYPE_ORE, if jspace { "Perimeter" } else { "Deposit" }, false),
        Field::Ice => (ARCHETYPE_ICE, "Belt", false),
        Field::Gas => (ARCHETYPE_GAS, if jspace { "Reservoir" } else { " Nebula" }, !jspace),
    };
    let archetype_text = archetype.to_string();
    let candidates: Vec<_> = sde.dungeons.iter().filter(|d| {
        d.archetype.as_deref() == Some(archetype_text.as_str())
            && d.name_id.and_then(|id| sde.localization.english(id))
                .is_some_and(|name| mining_name(name, needle, suffix))
    }).collect();
    let Some(r) = pick(&candidates) else { return Ok(None) };
    let (kind, strength, ease) = match field {
        Field::Gas => (Kind::Signature, STRENGTH_LADAR, if jspace { 0.6 } else { 0.8 }),
        _ => (Kind::Anomaly, STRENGTH_GRAVIMETRIC, 1.0),
    };
    let mut site = new_site(system, exploration_site_position(sde, system), kind, strength, ease, taken);
    site.dungeon_id = Some(r.id);
    site.name_id = r.name_id;
    site.archetype = Some(archetype);
    site.difficulty = match info.class {
        7 => 1,
        8 => 2,
        _ => 3,
    };
    Ok(Some(site))
}

/// A data or relic site: the faction's (or the Sleepers') hackable ruins.
pub(super) fn hack_site(sde: &Sde, system: i64, info: &SystemInfo, relic: bool, taken: &mut HashSet<String>) -> Result<Option<Site>, CallError> {
    let jspace = (1..=6).contains(&info.class);
    let faction = if jspace { 500021 } else { pirate_faction(info.faction, info.region) };
    // Retail naming by security: Local/Crumbling (high), Regional/Decayed (low),
    // Central/Ruined (null); Unsecured/Forgotten in wormhole space.
    let (prefix, tier, ease) = match (info.class, relic) {
        (1..=6, false) => ("Unsecured", (info.class + 1) / 2 + 1, 0.5),
        (1..=6, true) => ("Forgotten", (info.class + 1) / 2 + 1, 0.5),
        (7, false) => ("Local", 1, 0.8),
        (7, true) => ("Crumbling", 1, 0.8),
        (8, false) => ("Regional", 2, 0.6),
        (8, true) => ("Decayed", 2, 0.6),
        (_, false) => ("Central", 3, 0.5),
        (_, true) => ("Ruined", 3, 0.5),
    };
    let archetype = if relic { ARCHETYPE_RELIC } else { ARCHETYPE_DATA };
    let candidates = hack_candidates(sde.dungeons.iter(), |id| sde.localization.english(id), faction, archetype, prefix);
    let Some(r) = pick(&candidates) else { return Ok(None) };
    let strength = if relic { STRENGTH_MAGNETOMETRIC } else { STRENGTH_RADAR };
    let mut site = new_site(system, exploration_site_position(sde, system), Kind::Signature, strength, ease, taken);
    site.dungeon_id = Some(r.id);
    site.name_id = r.name_id;
    site.archetype = Some(if relic { 44 } else { 45 });
    site.faction = Some(faction);
    site.difficulty = tier.clamp(1, 4);
    Ok(Some(site))
}

#[cfg(test)]
mod tests;
