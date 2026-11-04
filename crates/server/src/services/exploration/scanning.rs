//! Exploration scanning mechanics.
//! Shared exploration state and data sources are documented in the parent module.
//! Unresolved scan results use the configured uncertainty floor.

use super::*;

/// Persist a batch before publishing any in-memory authorization.
pub(super) async fn complete_scans(db: &Db, char_id: i64, site_ids: &[i64]) -> Result<(), CallError> {
    let mut ids = site_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }
    let ids = Arc::new(ids);
    db.transaction(|tx, _: &mut ()| {
        let ids = Arc::clone(&ids);
        Box::pin(async move {
            for &id in ids.iter() {
                crate::dao::exploration::mark_scanned(tx, char_id, id).await.map_err(db_err)?;
            }
            Ok::<(), CallError>(())
        })
    }, |(), ()| async { Ok(()) }).await?;
    state().scanned.extend(ids.iter().map(|&id| (char_id, id)));
    Ok(())
}


// ---------------------------------------------------------------- the sensor overlay

pub(super) fn anomaly_info(s: &Site) -> Value {
    keyval([
        ("position", pos_value(s.pos)),
        ("targetID", Value::str(&s.target)),
        ("difficulty", Value::Int(s.difficulty.max(1))),
        ("dungeonID", s.dungeon_id.map(Value::Int).unwrap_or(Value::None)),
        ("archetypeID", s.archetype.map(Value::Int).unwrap_or(Value::None)),
        ("siteID", Value::Int(s.id)),
        ("instanceID", Value::Int(s.id)),
        ("dungeonNameID", s.name_id.map(Value::Int).unwrap_or(Value::None)),
        ("factionID", s.faction.map(Value::Int).unwrap_or(Value::None)),
        ("scanStrengthAttribute", Value::Int(s.strength_attr)),
        ("allowedTypes", Value::list([])),
        ("entryObjectTypeID", Value::None),
        ("solarSystemID", Value::Int(s.system)),
    ])
}

pub(super) fn signature_info(s: &Site) -> Value {
    keyval([
        ("position", pos_value(s.overlay_pos)),
        ("targetID", Value::str(&s.target)),
        ("difficulty", Value::Int(s.difficulty.max(1))),
        ("dungeonID", Value::None),
        ("archetypeID", Value::None),
        ("deviation", Value::Float(s.deviation)),
    ])
}

pub(super) fn full_state(system: i64) -> Value {
    let sites = system_sites(system);
    let of = |kind: Kind, info: fn(&Site) -> Value| {
        Value::Dict(sites.iter().filter(|s| s.kind == kind).map(|s| (Value::Int(s.id), info(s))).collect())
    };
    Value::tuple([of(Kind::Anomaly, anomaly_info), of(Kind::Signature, signature_info), Value::Dict(vec![]), Value::Dict(vec![])])
}

/// Tell every pilot in `system` about sites that came or went.
pub(super) async fn broadcast_sites(world: &World, system: i64, added: &[Site], removed: &[Site]) {
    let pilots: Vec<i64> = world.with(system, |s| s.pilots().filter_map(|b| b.char_id).collect()).await.unwrap_or_default();
    for (kind, event, info) in [
        (Kind::Anomaly, "OnSignalTrackerAnomalyUpdate", anomaly_info as fn(&Site) -> Value),
        (Kind::Signature, "OnSignalTrackerSignatureUpdate", signature_info),
    ] {
        let add: Vec<(Value, Value)> = added.iter().filter(|s| s.kind == kind).map(|s| (Value::Int(s.id), info(s))).collect();
        let gone: Vec<Value> = removed.iter().filter(|s| s.kind == kind).map(|s| Value::Int(s.id)).collect();
        if add.is_empty() && gone.is_empty() {
            continue;
        }
        for ch in &pilots {
            world.notify_pilot(*ch, event, vec![Value::Int(system), Value::Dict(add.clone()), Value::list(gone.clone())]);
        }
    }
}

// ---------------------------------------------------------------- probes

