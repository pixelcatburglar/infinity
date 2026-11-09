//! Exploration client-service dispatch.
//! Shared exploration state and data sources are documented in the parent module.

use super::*;

// eve/client/script/ui/inflight/scannerFiles/scanResults.py:result_menu selects
// result.id (str); eve/client/script/parklife/scanSvc.py:QAScanSites sends one
// tuple of those strings. probescanning/scanHandler.py:InjectResults consumes
// result objects with id, certainty, data/pos tuples and optional metadata.
async fn qa_scan_sites(c: &Client, call: &Call<'_>) -> CallResult {
    if c.login.role & infinity_core::consts::ROLE_QA == 0 {
        return Err(notify_error(messages::exploration::QA_REQUIRED));
    }
    let char_id = c.session.int("charid").filter(|id| *id > 0)
        .ok_or_else(|| notify_error(messages::exploration::SPACE_REQUIRED))?;
    let system = c.session.int("solarsystemid").filter(|id| *id > 0)
        .ok_or_else(|| notify_error(messages::exploration::SPACE_REQUIRED))?;
    let [Value::Tuple(values)] = call.args else {
        return Err(notify_error(messages::exploration::INVALID_TARGETS));
    };
    if !matches!(call.kwargs, Value::Dict(items) if items.is_empty()) {
        return Err(notify_error(messages::exploration::INVALID_TARGETS));
    }
    let mut targets = HashSet::new();
    for value in values {
        let target = match value {
            Value::Bytes(bytes) => std::str::from_utf8(bytes).ok(),
            Value::Unicode(text) => Some(text.as_str()),
            _ => None,
        }.ok_or_else(|| notify_error(messages::exploration::INVALID_TARGETS))?;
        targets.insert(target);
    }
    if targets.is_empty() {
        return Ok(Value::list([]));
    }
    ensure_loaded(&c.db, &c.world, system).await?;
    let now = filetime_now();
    let mut sites: Vec<Site> = state().sites.values()
        .filter(|site| site.system == system && targets.contains(site.target.as_str())
            && !site.wormhole.as_ref().is_some_and(|w| now - w.born > filetime_ticks_from_seconds(minutes_to_seconds(w.life_minutes) as i64)))
        .cloned().collect();
    sites.sort_by_key(|site| site.id);
    let signatures: Vec<ResolvedSignature> = sites.iter().filter(|site| site.kind == Kind::Signature).map(ResolvedSignature::from).collect();
    complete_scans(&c.db, char_id, &signatures, &fact_context(c, FactOrigin::Qa)).await?;
    state().reported.entry((char_id, system)).or_default().extend(sites.iter().map(|site| site.target.clone()));
    Ok(Value::list(sites.iter().map(|site| result_value(site, 1.0, 0.0))))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("scanMgr", "GetSystemScanMgr") => Some(Ok(c.bind("scanMgr", Value::None))),
        ("scanMgr", "QAScanSites") => Some(qa_scan_sites(c, call).await),
        ("scanMgr", "SignalTrackerRegister") => {
            let system = c.session.int("solarsystemid")?;
            if let Err(e) = c.world.system(system).await {
                return Some(Err(CallError::Internal(e)));
            }
            if let Err(e) = expire_wormholes(&c.db, &c.world).await {
                return Some(Err(e));
            }
            if let Err(e) = ensure_loaded(&c.db, &c.world, system).await {
                return Some(Err(e));
            }
            if let Err(e) = ensure_static_exit(&c.db, &c.world, system).await {
                return Some(Err(e));
            }
            c.notify("OnSignalTrackerFullState", vec![Value::Int(system), full_state(system)]);
            Some(Ok(Value::None))
        }
        // Docked (the Agency refreshing local anomalies), it's the system the station is in.
        ("scanMgr", "GetFullState") => {
            let system = c.session.int("solarsystemid").or_else(|| c.session.int("solarsystemid2"))?;
            if let Err(e) = ensure_loaded(&c.db, &c.world, system).await {
                return Some(Err(e));
            }
            Some(Ok(full_state(system)))
        }
        ("scanMgr", "RequestScans") => Some(request_scans(c, call).await),
        ("scanMgr", "ConeScan") => Some(cone_scan(c, call).await),
        ("scanMgr", "RecoverProbes") => Some(recover_probes(c, call, false).await),
        ("scanMgr", "DestroyProbe") => Some(recover_probes(c, call, true).await.map(|_| Value::None)),
        ("scanMgr", "ReconnectToLostProbes") => {
            let (system, char_id) = (c.session.int("solarsystemid")?, c.session.int("charid")?);
            let probes: Vec<Value> = state().probes.iter().filter(|(_, p)| p.char_id == char_id && p.system == system).map(|(id, p)| probe_value(*id, p)).collect();
            for p in probes {
                c.notify("OnNewProbe", vec![p]);
            }
            Some(Ok(Value::None))
        }
        ("scanMgr", "SetActivityState" | "QAOverrideProbeExpiry") => Some(Ok(Value::None)),
        _ => None,
    }
}

#[cfg(test)] mod tests;
