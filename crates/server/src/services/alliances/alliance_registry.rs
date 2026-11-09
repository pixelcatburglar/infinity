//! Public and bound `allianceRegistry` calls, mirroring
//! `eve/client/script/ui/services/alliances`.
//! Shared mechanics in `services::alliances` use the alliance DAO and
//! `game.alliances`, `game.corporations`, `game.alliance_applications` and
//! `game.alliance_history`.
//! Mutual wars are supported separately; bills, ordinary capital changes and prime time are not.
use super::*;
use super::super::corps::{is_player_corp, permissions};
/// A member corporation backs `candidate` as executor; one backed by corporations holding
/// more than half the alliance's pilots takes over.
async fn support_executor(db: &Db, world: &World, me: i64, alliance: i64, candidate: i64) -> Result<(), CallError> {
    let (corp, _) = membership(db, me).await?;
    db.transaction(
        |tx, outbox: &mut Outbox| Box::pin(async move {
            if !crate::dao::corporations::registry::lock_admission(tx, &[alliance], &[corp, candidate], &[me]).await.map_err(db_err)?
                || !permissions::is_active_ceo(tx, me, corp).await? { return Err(notify_error(messages::CORP_CEO_REQUIRED_TO_CHOOSE_EXECUTOR)); }
            let members = member_lines(tx, alliance).await?;
            if !members.iter().any(|(id, _)| *id == candidate) || !members.iter().any(|(id, _)| *id == corp) {
                return Err(notify_error(messages::CORP_NOT_ALLIANCE_MEMBER));
            }
            let old = members.iter().find(|(id, _)| *id == corp).map(|(_, line)| line.clone());
            dao::support(tx, corp, candidate).await.map_err(db_err)?;
            queue_member_changed(tx, outbox, alliance, corp, old).await?;
            let votes = dao::votes(tx, alliance, candidate).await.map_err(db_err)?;
            if votes.backing * MAJORITY_DENOMINATOR > votes.total {
                let old_alliance = alliance_line_of(tx, alliance).await?;
                if dao::elect_executor(tx, alliance, candidate).await.map_err(db_err)? > 0 {
                    queue_alliance_changed(tx, outbox, alliance, old_alliance).await?;
                }
            }
            Ok(())
        }),
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await
}

async fn delete_member_inner(db: &Db, world: &World, me: i64, alliance: i64, corp: i64, my_corp: i64, now: i64, admitted: crate::services::sovereignty::governance::AdmittedAllianceChange) -> CallResult {
    let admitted = std::sync::Arc::new(admitted);
    db.transaction(
        |tx, outbox: &mut Outbox| {
            let admitted = std::sync::Arc::clone(&admitted);
            Box::pin(async move {
                if !crate::dao::corporations::registry::lock_admission(tx, &[alliance], &[my_corp, corp], &[me]).await.map_err(db_err)?
                    || membership(tx, me).await?.0 != my_corp { return Err(notify_error(messages::NOT_AUTHORIZED_TO_REMOVE_CORP_FROM_ALLIANCE)); }
                let allowed = if corp == my_corp { permissions::is_active_ceo(tx, me, corp).await? } else { executor_director(tx, me, alliance).await? };
                if !allowed { return Err(notify_error(messages::NOT_AUTHORIZED_TO_REMOVE_CORP_FROM_ALLIANCE)); }
                if !member_lines(tx, alliance).await?.iter().any(|(id, _)| *id == corp) { return Err(notify_error(messages::CORP_NOT_ALLIANCE_MEMBER)); }
                leave_alliance(tx, outbox, corp, now, &admitted).await?;
                Ok(Value::None)
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await
}

async fn delete_member(db: &Db, world: &World, me: i64, alliance: i64, corp: i64) -> CallResult {
    use crate::services::sovereignty::governance::{self, AdmittedAllianceChange};
    let my_corp = membership(db, me).await?.0;
    loop {
        if crate::dao::sovereignty::systems::for_corporation(db, corp).await.map_err(db_err)?.is_empty() {
            match delete_member_inner(db, world, me, alliance, corp, my_corp, filetime_now(), AdmittedAllianceChange::NoOwnershipChange).await {
                Err(error) if crate::services::sovereignty::is_stale(&error) => continue,
                result => return result,
            }
        }
        let db = db.clone();
        let world = world.shared().ok_or_else(|| notify_error(messages::NOT_AUTHORIZED_TO_REMOVE_CORP_FROM_ALLIANCE))?;
        return tokio::spawn(async move {
            loop {
                let now = filetime_now();
                let prepared = match governance::prepare(&db, corp, now).await {
                    Err(error) if crate::services::sovereignty::is_stale(&error) => continue,
                    result => result?,
                };
                let result = if matches!(prepared, governance::PreparedAllianceChange::NoOwnershipChange) {
                    let admitted = governance::admit(&world, prepared, Some(me))?;
                    delete_member_inner(&db, &world, me, alliance, corp, my_corp, now, admitted).await
                } else {
                    let _market = crate::services::market::lock().await;
                    let admitted = governance::admit(&world, prepared, Some(me))?;
                    delete_member_inner(&db, &world, me, alliance, corp, my_corp, now, admitted).await
                };
                match result {
                    Err(error) if crate::services::sovereignty::is_stale(&error) => continue,
                    result => return result,
                }
            }
        }).await.map_err(db_err)?;
    }
}
/// allianceRegistry, unbound (public) and bound to the session's alliance.
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let (db, world) = (&c.db, &*c.world);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    // ---- public ----
    match call.method {
        "GetAlliance" if !call.bound => return Some(alliance_row(db, int(0)).await),
        "GetAlliancePublicInfo" => return Some(alliance_row(db, int(0)).await),
        // GetRankedAlliances(maxLen): the largest alliances by pilots.
        "GetRankedAlliances" => {
            let limit = call.args.first().and_then(|v| v.as_int()).unwrap_or(DEFAULT_RANKED_LIMIT).clamp(1, MAX_RANKED_LIMIT);
            return Some(match dao::ranked(db, limit).await {
                Ok(rows) => {
                    let header = alliance_header();
                    Ok(crowset(&header, rows.into_iter().map(|r| header.row(&alliance_line(r))).collect()))
                }
                Err(e) => Err(db_err(e)),
            });
        }
        // [row(corporationID, ...)] of an alliance's members (info window).
        "GetAllianceMembers" => {
            return Some(member_lines(db, int(0)).await.map(|lines| {
                let header = member_header();
                crowset(&header, lines.into_iter().map(|(_, l)| header.row(&l)).collect())
            }))
        }
        // GetEmploymentRecord(corporationID): its alliances, newest first, then its founding
        // (the info window leaves the last entry out).
        "GetEmploymentRecord" => {
            let corp = int(0);
            if !is_player_corp(corp) {
                return Some(Ok(Value::list([])));
            }
            let rows = dao::employment(db, corp).await;
            return Some(match rows {
                Ok(rows) => {
                    let header = DbRowDescriptor::new([("allianceID", DbType::I4), ("startDate", DbType::FileTime), ("deleted", DbType::Bool)]);
                    Ok(crowset(
                        &header,
                        rows.iter()
                            .map(|r| header.row(&[r.alliance.map(Value::Int).unwrap_or(Value::None), Value::Int(r.start), Value::Bool(r.deleted)]))
                            .collect(),
                    ))
                }
                Err(e) => Err(db_err(e)),
            });
        }
        _ => {}
    }
    // ---- bound to an alliance ----
    let alliance = bound_alliance(c, call)?;
    Some(match call.method {
        "GetAlliance" => alliance_row(db, alliance).await,
        // UpdateAlliance(description, url)
        "UpdateAlliance" => {
            let corp = match membership(db, me).await { Ok((corp, _)) => corp, Err(error) => return Some(Err(error)) };
            let description = trimmed_arg_text(call, 0);
            let url = trimmed_arg_text(call, 1);
            db.transaction(
                |tx, outbox: &mut Outbox| {
                    let description = description.clone();
                    let url = url.clone();
                    Box::pin(async move {
                        if !crate::dao::corporations::registry::lock_admission(tx, &[alliance], &[corp], &[me]).await.map_err(db_err)?
                            || membership(tx, me).await?.0 != corp || !executor_director(tx, me, alliance).await? {
                            return Err(notify_error(messages::EXECUTOR_CORP_DIRECTOR_REQUIRED_TO_EDIT_ALLIANCE));
                        }
                        let old = alliance_line_of(tx, alliance).await?;
                        dao::update_alliance(tx, alliance, &description, &url).await.map_err(db_err)?;
                        queue_alliance_changed(tx, outbox, alliance, old).await?;
                        Ok(Value::None)
                    })
                },
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await
        }
        "GetMembers" => member_lines(db, alliance).await.map(|lines| {
            let header = member_header();
            indexed_rowset(&header, "corporationID", lines.into_iter().map(|(corp, l)| (corp, header.row(&l))).collect())
        }),
        // {corporationID: application}
        "GetApplications" => {
            let identity = crate::services::identity::Identity::of_client(c);
            if !call.bound || !matches!(call.bind_params, Some(Value::Tuple(parts)) if parts.len() == 2 && parts[0].as_int() == Some(alliance) && matches!(parts[1], Value::Bool(_))) || !call.args.is_empty() {
                return Some(Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION)));
            }
            admission::applications(db, &identity, alliance).await
        }
        "UpdateApplication" | "GMForceAcceptApplication" => {
            if !call.bound || !matches!(call.bind_params, Some(Value::Tuple(parts)) if parts.len() == 2 && parts[0].as_int() == Some(alliance) && matches!(parts[1], Value::Bool(_))) {
                return Some(Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION)));
            }
            let decision = match (call.method, call.args) {
                ("UpdateApplication", [corp, Value::Unicode(text), state]) if corp.as_int().is_some() && state.as_int().and_then(|v| i32::try_from(v).ok()).is_some() && !text.contains('\0') => {
                    (corp.as_int().unwrap(), admission::Decision::Manager { text: text.clone(), state: i32::try_from(state.as_int().unwrap()).unwrap() })
                }
                ("GMForceAcceptApplication", [corp]) if corp.as_int().is_some() => (corp.as_int().unwrap(), admission::Decision::Force { role: c.login.role }),
                _ => return Some(Err(notify_error(messages::ALLIANCE_APPLICATION_STATUS_INVALID))),
            };
            admission::decide(db, world, &crate::services::identity::Identity::of_client(c), alliance, decision.0, decision.1).await.map(|_| Value::None)
        }
        "DeclareExecutorSupport" => support_executor(db, world, me, alliance, int(0)).await.map(|_| Value::None),
        // DeleteMember(corpID): leave (the corporation's CEO) or remove (executor directors).
        "DeleteMember" => delete_member(db, world, me, alliance, int(0)).await,
        // {toID: row(toID, relationship)}
        "GetRelationships" => match dao::relationships(db, alliance).await {
            Ok(rows) => {
                let header = relationship_header();
                let rows = rows.iter().map(|r| (r.to, header.row(&[Value::Int(r.to), Value::Int(r.value as i64)]))).collect();
                Ok(indexed_rowset(&header, "toID", rows))
            }
            Err(e) => Err(db_err(e)),
        },
        // SetRelationship(relationship, toID) / DeleteRelationship(toID)
        "SetRelationship" | "DeleteRelationship" => {
            let corp = match membership(db, me).await { Ok((corp, _)) => corp, Err(error) => return Some(Err(error)) };
            let (relationship, to) = if call.method == "SetRelationship" { (Some(int(0) as i32), int(1)) } else { (None, int(0)) };
            db.transaction(
                |tx, outbox: &mut Outbox| Box::pin(async move {
                    if !crate::dao::corporations::registry::lock_admission(tx, &[alliance], &[corp], &[me]).await.map_err(db_err)?
                        || membership(tx, me).await?.0 != corp || !executor_director(tx, me, alliance).await? {
                        return Err(notify_error(messages::EXECUTOR_CORP_DIRECTOR_REQUIRED_TO_SET_RELATIONSHIP));
                    }
                    let old = dao::relationship(tx, alliance, to).await.map_err(db_err)?;
                    match relationship {
                        Some(rel) => { dao::set_relationship(tx, alliance, to, rel).await.map_err(db_err)?; }
                        None => { dao::delete_relationship(tx, alliance, to).await.map_err(db_err)?; }
                    }
                    let line = |rel: Option<i32>| rel.map(|r| vec![Value::Int(to), Value::Int(r as i64)]);
                    let ch = change(RELATIONSHIP_COLUMNS, line(old).as_deref(), line(relationship).as_deref());
                    queue_tell(tx, outbox, alliance, None, "OnAllianceRelationshipChanged", vec![Value::Int(alliance), Value::Int(to), ch]).await?;
                    Ok(Value::None)
                }),
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await
        }
        "DeclareWarAgainst" => Err(notify_error(messages::wars::AGGRESSIVE_UNAVAILABLE)),
        // The alliance's bills (services/bills), paid by the executor's accountants.
        "GetBills" => super::super::bills::bills(db, alliance, false).await,
        "GetBillsReceivable" => super::super::bills::bills(db, alliance, true).await,
        "GetBillBalance" => match dao::bill_amount(db, int(0), alliance).await {
            Ok(r) => Ok(Value::Float(r.unwrap_or(0.0))),
            Err(e) => Err(db_err(e)),
        },
        // PayBill(billID, fromAccountKey): from the executor corporation's wallet.
        "PayBill" => {
            let (bill, key) = (int(0), call.kwargs.get("fromAccountKey").and_then(|v| v.as_int()).unwrap_or(int(1)) as i32);
            let corp = c.session.int("corpid").unwrap_or(0);
            super::super::bills::pay(db, world, bill, alliance, Some(corp), key, Some(me)).await.map(|_| Value::None)
        }
        _ => return None,
    })
}

#[cfg(test)] mod tests;