/// A pilot's probe strength (baseSensorStrength with the Astrometrics skills) and scan
/// time (ms, Astrometric Acquisition).
pub(super) async fn probe_skills(ex: &(impl Executor + ?Sized), sde: &Sde, char_id: i64, at: i64) -> Result<(f64, f64), CallError> {
    let skills = crate::services::skills::effective::snapshot_at(ex, sde, char_id, at).await?;
    let level = |skill: i32| skills.level(skill) as f64;
    let strength = (1.0 + 0.05 * level(SKILL_ASTROMETRICS)) * (1.0 + 0.05 * level(SKILL_RANGEFINDING));
    let scan_ms = 10_000.0 * (1.0 - 0.1 * level(SKILL_ACQUISITION));
    Ok((strength, scan_ms))
}

pub(super) fn probe_value(id: i64, p: &Probe) -> Value {
    keyval([
        ("probeID", Value::Int(id)),
        ("typeID", Value::Int(p.type_id as i64)),
        ("pos", pos_value(p.pos)),
        ("destination", pos_value(p.pos)),
        ("expiry", Value::Int(p.expiry)),
    ])
}
/// Field of a KeyVal (or dict) sent back by the client.
pub(super) fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Instance(_, state) => state.get(key),
        Value::Reduce { data, dict, .. } | Value::NewObj { data, dict, .. } => data
            .items()
            .and_then(|d| d.get(2))
            .and_then(|s| s.get(key))
            .or_else(|| dict.iter().find(|(k, _)| k.as_text().is_some_and(|t| t == key)).map(|(_, v)| v)),
        other => other.get(key),
    }
}

pub(super) fn as_pos(v: Option<&Value>) -> Option<V3> {
    let items = v?.items()?;
    match items {
        [x, y, z] => Some([x.as_float()?, y.as_float()?, z.as_float()?]),
        _ => None,
    }
}

/// How well a set of (position, range, strength) probes resolves a site.
pub(super) fn certainty(site: &Site, probes: &[(V3, f64, f64)]) -> (f64, f64) {
    let mut signal = 0.0;
    let mut covering = 0;
    let mut tightest = f64::MAX;
    for &(pos, range, strength) in probes {
        let d = dist(pos, site.pos);
        if d > range {
            continue;
        }
        covering += 1;
        tightest = tightest.min(range);
        // Strength halves with each doubling of range beyond 0.25 AU, and fades toward
        // the edge of the probe's sphere.
        let falloff = 1.0 - (d / range).powi(2) * 0.5;
        signal += strength * (0.25 * AU / range).min(1.0) * falloff;
    }
    if covering == 0 {
        return (0.0, 0.0);
    }
    // One probe gives a sphere, two a circle, three a pair of points: only four or more
    // pin a site down.
    let cap = [0.0, 0.24, 0.49, 0.74, 1.0][covering.min(4)];
    let c = (signal * site.ease / CERTAINTY_DIVISOR).min(cap);
    (if c >= 0.999 { 1.0 } else { c }, tightest)
}

pub(super) fn result_value(s: &Site, certainty: f64, spread: f64) -> Value {
    let anomaly = s.kind == Kind::Anomaly;
    let perfect = certainty >= 1.0;
    let mut f: Vec<(&str, Value)> = vec![
        ("id", Value::str(&s.target)),
        ("scanGroupID", Value::Int(if anomaly { SCAN_GROUP_ANOMALIES } else { SCAN_GROUP_SIGNATURES })),
        ("certainty", Value::Float(certainty)),
        ("itemID", Value::None),
    ];
    if perfect {
        f.push(("data", pos_value(s.pos)));
        f.push(("pos", pos_value(s.pos)));
    } else {
        // A sphere of uncertainty around a guess near the site.
        let radius = (spread * (1.0 - certainty)).max(crate::config::game().exploration.scan_uncertainty_floor_m);
        f.push(("data", Value::Float(radius)));
        f.push(("pos", pos_value(offset(s.pos, rand01() * radius * 0.5))));
    }
    if certainty >= 0.25 {
        f.push(("groupID", Value::Int(if anomaly { GROUP_COSMIC_ANOMALY } else { GROUP_COSMIC_SIGNATURE })));
        f.push(("strengthAttributeID", Value::Int(s.strength_attr)));
        f.push(("difficulty", Value::Int(s.difficulty.max(1))));
    }
    if certainty >= 0.75 {
        f.push(("typeID", s.wormhole.as_ref().map(|w| Value::Int(w.type_id as i64)).unwrap_or(Value::None)));
        f.push(("dungeonID", s.dungeon_id.map(Value::Int).unwrap_or(Value::None)));
        f.push(("dungeonNameID", s.name_id.map(Value::Int).unwrap_or(Value::None)));
        f.push(("archetypeID", s.archetype.map(Value::Int).unwrap_or(Value::None)));
        f.push(("factionID", s.faction.map(Value::Int).unwrap_or(Value::None)));
    }
    keyval(f)
}

