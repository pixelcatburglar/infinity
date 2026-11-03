//! Corporation role, title and leadership mechanics for corpRegistry.
//! Mirrors eve/client/script/ui/services/corporation, using game corporations,
//! characters, skills and titles, and startup SDE schools and NPC corporations. CEO eligibility
//! uses effective management skill; shares remain unsupported.

use super::*;
use crate::dao::corporations::roles as role_dao;

// ---------------------------------------------------------------- roles and titles

/// The lines of a `Rowset` argument (UpdateMembers, UpdateTitles).
pub(super) fn rowset_lines(v: Option<&Value>) -> Vec<Vec<Value>> {
    let lines = match v {
        Some(Value::Instance(_, state)) => state.get("lines").cloned(),
        Some(other) => other.get("lines").cloned().or_else(|| Some(other.clone())),
        None => None,
    };
    lines.and_then(|l| l.items().map(|it| it.iter().filter_map(|line| line.items().map(|x| x.to_vec())).collect())).unwrap_or_default()
}

/// `UpdateMember(characterID, title, divisionID, squadronID, roles, grantableRoles, rolesAtHQ,
/// grantableRolesAtHQ, rolesAtBase, grantableRolesAtBase, rolesAtOther, grantableRolesAtOther,
/// baseID, titleMask, blockRoles)`: None leaves a field as it is. Roles are one mask here, so
/// the per-location ones are folded into it. Directors edit anyone but the CEO.
pub(super) async fn update_member(db: &Db, world: &World, me: i64, args: &[Value]) -> Result<(), CallError> {
    let int = |i: usize| args.get(i).and_then(|v| v.as_int());
    let target = int(0).unwrap_or(0);
    let title = args.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned());
    let roles = int(4).map(|r| r | int(6).unwrap_or(0) | int(8).unwrap_or(0) | int(10).unwrap_or(0));
    let grantable = int(5).map(|g| g | int(7).unwrap_or(0) | int(9).unwrap_or(0) | int(11).unwrap_or(0));
    let title_mask = int(13);
    if roles.is_none() && grantable.is_none() && title.is_none() && title_mask.is_none() {
        return Ok(()); // e.g. only blockRoles, which isn't modelled
    }
    let (my_corp, my_roles) = membership(db, me).await?;
    let (their_corp, _) = membership(db, target).await?;
    let ceo = ceo_of(db, my_corp).await?;
    let is_ceo = ceo == Some(me);
    if my_corp != their_corp || (my_roles & ROLE_DIRECTOR == 0 && !is_ceo) || (ceo == Some(target) && !is_ceo) {
        return Err(notify_error(messages::CANNOT_MODIFY_MEMBER_ROLES));
    }
    // A CEO keeps every role.
    let roles = if ceo == Some(target) { Some(CEO_ROLES) } else { roles };
    role_dao::update_member(db, target, roles, grantable, title, title_mask).await.map_err(db_err)?;
    refresh_roles(db, world, target).await
}

/// `ExecuteActions(targetIDs, [(verb, property, value)])`: appConst.CTV_ADD/REMOVE/SET on
/// roles, grantable roles or titles of several members (bases aren't modelled).
pub(super) async fn execute_actions(db: &Db, world: &World, me: i64, call: &Call<'_>) -> Result<(), CallError> {
    let targets = value_id_list(call.args.first());
    let actions = call.args.get(1).and_then(|v| v.items()).unwrap_or(&[]);
    let (my_corp, my_roles) = membership(db, me).await?;
    let ceo = ceo_of(db, my_corp).await?;
    if my_roles & ROLE_DIRECTOR == 0 && ceo != Some(me) {
        return Err(notify_error(messages::DIRECTOR_REQUIRED_FOR_BULK_MEMBER_EDIT));
    }
    for target in targets {
        let (their_corp, _) = membership(db, target).await?;
        if their_corp != my_corp || ceo == Some(target) {
            continue;
        }
        for action in actions {
            let Some([verb, property, value]) = action.items() else { continue };
            let column = match property.as_text().as_deref() {
                Some("roles") => role_dao::Field::Roles,
                Some("grantableRoles") => role_dao::Field::Grantable,
                Some("titleMask") => role_dao::Field::TitleMask,
                _ => continue,
            };
            let value = value.as_int().unwrap_or(0);
            let action = match verb.as_int() {
                Some(CTV_ADD) => role_dao::Action::Add,
                Some(CTV_REMOVE) => role_dao::Action::Remove,
                Some(CTV_SET) => role_dao::Action::Set,
                _ => continue,
            };
            role_dao::action(db, target, column, action, value).await.map_err(db_err)?;
        }
        refresh_roles(db, world, target).await?;
    }
    Ok(())
}

