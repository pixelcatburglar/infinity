//! Public and bound `allianceRegistry` calls, mirroring
//! `eve/client/script/ui/services/alliances`.
//! Shared mechanics in `services::alliances` use the alliance DAO and
//! `game.alliances`, `game.corporations`, `game.alliance_applications` and
//! `game.alliance_history`.
//! Mutual wars are supported separately; bills, sovereignty, capital systems and prime time are not.
use super::*;
use super::super::corps::{ceo_of, is_player_corp};
use tracing::info;
/// Join a corporation and, when required, make its application effective atomically.
pub(crate) async fn admit(db: &Db, world: &World, alliance: i64, corp: i64, require_application: bool) -> Result<(), CallError> {
    let now = filetime_now();
    db.transaction(
        |tx, outbox| Box::pin(async move {
            let allied = dao::live_corporation_alliance(tx, corp).await.map_err(db_err)?
                .ok_or_else(|| notify_error(messages::CORP_NOT_FOUND))?;
            if allied.is_some() {
                return Err(notify_error(messages::TARGET_CORP_ALREADY_IN_ALLIANCE));
            }
            if !dao::exists(tx, alliance).await.map_err(db_err)? {
                return Err(notify_error(messages::ALLIANCE_NOT_FOUND));
            }
            let old_alliance = alliance_line_of(tx, alliance).await?;
            let application = if require_application {
                Some(dao::application(tx, alliance, corp).await.map_err(db_err)?
                    .ok_or_else(|| notify_error(messages::ALLIANCE_APPLICATION_NOT_FOUND))?)
            } else {
                None
            };
            set_alliance(tx, outbox, corp, Some(alliance), now).await?;
            let others = dao::delete_other_applications(tx, corp, alliance).await.map_err(db_err)?;
            if let Some(application) = application {
                if dao::update_application(tx, alliance, corp, APP_EFFECTIVE, application.state).await.map_err(db_err)? == 0 {
                    return Err(notify_error(messages::ALLIANCE_APPLICATION_NOT_FOUND));
                }
                queue_application_changed(tx, outbox, alliance, corp, Some(application_line(application))).await?;
            }
            for application in others {
                let other_alliance = application.alliance;
                queue_application_changed(tx, outbox, other_alliance, corp, Some(application_line(application))).await?;
            }
            queue_member_changed(tx, outbox, alliance, corp, None).await?;
            queue_alliance_changed(tx, outbox, alliance, old_alliance).await
        }),
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await
}
/// A member corporation backs `candidate` as executor; one backed by corporations holding
/// more than half the alliance's pilots takes over.
async fn support_executor(db: &Db, world: &World, me: i64, alliance: i64, candidate: i64) -> Result<(), CallError> {
    let (corp, _) = membership(db, me).await?;
    if ceo_of(db, corp).await? != Some(me) {
        return Err(notify_error(messages::CORP_CEO_REQUIRED_TO_CHOOSE_EXECUTOR));
    }
    let members = member_lines(db, alliance).await?;
    if !members.iter().any(|(c, _)| *c == candidate) || !members.iter().any(|(c, _)| *c == corp) {
        return Err(notify_error(messages::CORP_NOT_ALLIANCE_MEMBER));
    }
    let old = members.iter().find(|(c, _)| *c == corp).map(|(_, l)| l.clone());
    dao::support(db, corp, candidate).await.map_err(db_err)?;
    member_changed(db, world, alliance, corp, old).await?;
    let votes = dao::votes(db, alliance, candidate).await.map_err(db_err)?;
    let (backing, total) = (votes.backing, votes.total);
    if backing * MAJORITY_DENOMINATOR > total {
        let old_alliance = alliance_line_of(db, alliance).await?;
        let changed = dao::elect_executor(db, alliance, candidate).await.map_err(db_err)?;
        if changed > 0 {
            info!("alliances: {candidate} elected executor of {alliance}");
            alliance_changed(db, world, alliance, old_alliance).await?;
        }
    }
    Ok(())
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
            if !executor_director(db, me, alliance).await.unwrap_or(false) {
                return Some(Err(notify_error(messages::EXECUTOR_CORP_DIRECTOR_REQUIRED_TO_EDIT_ALLIANCE)));
            }
            let old = alliance_line_of(db, alliance).await.ok().flatten();
            match dao::update_alliance(db, alliance, &trimmed_arg_text(call, 0), &trimmed_arg_text(call, 1)).await
            {
                Ok(_) => alliance_changed(db, world, alliance, old).await.map(|_| Value::None),
                Err(e) => Err(db_err(e)),
            }
        }
        "GetMembers" => member_lines(db, alliance).await.map(|lines| {
            let header = member_header();
            indexed_rowset(&header, "corporationID", lines.into_iter().map(|(corp, l)| (corp, header.row(&l))).collect())
        }),
        // {corporationID: application}
        "GetApplications" => match dao::applications(db, dao::ApplicationFilter::Alliance(alliance)).await {
            Ok(rows) => {
                let header = application_header();
                let rows = rows.into_iter().map(|r| (r.corporation, header.row(&application_line(r)))).collect();
                Ok(indexed_rowset(&header, "corporationID", rows))
            }
            Err(e) => Err(db_err(e)),
        },
        // UpdateApplication(corpID, applicationText, state): accepting admits the corporation.
        "UpdateApplication" | "GMForceAcceptApplication" => {
            let corp = int(0);
            let state = if call.method == "GMForceAcceptApplication" { APP_ACCEPTED } else { int(2) as i32 };
            if !executor_director(db, me, alliance).await.unwrap_or(false) {
                return Some(Err(notify_error(messages::EXECUTOR_CORP_DIRECTOR_REQUIRED_TO_HANDLE_APPLICATION)));
            }
            if state == APP_ACCEPTED {
                admit(db, world, alliance, corp, true).await.map(|_| Value::None)
            } else {
                let old = match dao::application(db, alliance, corp).await {
                    Ok(Some(old)) => old,
                    Ok(None) => return Some(Err(notify_error(messages::ALLIANCE_APPLICATION_NOT_FOUND))),
                    Err(e) => return Some(Err(db_err(e))),
                };
                if state != APP_REJECTED {
                    return Some(Err(notify_error(messages::ALLIANCE_APPLICATION_STATUS_INVALID)));
                }
                match dao::update_application(db, alliance, corp, APP_REJECTED, old.state).await {
                    Ok(0) => Err(notify_error(messages::ALLIANCE_APPLICATION_NOT_FOUND)),
                    Ok(_) => application_changed(db, world, alliance, corp, Some(application_line(old))).await.map(|_| Value::None),
                    Err(e) => Err(db_err(e)),
                }
            }
        }
        "DeclareExecutorSupport" => support_executor(db, world, me, alliance, int(0)).await.map(|_| Value::None),
        // DeleteMember(corpID): leave (the corporation's CEO) or remove (executor directors).
        "DeleteMember" => {
            let corp = int(0);
            let (my_corp, _) = match membership(db, me).await {
                Ok(m) => m,
                Err(e) => return Some(Err(e)),
            };
            let allowed = if corp == my_corp {
                ceo_of(db, corp).await.ok().flatten() == Some(me)
            } else {
                executor_director(db, me, alliance).await.unwrap_or(false)
            };
            if !allowed {
                return Some(Err(notify_error(messages::NOT_AUTHORIZED_TO_REMOVE_CORP_FROM_ALLIANCE)));
            }
            match member_lines(db, alliance).await {
                Ok(lines) if lines.iter().any(|(c, _)| *c == corp) => {
                    let now = filetime_now();
                    db.transaction(
                        |tx, outbox| Box::pin(async move { leave_alliance(tx, outbox, corp, now).await }),
                        |value, outbox| async move { dispatch_world(value, outbox, world) },
                    ).await.map(|_| Value::None)
                },
                Ok(_) => Err(notify_error(messages::CORP_NOT_ALLIANCE_MEMBER)),
                Err(e) => Err(e),
            }
        }
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
            if !executor_director(db, me, alliance).await.unwrap_or(false) {
                return Some(Err(notify_error(messages::EXECUTOR_CORP_DIRECTOR_REQUIRED_TO_SET_RELATIONSHIP)));
            }
            let (relationship, to) = if call.method == "SetRelationship" { (Some(int(0) as i32), int(1)) } else { (None, int(0)) };
            let old: Option<i32> = match dao::relationship(db, alliance, to).await {
                Ok(r) => r,
                Err(e) => return Some(Err(db_err(e))),
            };
            let stored = match relationship {
                Some(rel) => dao::set_relationship(db, alliance, to, rel).await,
                None => dao::delete_relationship(db, alliance, to).await,
            };
            if let Err(e) = stored {
                return Some(Err(db_err(e)));
            }
            let line = |rel: Option<i32>| rel.map(|r| vec![Value::Int(to), Value::Int(r as i64)]);
            let ch = change(RELATIONSHIP_COLUMNS, line(old).as_deref(), line(relationship).as_deref());
            tell(db, world, alliance, None, "OnAllianceRelationshipChanged", vec![Value::Int(alliance), Value::Int(to), ch]).await.map(|_| Value::None)
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
            let owed = match crate::dao::bills::owned(db, bill, alliance).await {
                Ok(r) => r,
                Err(e) => return Some(Err(db_err(e))),
            };
            let allowed = executor_director(db, me, alliance).await.unwrap_or(false)
                || (super::super::bills::is_accountant(db, me, corp).await.unwrap_or(false)
                    && dao::is_executor(db, alliance, corp).await.ok().unwrap_or(false));
            if !owed || !allowed || !super::super::corpwallet::may_take(db, me, corp, key).await.unwrap_or(false) {
                return Some(Err(notify_error(messages::BILL_PAYMENT_NOT_AUTHORIZED)));
            }
            super::super::bills::pay(db, world, bill, Some(corp), key).await.map(|_| Value::None)
        }
        _ => return None,
    })
}

#[cfg(test)] mod tests;
