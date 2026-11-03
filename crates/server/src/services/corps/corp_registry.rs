//! Player corporation `corpRegistry` dispatch, mirroring
//! `eve/client/script/ui/services/corporation`.
//! Uses the shared corporation mechanics in `services::corps` and its DAOs,
//! including `game.corporations`, `game.characters`, `game.corp_applications` and startup SDE corporations.
//! Founding uses effective management skill. Share ownership and dividends use the corporation
//! ledger; alliance calls have separate handlers.

use super::*;
use crate::dao::corporations::dispatch as dispatch_dao;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "corpRegistry" {
        return None;
    }
    // SetAccountKey(accountKey): the wallet division the member acts from.
    if call.method == "SetAccountKey" {
        let key = call.args.first().and_then(|v| v.as_int()).unwrap_or(ACCOUNT_CASH as i64);
        c.change_session(vec![("corpAccountKey", Value::Int(key))]);
        return Some(Ok(Value::None));
    }
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let (db, world) = (&c.db, &*c.world);
    let corp = bound_corp(c, call);
    let governance_bound = match call.bind_params {
        Some(p) => p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int())).unwrap_or(0),
        None => corp,
    };
    Some(match call.method {
        "AddCorporation" => found(c, call).await,
        // GetSuggestedTickerNames(name) -> [KeyVal(tickerName)]
        "GetSuggestedTickerNames" => {
            let name = call.args.first().and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            Ok(Value::list(super::alliances::suggested_names(&name).into_iter().map(|t| keyval([("tickerName", Value::str(t))]))))
        }
        // (canLeave, error, errorDetails)
        "CanLeaveCurrentCorporation" => match membership(db, me).await {
            Ok((my_corp, _)) => {
                let blocked = if !is_player_corp(my_corp) {
                    Some("CrpCantQuitDefaultCorporation")
                } else {
                    match (ceo_of(db, my_corp).await, members(db, my_corp, registry::MemberFilter::All, &[]).await) {
                        (Ok(Some(ceo)), Ok(list)) if ceo == me && list.len() > 1 => Some("CrpCantQuitCEO"),
                        (Err(e), _) | (_, Err(e)) => return Some(Err(e)),
                        _ => None,
                    }
                };
                Ok(Value::tuple([Value::Bool(blocked.is_none()), blocked.map(Value::str).unwrap_or(Value::None), Value::None]))
            }
            Err(e) => Err(e),
        },
        "CanBeKickedOut" => {
            let target = int(0).unwrap_or(0);
            ceo_of(db, corp).await.map(|ceo| Value::Bool(ceo != Some(target) || target == me))
        }
        "KickOutMember" | "KickOutMembers" => {
            let targets: Vec<i64> = match call.method {
                "KickOutMember" => int(0).into_iter().collect(),
                _ => value_id_list(call.args.first()),
            };
            let (my_corp, roles) = match membership(db, me).await {
                Ok(m) => m,
                Err(e) => return Some(Err(e)),
            };
            for target in &targets {
                if *target != me {
                    let (their_corp, _) = match membership(db, *target).await {
                        Ok(m) => m,
                        Err(e) => return Some(Err(e)),
                    };
                    if their_corp != my_corp || roles & ROLE_DIRECTOR == 0 || ceo_of(db, my_corp).await.ok().flatten() == Some(*target) {
                        return Some(Err(notify_error(messages::CANNOT_REMOVE_MEMBER)));
                    }
                }
                if let Err(e) = leave(db, &c.sde, world, *target).await {
                    return Some(Err(e));
                }
            }
            Ok(if call.method == "KickOutMembers" { Value::list([]) } else { Value::None })
        }
        // InsertApplication(corporationID, text)
        "InsertApplication" => {
            let target = int(0).unwrap_or(0);
            let text = call.args.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            if !is_player_corp(target) {
                return Some(Err(notify_error(messages::NPC_CORP_DOES_NOT_ACCEPT_APPLICATIONS)));
            }
            let recruiting: Option<bool> = match dispatch_dao::recruiting(db, target).await {
                Ok(r) => r,
                Err(e) => return Some(Err(db_err(e))),
            };
            if recruiting != Some(true) {
                return Some(Err(notify_error(messages::CORP_NOT_RECRUITING)));
            }
            dispatch_dao::insert_application(db, target, me, &text, APP_APPLIED, filetime_now())
                .await
                .map(Value::Int)
                .map_err(db_err)
        }
        // InsertInvitation(characterID)
        "InsertInvitation" => {
            let target = int(0).unwrap_or(0);
            match membership(db, me).await {
                Ok((my_corp, roles)) if is_player_corp(my_corp) && roles & (ROLE_PERSONNEL_MANAGER | ROLE_DIRECTOR) != 0 => dispatch_dao::insert_invitation(db, my_corp, target, APP_INVITED_BY_CORPORATION, filetime_now())
                    .await
                    .map(|_| Value::None)
                    .map_err(db_err),
                Ok(_) => Err(notify_error(messages::PERSONNEL_MANAGER_REQUIRED_TO_INVITE)),
                Err(e) => Err(e),
            }
        }
        "GetMyApplications" => applications(db, registry::ApplicationFilter::Character, me, true).await,
        "GetApplications" => applications(db, registry::ApplicationFilter::Corporation, corp, false).await,
        "GetMyOldApplications" => old_applications(db, me).await,
        "UpdateApplicationOffer" => update_application(c, call).await,
        // [row(ownerID, ownerName, typeID, gender)] of the members.
        "GetEveOwners" => match dispatch_dao::owners(db, corp).await
        {
            Ok(rows) => {
                let header = DbRowDescriptor::new([("ownerID", DbType::I4), ("ownerName", DbType::WStr), ("typeID", DbType::I2), ("gender", DbType::I2)]);
                Ok(Value::list(rows.iter().map(|r| {
                    header.row(&[Value::Int(r.character_id), Value::unicode(r.name.as_str()), Value::Int(TYPE_CHARACTER as i64), Value::Int(r.gender_id)])
                })))
            }
            Err(e) => Err(db_err(e)),
        },
        // GetMembersPaged(page): everyone on the first page.
        "GetMembersPaged" => {
            if int(0).unwrap_or(1) > 1 {
                return Some(Ok(Value::list([])));
            }
            members(db, corp, registry::MemberFilter::All, &[]).await.map(|rows| Value::list(rows.iter().map(|r| member_row(r, corp))))
        }
        "GetMember" => {
            let id = int(0).unwrap_or(0);
            members(db, corp, registry::MemberFilter::Include, &[id]).await.map(|rows| rows.first().map(|r| member_row(r, corp)).unwrap_or(Value::None))
        }
        "GetMembersByIds" => {
            let ids = value_id_list(call.args.first());
            members(db, corp, registry::MemberFilter::Include, &ids).await.map(|rows| Value::list(rows.iter().map(|r| member_row(r, corp))))
        }
        "GetMemberIDsByQuery" => members(db, corp, registry::MemberFilter::All, &[]).await.map(|rows| Value::list(rows.iter().map(|r| Value::Int(r.character_id)))),
        "GetMemberTrackingInfo" | "GetMemberTrackingInfoSimple" => members(db, corp, registry::MemberFilter::All, &[]).await.map(|rows| {
            rowset(
                &["characterID", "roles", "grantableRoles", "baseID", "title", "startDateTime", "logonDateTime", "logoffDateTime", "locationID", "shipTypeID"],
                rows.iter()
                    .map(|r| {
                        let location = r.station_id.or(r.solar_system_id);
                        vec![
                            Value::Int(r.character_id),
                            Value::Int(r.roles),
                            Value::Int(0),
                            Value::None,
                            Value::unicode(""),
                            Value::Int(r.start_date),
                            Value::Int(r.logoff_date),
                            Value::Int(r.logoff_date),
                            location.map(Value::Int).unwrap_or(Value::None),
                            r.ship_type_id.map(|t| Value::Int(t as i64)).unwrap_or(Value::None),
                        ]
                    })
                    .collect(),
            )
        }),
        // [characterID] of the members who could take over as CEO (Corporation Management).
        "GetNumberOfPotentialCEOs" => potential_ceos(db, corp, me).await.map(|ids| Value::list(ids.into_iter().map(Value::Int))),
        "UpdateMember" => update_member(db, world, me, call.args).await.map(|_| Value::None),
        // UpdateMembers(Rowset of UpdateMember argument lines)
        "UpdateMembers" => {
            for line in rowset_lines(call.args.first()) {
                if let Err(e) = update_member(db, world, me, &line).await {
                    return Some(Err(e));
                }
            }
            Ok(Value::None)
        }
        "ExecuteActions" => execute_actions(db, world, me, call).await.map(|_| Value::None),
        "GetTitles" => titles(db, corp).await,
        // UpdateTitle(titleID, titleName, roles, grantableRoles, rolesAtHQ, grantableRolesAtHQ, rolesAtBase, ...)
        "UpdateTitle" => update_titles(db, world, me, corp, vec![call.args.to_vec()]).await.map(|_| Value::None),
        "UpdateTitles" => update_titles(db, world, me, corp, rowset_lines(call.args.first())).await.map(|_| Value::None),
        "DeleteTitle" => delete_title(db, world, me, corp, int(0).unwrap_or(0)).await.map(|_| Value::None),
        "ResignFromCEO" => resign_ceo(db, &c.sde, world, me, int(0)).await,
        "UpdateDivisionNames" => {
            let names: Vec<Option<String>> = (0..CORP_DIVISION_NAME_COUNT).map(|i| call.args.get(i).and_then(|v| v.as_text()).map(|t| t.into_owned())).collect();
            match membership(db, me).await {
                Ok((my_corp, roles)) if my_corp == corp && roles & ROLE_DIRECTOR != 0 => dispatch_dao::set_division_names(db, corp, &names)
                    .await
                    .map(|_| Value::None)
                    .map_err(db_err),
                Ok(_) => Err(notify_error(messages::DIRECTOR_REQUIRED_TO_EDIT_DIVISION_NAMES)),
                Err(e) => Err(e),
            }
        }
        // The member limit follows the CEO's skills; it isn't enforced, so nothing to recompute.
        "UpdateCorporationAbilities" => Ok(Value::Bool(true)),
        // KeyVal(corpID, allianceID, title, title1..title16): a member's titles, for show info.
        "GetInfoWindowDataForChar" => info_window_data(db, int(0).unwrap_or(me)).await,
        "GetSharesByShareholder" => {
            let company = match call.args.first() {
                None | Some(Value::Int(0)) => false,
                Some(Value::Int(1)) => true,
                _ => return Some(Err(notify_error(messages::corporations::SHARE_AUTHORITY_REQUIRED))),
            };
            shares::holdings(db, me, governance_bound, company).await
        }
        "GetShareholders" => match call.args.first() {
            Some(Value::Int(issuer)) if *issuer > 0 => shares::shareholders(db, me, governance_bound, *issuer).await,
            _ => Err(notify_error(messages::corporations::SHARES_UNAVAILABLE)),
        },
        "GetMemberIDsWithMoreThanAvgShares" => match membership(db, me).await {
            Ok((actual, _)) if actual == governance_bound => crate::dao::corporations::shares::above_average_members(db, actual).await.map(|ids| Value::list(ids.into_iter().map(Value::Int))).map_err(db_err),
            Ok(_) => Err(notify_error(messages::corporations::SHARE_AUTHORITY_REQUIRED)),
            Err(e) => Err(e),
        },
        "MoveCompanyShares" | "MovePrivateShares" => {
            if call.args.len() != 3 { return Some(Err(notify_error(messages::corporations::INVALID_SHARE_TRANSFER))); }
            let (issuer, to, quantity) = match (call.args.first(), call.args.get(1), call.args.get(2)) {
                (Some(Value::Int(issuer)), Some(Value::Int(to)), Some(Value::Int(quantity))) if *issuer > 0 && *to > 0 && *quantity > 0 => (*issuer, *to, *quantity),
                _ => return Some(Err(notify_error(messages::corporations::INVALID_SHARE_TRANSFER))),
            };
            shares::transfer(db, world, me, governance_bound, issuer, to, quantity, call.method == "MoveCompanyShares").await.map(|_| Value::None)
        }
        "PayoutDividend" => {
            if call.args.len() != 2 { return Some(Err(notify_error(messages::corporations::INVALID_DIVIDEND))); }
            match call.args.first() {
                Some(Value::Bool(true) | Value::Int(1)) => {}
                Some(Value::Bool(false) | Value::Int(0)) => return Some(Err(notify_error(messages::corporations::MEMBER_DIVIDEND_UNSUPPORTED))),
                _ => return Some(Err(notify_error(messages::corporations::INVALID_DIVIDEND))),
            }
            let amount = match call.args.get(1) {
                Some(Value::Float(value)) => *value,
                _ => return Some(Err(notify_error(messages::corporations::INVALID_DIVIDEND))),
            };
            dividends::payout(db, world, me, governance_bound, amount).await.map(|_| Value::None)
        }
        // UpdateCorporation(description, url, taxRate, isRecruiting, lpTaxRate)
        "UpdateCorporation" => {
            let text = |i: usize| call.args.get(i).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            let tax = call.args.get(2).and_then(|v| v.as_float()).unwrap_or(0.0).clamp(0.0, 1.0);
            let recruiting = call.args.get(3).is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int().is_some_and(|i| i != 0));
            let lp_tax = call.args.get(4).and_then(|v| v.as_float()).unwrap_or(0.0).clamp(0.0, 1.0);
            match membership(db, me).await {
                Ok((my_corp, roles)) if my_corp == corp && roles & ROLE_DIRECTOR != 0 => dispatch_dao::update_corporation(db, corp, &text(0), &text(1), tax, recruiting, lp_tax)
                    .await
                    .map(|_| Value::None)
                    .map_err(db_err),
                Ok(_) => Err(notify_error(messages::DIRECTOR_REQUIRED_TO_EDIT_CORP_INFO)),
                Err(e) => Err(e),
            }
        }
        "UpdateLogo" => {
            let s = |i: usize| call.args.get(i).and_then(|v| v.as_int()).map(|v| v as i32);
            dispatch_dao::update_logo(db, corp, [s(0), s(1), s(2)], [s(3), s(4), s(5)], s(6), me)
                .await
                .map(|_| Value::None)
                .map_err(db_err)
        }
        "GetCorpWelcomeMail" => match dispatch_dao::welcome_mail(db, corp).await {
            Ok(r) => Ok(Value::unicode(r.unwrap_or_default())),
            Err(e) => Err(db_err(e)),
        },
        "SetCorpWelcomeMail" => {
            let text = call.args.first().and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            dispatch_dao::set_welcome_mail(db, corp, &text, me)
                .await
                .map(|_| Value::None)
                .map_err(db_err)
        }
        _ => return None,
    })
}


