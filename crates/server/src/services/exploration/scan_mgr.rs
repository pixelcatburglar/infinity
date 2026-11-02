//! Exploration client-service dispatch.
//! Shared exploration state and data sources are documented in the parent module.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("scanMgr", "GetSystemScanMgr") => Some(Ok(c.bind("scanMgr", Value::None))),
        ("scanMgr", "SignalTrackerRegister") => {
            let system = c.session.int("solarsystemid")?;
            if let Err(e) = crate::services::mining::ensure_fields(&c.db, &c.world, system).await {
                return Some(Err(e));
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
