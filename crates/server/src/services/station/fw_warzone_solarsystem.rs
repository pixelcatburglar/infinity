//! `fwWarzoneSolarsystem` occupation and headquarters reads.
//! `eve/client/script/ui/services/fwWarzoneSvc.py` and the shipcaster dashboard
//! consume startup faction home systems validated against the map dataset.
//! Using home systems is server display policy, not official warzone HQ data;
//! occupation is not modelled.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(Ok(match (call.service, call.method) {
        ("fwWarzoneSolarsystem", "GetLocalOccupationState") => Value::tuple([c.session.get("solarsystemid2").clone(), Value::None]),
        ("fwWarzoneSolarsystem", "GetHQSystemIDs") => {
            let mut systems = Vec::new();
            for faction_id in crate::consts::WARFARE_FACTIONS {
                let Some(system_id) = c.sde.factions.get(faction_id).and_then(|f| f.solar_system_id) else {
                    return Some(Err(CallError::Internal(anyhow::anyhow!("missing home system for warfare faction {faction_id}"))));
                };
                if c.sde.map.system(system_id).is_none() {
                    return Some(Err(CallError::Internal(anyhow::anyhow!("invalid home system {system_id} for warfare faction {faction_id}"))));
                }
                systems.push((Value::Int(faction_id), Value::Int(system_id)));
            }
            Value::Dict(systems)
        }
        _ => return None,
    }))
}