/// `AddCorporation(name, ticker, description, url, taxRate, shape1..3, color1..3, typeface,
/// applicationsEnabled, friendlyFire, lpTaxRate)` -> corporationID.
async fn found(c: &Client, call: &Call<'_>) -> CallResult {
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).map(|v| v as i32);
    let input = FoundInput {
        name: trimmed_arg_text(call, 0),
        ticker: trimmed_arg_text(call, 1).to_uppercase(),
        description: trimmed_arg_text(call, 2),
        url: trimmed_arg_text(call, 3),
        tax: call.args.get(4).and_then(|v| v.as_float()).unwrap_or(0.0).clamp(0.0, 1.0),
        recruiting: call.args.get(12).and_then(|v| v.as_int()).unwrap_or(1) != 0,
        friendly: matches!(call.args.get(13), Some(Value::Bool(true))),
        shapes: [int(5), int(6), int(7), int(8), int(9), int(10), int(11)],
    };
    found_core(c, input, false).await.map(Value::Int)
}

struct FoundInput {
    name: String,
    ticker: String,
    description: String,
    url: String,
    tax: f64,
    recruiting: bool,
    friendly: bool,
    shapes: [Option<i32>; 7],
}

async fn found_core(c: &Client, input: FoundInput, gm: bool) -> Result<i64, CallError> {
    let (db, world) = (&c.db, &*c.world);
    let sde = c.sde.clone();
    let me = c.session.int("charid").unwrap_or(0);
    let FoundInput { name, ticker, description, url, tax, recruiting, friendly, shapes } = input;
    let Some(station) = c.session.int("stationid") else {
        return Err(notify_error(messages::STATION_DOCK_REQUIRED_TO_CREATE_CORP));
    };
    if !(ORG_NAME_MIN_LENGTH..=ORG_NAME_MAX_LENGTH).contains(&name.chars().count()) {
        return Err(notify_error(messages::CORP_NAME_LENGTH_INVALID));
    }
    if !(ORG_SHORT_NAME_MIN_LENGTH..=ORG_SHORT_NAME_MAX_LENGTH).contains(&ticker.chars().count()) || !ticker.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        return Err(notify_error(messages::CORP_SHORT_NAME_INVALID));
    }
    if !gm {
        let skill = crate::services::skills::effective::snapshot(db, me).await?.level(SKILL_CORPORATION_MANAGEMENT);
        if i64::from(skill) < crate::config::game().corporations.min_management_skill_level {
            return Err(notify_error(messages::CORPORATION_MANAGEMENT_SKILL_REQUIRED));
        }
    }
    let (current, _) = membership(db, me).await?;
    if is_player_corp(current) && ceo_of(db, current).await? == Some(me) {
        return Err(notify_error(messages::ALREADY_CORP_CEO_MUST_RESIGN_OR_CLOSE));
    }
    let result = registry::name_taken(db, &name, &ticker).await.map_err(db_err)?;
    let taken = result.taken || sde.npc_corporations.iter().any(|corp| {
        corp.lower_name.as_deref() == Some(result.lower_name.as_str())
            || corp.upper_ticker.as_deref() == Some(ticker.as_str())
    });
    if taken {
        return Err(notify_error(messages::CORP_NAME_OR_SHORT_NAME_TAKEN));
    }
    let _trading = super::super::market::lock().await;
    let now = filetime_now();
    let id = db.transaction(
        |tx, outbox| {
            let (name, ticker, description, url) = (name.clone(), ticker.clone(), description.clone(), url.clone());
            let sde = sde.clone();
            Box::pin(async move {
                if !gm {
                    super::super::wallets::debit(tx, outbox, me, STARTUP_COST).await?;
                }
                let id = registry::create(tx, &registry::NewCorporation {
                    name: &name, ticker: &ticker, description: &description, url: &url,
                    tax, ceo: me, station, recruiting, friendly, shapes, now,
                }).await.map_err(db_err)?;
                if !crate::dao::corporations::shares::initialize(tx, id, crate::config::game().corporations.initial_shares).await.map_err(db_err)? {
                    return Err(CallError::Internal(anyhow::anyhow!("new corporation share issuance could not be initialized")));
                }
                set_corp(tx, &sde, outbox, me, id, CEO_ROLES, now).await?;
                Ok(id)
            })
        },
        |id, outbox| async move { dispatch_world(id, outbox, world) },
    ).await?;
    info!("corporations: {me} founded {id} [{ticker}] {name}");
    Ok(id)
}