const TITLE_HEADER: &[&str] = &[
    "titleID",
    "titleName",
    "roles",
    "grantableRoles",
    "rolesAtHQ",
    "grantableRolesAtHQ",
    "rolesAtBase",
    "grantableRolesAtBase",
    "rolesAtOther",
    "grantableRolesAtOther",
];

/// {titleID: Row} for all 16 title bits; unnamed ones the client calls "Untitled n".
pub(super) async fn titles(db: &Db, corp: i64) -> CallResult {
    let rows = role_dao::titles(db, corp).await.map_err(db_err)?;
    let header = Value::list(TITLE_HEADER.iter().map(|h| Value::str(*h)));
    Ok(Value::Dict(
        (0..TITLE_COUNT)
            .map(|bit| {
                let id = 1i64 << bit;
                let line: Vec<Value> = match rows.iter().find(|r| r.title_id == id) {
                    Some(r) => std::iter::once(Value::Int(id))
                        .chain(std::iter::once(Value::unicode(r.name.clone())))
                        .chain([r.roles, r.grantable_roles, r.roles_hq, r.grantable_hq, r.roles_base, r.grantable_base, r.roles_other, r.grantable_other].into_iter().map(Value::Int))
                        .collect(),
                    None => std::iter::once(Value::Int(id)).chain(std::iter::once(Value::unicode(""))).chain((2..10).map(|_| Value::Int(0))).collect(),
                };
                (Value::Int(id), row(header.clone(), line))
            })
            .collect(),
    ))
}

/// Online members holding any of `title_mask`'s titles get their roles again.
async fn refresh_title_holders(db: &Db, world: &World, corp: i64, title_mask: i64) -> Result<(), CallError> {
    let rows = role_dao::holders(db, corp, title_mask, true).await.map_err(db_err)?;
    for r in rows {
        refresh_roles(db, world, r).await?;
    }
    Ok(())
}

/// Store title lines `(titleID, titleName, roles, grantableRoles, rolesAtHQ, ...)`; only
/// directors may, and no title carries the Director role.
pub(super) async fn update_titles(db: &Db, world: &World, me: i64, corp: i64, lines: Vec<Vec<Value>>) -> Result<(), CallError> {
    let (my_corp, roles) = membership(db, me).await?;
    if my_corp != corp || roles & ROLE_DIRECTOR == 0 {
        return Err(notify_error(messages::DIRECTOR_REQUIRED_TO_EDIT_TITLES));
    }
    let mut changed = 0i64;
    let mut titles = Vec::new();
    for line in lines {
        let int = |i: usize| line.get(i).and_then(|v| v.as_int()).unwrap_or(0) & !ROLE_DIRECTOR;
        let id = line.first().and_then(|v| v.as_int()).unwrap_or(0);
        if id <= 0 || id.count_ones() != 1 || id > 1 << (TITLE_COUNT - 1) {
            continue;
        }
        let name = line.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
        titles.push(role_dao::Title { title_id: id, name, roles: int(2), grantable_roles: int(3), roles_hq: int(4), grantable_hq: int(5), roles_base: int(6), grantable_base: int(7), roles_other: int(8), grantable_other: int(9) });
        changed |= id;
    }
    role_dao::save_titles(db, corp, titles).await.map_err(db_err)?;
    refresh_title_holders(db, world, corp, changed).await
}

pub(super) async fn delete_title(db: &Db, world: &World, me: i64, corp: i64, title: i64) -> Result<(), CallError> {
    let (my_corp, roles) = membership(db, me).await?;
    if my_corp != corp || roles & ROLE_DIRECTOR == 0 {
        return Err(notify_error(messages::DIRECTOR_REQUIRED_TO_DELETE_TITLES));
    }
    let holders = role_dao::holders(db, corp, title, false).await.map_err(db_err)?;
    role_dao::delete_title(db, corp, title).await.map_err(db_err)?;
    for r in holders {
        refresh_roles(db, world, r).await?;
    }
    Ok(())
}

