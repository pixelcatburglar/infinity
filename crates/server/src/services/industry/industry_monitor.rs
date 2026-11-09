//! `industryMonitor` dispatch for the client industry package, using authorized materials.
//! `ConnectJob` available quantities are projected to the client display limit;
//! stored stack quantities stay exact.
use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    Some(match (call.service, call.method) {
        // ConnectJob(job.dump()) -> (monitorID, {typeID: quantity available})
        ("industryMonitor", "ConnectJob") => {
            let request = call.args.first().cloned().unwrap_or(Value::None);
            let result = async {
                let bp = match request.get("blueprintID") {
                    None | Some(Value::None) => None,
                    Some(value) => {
                        let id = value.as_int().filter(|id| *id > 0)
                            .ok_or_else(|| user_error(messages::MSG_BLUEPRINT_NOT_FOUND, vec![]))?;
                        Some(industry_dao::blueprint(&c.db, id).await.map_err(db_err)?
                            .ok_or_else(|| user_error(messages::MSG_BLUEPRINT_NOT_FOUND, vec![]))?)
                    }
                };
                let fac = facility(&c.sde, request.get("facilityID").and_then(Value::as_int).unwrap_or(0))?;
                let owner = if let Some(bp) = &bp {
                    bp.owner
                } else {
                    match request.get("inputLocation").and_then(|location| field(location, "ownerID")) {
                        None | Some(Value::None) => me,
                        Some(value) => value.as_int().filter(|id| *id > 0)
                            .ok_or_else(|| user_error(messages::MSG_INDUSTRY_BLUEPRINT_ACCESS_DENIED, vec![]))?,
                    }
                };
                let locations = authorization::locations(&c.db, &c.sde, me, owner, &fac).await?;
                if let Some(bp) = &bp {
                    if !locations.iter().any(|l| l.item_id == bp.location && l.flag_id == bp.flag && l.can_view) {
                        return Err(user_error(messages::MSG_INDUSTRY_BLUEPRINT_ACCESS_DENIED, vec![]));
                    }
                }
                let input = authorization::select_location(request.get("inputLocation"), &locations, true)?;
                industry_dao::available(&c.db, owner, input.item_id, input.flag_id).await.map_err(db_err)
            }.await;
            let have = match result { Ok(h) => h, Err(e) => return Some(Err(e)) };
            Ok(Value::tuple([Value::Int(1), Value::Dict(have.into_iter().map(|(t, q)| (Value::Int(t as i64), Value::Int(crate::services::dogma::info::cap_client_quantity(q)))).collect())]))
        }
        ("industryMonitor", "DisconnectJob") => Ok(Value::None),

        _ => return None,
    })
}