/// Found a corporation without the player skill or startup fee requirements.
pub(crate) async fn gm_found(c: &mut Client, name: &str, ticker: &str) -> Result<i64, CallError> {
    found_core(c, FoundInput {
        name: name.trim().to_owned(),
        ticker: ticker.trim().to_uppercase(),
        description: String::new(),
        url: String::new(),
        tax: 0.0,
        recruiting: true,
        friendly: false,
        shapes: [None; 7],
    }, true).await
}


fn application_row(r: &registry::Application) -> Value {
    keyval([
        ("applicationID", Value::Int(r.application_id)),
        ("corporationID", Value::Int(r.corporation_id)),
        ("characterID", Value::Int(r.character_id)),
        ("applicationText", Value::unicode(r.application_text.clone())),
        ("status", Value::Int(r.status as i64)),
        ("applicationDateTime", Value::Int(r.created)),
        ("customMessage", Value::unicode(r.custom_message.clone())),
    ])
}

/// Open applications grouped by `key` (corporation or character): {id: [application]}.
async fn applications(db: &Db, filter: registry::ApplicationFilter, arg: i64, by_corp: bool) -> CallResult {
    let rows = registry::applications(db, filter, arg, &OPEN_APPLICATION).await.map_err(db_err)?;
    let mut grouped: Vec<(i64, Vec<Value>)> = vec![];
    for r in &rows {
        let key = if by_corp { r.corporation_id } else { r.character_id };
        match grouped.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(application_row(r)),
            None => grouped.push((key, vec![application_row(r)])),
        }
    }
    Ok(Value::Dict(grouped.into_iter().map(|(k, v)| (Value::Int(k), Value::List(v))).collect()))
}