/// bound scanMgr.RequestScans(probes): move the probes to their destinations, scan, report.
pub(super) async fn request_scans(c: &mut Client, call: &Call<'_>) -> CallResult {
    let (Some(system), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("charid")) else { return Ok(Value::None) };
    ensure_loaded(&c.db, &c.world, system).await?;
    let (_, scan_ms) = probe_skills(&c.db, &c.sde, char_id, filetime_now()).await?;
    // (probeID, destination, range)
    let mut moved = vec![];
    if let Some(Value::Dict(probes)) = call.args.first() {
        for (k, p) in probes {
            let Some(id) = k.as_int() else { continue };
            let Some(dest) = as_pos(field(p, "destination")).or_else(|| as_pos(field(p, "pos"))) else { continue };
            let range = field(p, "scanRange").and_then(|v| v.as_float()).unwrap_or(8.0 * AU);
            moved.push((id, dest, range));
        }
    }
    let now = filetime_now();
    let mut scanning = vec![];
    let mut probe_ids = vec![];
    {
        let mut st = state();
        st.probes.retain(|_, p| p.expiry > now);
        for &(id, dest, range) in &moved {
            let Some(p) = st.probes.get_mut(&id).filter(|p| p.char_id == char_id && p.system == system) else { continue };
            p.pos = dest;
            scanning.push((dest, range, p.strength));
            probe_ids.push((id, probe_value(id, p)));
        }
    }
    let sent = Value::Dict(probe_ids.iter().map(|(id, v)| (Value::Int(*id), v.clone())).collect());
    c.notify("OnSystemScanStarted", vec![Value::Int(now), Value::Int(scan_ms as i64), sent]);
    let ids: Vec<i64> = probe_ids.iter().map(|(id, _)| *id).collect();
    let (world, db) = (c.world.clone(), c.db.clone());
    tokio::spawn(async move {
        if !world.after(system, scan_ms as u64).await {
            return;
        }
        let job_world = world.clone();
        world.post(system, move |s| s.defer(async move {
        let sites = system_sites(system);
        let mut candidates = vec![];
        let mut complete = vec![];
        for s in &sites {
            // Anomalies and already authorized signatures are exact without probes.
            let known = if s.kind == Kind::Anomaly {
                true
            } else {
                match is_scanned(&db, char_id, s.id).await {
                    Ok(known) => known,
                    Err(e) => {
                        warn!("reading scan authorization for {}: {}", s.target, err_text(e));
                        continue;
                    }
                }
            };
            let (c, spread) = if known {
                (1.0, 0.0)
            } else if scanning.is_empty() {
                (0.01, s.deviation)
            } else {
                certainty(s, &scanning)
            };
            if c <= 0.0 {
                continue;
            }
            let newly_complete = !known && c >= 1.0 && s.kind == Kind::Signature;
            if newly_complete {
                complete.push(s.id);
            }
            candidates.push((s, c, spread, newly_complete));
        }
        let committed = match complete_scans(&db, char_id, &complete).await {
            Ok(()) => true,
            Err(e) => {
                warn!("scan completion failed: {}", err_text(e));
                false
            }
        };
        let mut results = vec![];
        let mut reported = HashSet::new();
        for (s, c, spread, newly_complete) in candidates {
            if newly_complete && !committed {
                continue;
            }
            reported.insert(s.target.clone());
            results.push(result_value(s, c, spread));
        }
        let absent: Vec<Value> = {
            let mut st = state();
            let before = st.reported.insert((char_id, system), reported.clone()).unwrap_or_default();
            before.difference(&reported).map(Value::str).collect()
        };
        job_world.notify_pilot(
            char_id,
            "OnSystemScanStopped",
            vec![Value::list(ids.into_iter().map(Value::Int)), Value::List(results), Value::List(absent)],
        );
        }));
    });
    Ok(Value::None)
}