/// Members other than the CEO with Corporation Management, who could take over.
pub(super) async fn potential_ceos(db: &Db, corp: i64, ceo: i64) -> Result<Vec<i64>, CallError> {
    let candidates = role_dao::potential_ceos(db, corp, ceo, SKILL_CORPORATION_MANAGEMENT).await.map_err(db_err)?;
    let minimum = crate::config::game().corporations.min_management_skill_level;
    Ok(candidates.into_iter()
        .filter(|candidate| i64::from(crate::services::skills::effective::from_trained(candidate.skill.as_slice()).level(SKILL_CORPORATION_MANAGEMENT)) >= minimum)
        .map(|candidate| candidate.character_id)
        .collect())
}

/// `ResignFromCEO(newCeoID=None)`. Without a successor it returns the members who could be
/// one for the CEO to pick from; a CEO alone closes the corporation, and a CEO without
/// eligible successors closes it and sends everyone back to their school corporations.
pub(super) async fn resign_ceo(db: &Db, sde: &Arc<Sde>, world: &World, me: i64, successor: Option<i64>) -> CallResult {
    let (corp, _) = membership(db, me).await?;
    if !is_player_corp(corp) || ceo_of(db, corp).await? != Some(me) {
        return Err(CallError::User { msg: messages::MSG_ONLY_CEO_CAN_RESIGN.into(), dict: vec![] });
    }
    match successor {
        Some(next) => {
            if !potential_ceos(db, corp, me).await?.contains(&next) {
                return Err(notify_error(messages::MEMBER_CANNOT_BECOME_CEO));
            }
            db.transaction(
                |tx, outbox: &mut Outbox| Box::pin(async move {
                    role_dao::transfer_ceo(tx, corp, next, [(next, CEO_ROLES), (me, ROLE_DIRECTOR)]).await.map_err(db_err)?;
                    for char_id in [next, me] {
                        let (_, roles) = membership(tx, char_id).await?;
                        let pair = |k: &str| Value::tuple([Value::str(k), Value::Int(roles)]);
                        outbox.notify_pilot(char_id, "__session", vec![pair("corprole"), pair("rolesAtAll"), pair("rolesAtBase"), pair("rolesAtHQ"), pair("rolesAtOther")]);
                    }
                    Ok(())
                }),
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await?;
            info!("corporations: {me} handed {corp} to {next}");
            Ok(Value::None)
        }
        None => {
            let others: Vec<i64> = members(db, corp, registry::MemberFilter::Exclude, &[me]).await?.iter().map(|r| r.character_id).collect();
            if others.is_empty() {
                leave(db, sde, world, me).await?;
                return Ok(Value::None);
            }
            let candidates = potential_ceos(db, corp, me).await?;
            if !candidates.is_empty() {
                return Ok(Value::list(candidates.into_iter().map(Value::Int)));
            }
            let now = filetime_now();
            let sde = sde.clone();
            db.transaction(
                |tx, outbox| {
                    let others = others.clone();
                    let sde = sde.clone();
                    Box::pin(async move {
                        for other in others {
                            let school = school_corp(tx, &sde, other).await?;
                            set_corp(tx, &sde, outbox, other, school, 0, now).await?;
                        }
                        leave_inner(tx, &sde, outbox, me, now).await
                    })
                },
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await?;
            Ok(Value::None)
        }
    }
}

/// KeyVal(corpID, allianceID, title, title1..title16) for `char_id`: the names of the titles
/// they hold (None for the others).
pub(super) async fn info_window_data(db: &Db, char_id: i64) -> CallResult {
    let Some(r) = role_dao::info(db, char_id).await.map_err(db_err)?
    else {
        return Ok(Value::None);
    };
    let (corp, mask) = (r.corporation_id, r.corp_title_mask);
    let names = role_dao::title_names(db, corp).await.map_err(db_err)?;
    let mut fields: Vec<(String, Value)> = vec![
        ("corpID".into(), Value::Int(corp)),
        ("allianceID".into(), r.alliance_id.map(Value::Int).unwrap_or(Value::None)),
        ("title".into(), Value::unicode(r.corp_title)),
    ];
    for bit in 0..TITLE_COUNT {
        let id = 1i64 << bit;
        let held = mask & id != 0;
        let name = names.iter().find(|n| n.title_id == id).map(|n| n.name.clone()).unwrap_or_default();
        fields.push((format!("title{}", bit + 1), if held { Value::unicode(name) } else { Value::None }));
    }
    Ok(keyval(fields))
}