async fn old_applications(db: &Db, me: i64) -> CallResult {
    let rows = registry::old_applications(db, me, &OPEN_APPLICATION, OLD_APPLICATION_LIMIT).await.map_err(db_err)?;
    Ok(Value::list(rows.iter().map(application_row)))
}

/// `UpdateApplicationOffer(applicationID, characterID, corporationID, text, status, customMessage, date)`.
async fn update_application(c: &Client, call: &Call<'_>) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let sde = c.sde.clone();
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    let (app_id, status) = (int(0), int(4) as i32);
    let message = call.args.get(5).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    let Some(r) = registry::application(db, app_id).await.map_err(db_err)? else {
        return Err(notify_error(messages::ALLIANCE_APPLICATION_NOT_FOUND));
    };
    let (corp, applicant, current) = (r.corporation_id, r.character_id, r.status);
    if !OPEN_APPLICATION.contains(&current) {
        return Err(notify_error(messages::CORP_APPLICATION_ALREADY_PROCESSED));
    }
    let by_character = matches!(status, APP_ACCEPTED_BY_CHARACTER | APP_REJECTED_BY_CHARACTER | APP_WITHDRAWN_BY_CHARACTER | APP_RENEGOTIATED_BY_CHARACTER);
    if by_character && applicant != me {
        return Err(notify_error(messages::CORP_APPLICATION_NOT_OWNED));
    }
    if !by_character {
        let (my_corp, roles) = membership(db, me).await?;
        if my_corp != corp || roles & (ROLE_PERSONNEL_MANAGER | ROLE_DIRECTOR) == 0 {
            return Err(notify_error(messages::PERSONNEL_MANAGER_REQUIRED_FOR_APPLICATION));
        }
    }
    let joins = (status == APP_ACCEPTED_BY_CORPORATION && current != APP_INVITED_BY_CORPORATION)
        || (status == APP_ACCEPTED_BY_CHARACTER && current == APP_INVITED_BY_CORPORATION);
    let now = filetime_now();
    db.transaction(
        |tx, outbox| {
            let message = message.clone();
            let sde = sde.clone();
            Box::pin(async move {
                let affected = registry::update_application(tx, app_id, status, &message, current).await.map_err(db_err)?;
                if affected == 0 {
                    return Err(notify_error(messages::CORP_APPLICATION_ALREADY_PROCESSED));
                }
                if joins {
                    let (old, _) = membership(tx, applicant).await?;
                    if is_player_corp(old) && ceo_of(tx, old).await? == Some(applicant) {
                        return Err(notify_error(messages::CEO_MUST_LEAVE_CORP_BEFORE_JOINING));
                    }
                    set_corp(tx, &sde, outbox, applicant, corp, 0, now).await?;
                    registry::withdraw_applications(tx, applicant, app_id, APP_WITHDRAWN_BY_CHARACTER, &OPEN_APPLICATION).await.map_err(db_err)?;
                }
                Ok(())
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await?;
    Ok(Value::None)
}

/// A member row (bco_members, corp_ui_member_*).
fn member_row(r: &registry::Member, corp: i64) -> Value {
    let (roles, grantable) = (r.roles, r.grantable_roles);
    keyval([
        ("characterID", Value::Int(r.character_id)),
        ("corporationID", Value::Int(corp)),
        ("roles", Value::Int(roles)),
        ("grantableRoles", Value::Int(grantable)),
        ("rolesAtHQ", Value::Int(roles)),
        ("grantableRolesAtHQ", Value::Int(grantable)),
        ("rolesAtBase", Value::Int(roles)),
        ("grantableRolesAtBase", Value::Int(grantable)),
        ("rolesAtOther", Value::Int(roles)),
        ("grantableRolesAtOther", Value::Int(grantable)),
        ("baseID", Value::None),
        ("titleMask", Value::Int(r.title_mask)),
        ("title", Value::unicode(r.title.clone())),
        ("startDateTime", Value::Int(r.start_date)),
        ("divisionID", Value::Int(0)),
        ("squadronID", Value::Int(0)),
        ("blockRoles", Value::None),
        ("accountKey", Value::Int(ACCOUNT_CASH as i64)),
    ])
}


/// The corporation a bound corpRegistry is for (its bindParams), else the session's.
fn bound_corp(c: &Client, call: &Call<'_>) -> i64 {
    call.bind_params.and_then(|p| p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int()))).unwrap_or(c.session.int("corpid").unwrap_or(0))
}