/// bound scanMgr.RecoverProbes(probeIDs) -> recovered probe IDs; they return to cargo.
pub(super) async fn recover_probes(c: &mut Client, call: &Call<'_>, destroy: bool) -> CallResult {
    let _lock = crate::services::market::lock().await;
    let (Some(ship), Some(char_id)) = (c.session.int("shipid"), c.session.int("charid")) else { return Ok(Value::list([])) };
    let ids = value_ids(call.args.first());
    let mut back: HashMap<i32, i32> = HashMap::new();
    let mut done = vec![];
    let mut recovered_by_system: HashMap<i64, Vec<i64>> = HashMap::new();
    {
        let mut st = state();
        for id in ids {
            if st.probes.get(&id).is_some_and(|p| p.char_id == char_id) {
                let p = st.probes.remove(&id).unwrap();
                *back.entry(p.type_id).or_default() += 1;
                recovered_by_system.entry(p.system).or_default().push(id);
                done.push(id);
            }
        }
    }
    if !destroy {
        let mut outbox = Outbox::default();
        for (type_id, n) in back {
            crate::services::items::deposit(&c.db, &c.sde, &mut outbox, char_id, ship, FLAG_CARGO, type_id, n, ship).await?;
        }
        dispatch_client((), outbox, c)?;
    }
    for (system, gone) in recovered_by_system {
        if destroy {
            for id in gone {
                c.world.notify_pilot(char_id, "OnRemoveProbe", vec![Value::Int(id)]);
            }
        } else {
            c.world.schedule(system, PROBE_RECOVERY_DELAY_MS, move |s| {
                for id in gone {
                    s.notify_pilot(char_id, "OnRemoveProbe", vec![Value::Int(id)]);
                }
            });
        }
    }
    Ok(Value::list(done.into_iter().map(Value::Int)))
}

/// bound scanMgr.ConeScan(angle, range, x, y, z): everything in the cone, by type.
pub(super) async fn cone_scan(c: &mut Client, call: &Call<'_>) -> CallResult {
    let n = |i: usize| call.args.get(i).and_then(|v| v.as_float()).unwrap_or(0.0);
    let (angle, range, dir) = (n(0), n(1), [n(2), n(3), n(4)]);
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Ok(Value::list([])) };
    let Some(hits) = c.world.with(system, move |s| {
        let me = s.sample_position(ship)?;
        let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt().max(CONE_DIRECTION_MIN_LENGTH);
        let half = (angle / CONE_HALF_ANGLE_DIVISOR).min(std::f64::consts::PI);
        let mut hits = vec![];
        for b in s.balls() {
            if b.id == ship || b.cloak != 0 {
                continue;
            }
            let pos = b.position_at(s.now_ms());
            let v = [pos[0] - me[0], pos[1] - me[1], pos[2] - me[2]];
            let d = dist(pos, me);
            if d > range || d < 1.0 {
                continue;
            }
            let cos = (v[0] * dir[0] + v[1] * dir[1] + v[2] * dir[2]) / (d * len);
            if half < std::f64::consts::PI && cos.clamp(-1.0, 1.0).acos() > half {
                continue;
            }
            hits.push((b.id, b.type_id));
        }
        Some(hits)
    }).await.flatten() else { return Ok(Value::list([])) };
    let groups: HashMap<i32, i64> = hits.iter().filter_map(|&(_, id)| {
        c.world.sde().types.get(id).map(|t| (id, i64::from(t.group_id.unwrap_or(0))))
    }).collect();
    Ok(Value::List(
        hits.into_iter()
            .map(|(id, t)| keyval([("id", Value::Int(id)), ("typeID", Value::Int(t as i64)), ("groupID", Value::Int(groups.get(&t).copied().unwrap_or(0)))]))
            .collect(),
    ))
}

#[cfg(test)] mod tests;
