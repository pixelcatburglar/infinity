//! Contract listing mechanics for contractProxy, mirroring eve/client/script/ui/shared/neocom/contracts.
//! Uses game contracts and items plus immutable SDE stations and types; corporate inventory
//! access follows live membership and division view/take authority.
//! Search and owner listing limits use the configured contracts pagination policy.
//! `OnItemChange` stack quantities are projected to the client display limit.

use super::*;

async fn live_corp(c: &Client) -> Result<Option<i64>, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    if corp == 0 { return Ok(None); }
    Ok(crate::dao::characters::members::active_ids(&c.db, corp).await.map_err(db_err)?.contains(&me).then_some(corp))
}

pub(super) async fn selected_owner(c: &Client, for_corp: bool) -> Result<Option<i64>, CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    if !for_corp { return Ok(Some(me)); }
    Ok(live_corp(c).await?.filter(|corp| super::super::corps::is_player_corp(*corp)))
}

pub(super) async fn counts(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let corp = live_corp(c).await?.unwrap_or(-1);
    let counts = outstanding_counts(&c.db, me, corp).await?;
    Ok(keyval([
        ("myCharTotal", Value::Int(counts.my_char_total)),
        ("nonCorpForMyChar", Value::Int(counts.non_corp_for_my_char)),
        ("myCorpTotal", Value::Int(counts.my_corp_total)),
        ("nonCorpForMyCorp", Value::Int(counts.non_corp_for_my_corp)),
    ]))
}

// ---------------------------------------------------------------- lists


fn location_matches(location: Option<i64>, station: i64, ancestry: [Option<i64>; 3]) -> bool {
    location.is_none_or(|id| id == station || ancestry.contains(&Some(id)))
}

fn type_matches(type_id: i32, group: Option<i32>, category: Option<i32>, types: Option<&[i32]>, wanted_group: Option<i32>, wanted_category: Option<i32>) -> bool {
    if let Some(types) = types { types.contains(&type_id) }
    else if let Some(wanted) = wanted_group { group == Some(wanted) }
    else if let Some(wanted) = wanted_category { category == Some(wanted) }
    else { true }
}

/// `SearchContracts(**filters)` -> KeyVal(contracts, numFound, searchTime, maxResults).
pub(super) async fn search(c: &Client, kw: &Value) -> CallResult {
    let db = &c.db;
    let me = c.session.int("charid").unwrap_or(0);
    let corp = selected_owner(c, true).await?.unwrap_or(-1);
    let get = |k: &str| kw.get(k).filter(|v| !v.is_none());
    let int = |k: &str| get(k).and_then(|v| v.as_int());
    let float = |k: &str| get(k).and_then(|v| v.as_float());
    let assignee = match int("availability").unwrap_or(AVAIL_PUBLIC) {
        AVAIL_MYSELF => me, AVAIL_MY_CORP => corp, AVAIL_PUBLIC => 0, _ => -1,
    };
    let filters = query::Search {
        now: filetime_now(), assignee, kind: int("contractType").unwrap_or(CONTYPE_ALL),
        issuer: int("issuerID"),
        bounds: [float("minPrice"), float("maxPrice"), float("minReward"), float("maxReward"),
            float("minCollateral"), float("maxCollateral"), float("minVolume"), float("maxVolume")],
        description: get("description").and_then(|v| v.as_text()).filter(|t| !t.trim().is_empty()).map(|t| t.into_owned()),
        exclude_multiple: truthy(get("excludeMultiple")), exclude_trade: truthy(get("excludeTrade")),
        exclude_no_buyout: truthy(get("excludeNoBuyout")), sort: int("sortBy").unwrap_or(0),
        descending: int("sortDir").unwrap_or(0) != 0, outstanding: STATUS_OUTSTANDING,
        all: CONTYPE_ALL, combined: CONTYPE_AUCTION_AND_ITEM_EXCHANGE, exchange: TYPE_ITEM_EXCHANGE,
        auction: TYPE_AUCTION,
    };
    let (mut found, started) = query::search(db, filters).await.map_err(db_err)?;
    let matches_location = |id: i64, location: Option<i64>| {
        let station = c.sde.map.station(id);
        location_matches(location, id, [
            station.and_then(|s| s.solar_system_id),
            station.and_then(|s| s.constellation_id),
            station.and_then(|s| s.region_id),
        ])
    };
    found.retain(|k| matches_location(k.start, int("locationID")) && matches_location(k.end, int("endLocationID")));
    let item_types = get("itemTypes").map(id_list);
    let group = int("itemGroupID").map(|v| v as i32);
    let category = int("itemCategoryID").map(|v| v as i32);
    if item_types.is_some() || group.is_some() || category.is_some() {
        let ids: Vec<_> = found.iter().map(|k| k.id).collect();
        let matching: std::collections::HashSet<i64> = shared::items(db, &ids).await.map_err(db_err)?.into_iter()
            .filter(|item| item.in_crate && c.sde.types.get(item.type_id as i32).is_some_and(|t|
                type_matches(item.type_id as i32, t.group_id, t.category_id, item_types.as_deref(), group, category)))
            .map(|item| item.contract_id).collect();
        found.retain(|k| matching.contains(&k.id));
    }
    let policy = &crate::config::game().contracts;
    found.truncate(usize::try_from(policy.page_size_max).unwrap_or(usize::MAX));
    let start = usize::try_from(int("startNum").unwrap_or(0).max(0)).unwrap_or(usize::MAX).min(found.len());
    let page = &found[start..start.saturating_add(usize::try_from(policy.page_size).unwrap_or(usize::MAX)).min(found.len())];
    Ok(keyval([
        ("contracts", Value::List(bundles(db, &c.sde, page).await?)),
        ("numFound", Value::Int(found.len() as i64)),
        ("searchTime", Value::Int(started.elapsed().as_millis() as i64)),
        ("maxResults", Value::Int(policy.page_size_max)),
    ]))
}

/// `GetContractListForOwner(ownerID, status, contractType, issuedBy, num=, startContractID=)`.
pub(super) async fn list_for_owner(c: &Client, call: &Call<'_>) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let corp = selected_owner(c, true).await?;
    let arg_int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let owner = arg_int(0).unwrap_or(0);
    if owner != me && Some(owner) != corp {
        return listing(&c.db, &c.sde, &[]).await;
    }
    let filters = query::Owner {
        me: owner, issued_by: match call.args.get(3) { Some(Value::Bool(v)) => Some(*v), _ => None },
        status: arg_int(1).unwrap_or(STATUS_OUTSTANDING as i64) as i32, kind: arg_int(2),
        before: call.kwargs.get("startContractID").and_then(|v| v.as_int()),
        num: call.kwargs.get("num").and_then(|v| v.as_int()).unwrap_or(crate::config::game().contracts.page_size).clamp(1, crate::config::game().contracts.page_size_max),
        outstanding: STATUS_OUTSTANDING, in_progress: STATUS_IN_PROGRESS, finished: STATUS_FINISHED,
        all: CONTYPE_ALL, combined: CONTYPE_AUCTION_AND_ITEM_EXCHANGE, exchange: TYPE_ITEM_EXCHANGE, auction: TYPE_AUCTION,
    };
    let list = query::owner(&c.db, filters).await.map_err(db_err)?;
    listing(&c.db, &c.sde, &list).await
}

/// Contracts that need the character: their own expired or rejected ones, and courier
/// contracts overdue on either side.
pub(super) async fn needing_attention(db: &Db, me: i64) -> Result<Vec<Contract>, CallError> {
    query::list(db, Filter::Attention(me, filetime_now(), STATUS_OUTSTANDING, STATUS_REJECTED, STATUS_IN_PROGRESS, FILETIME_DAY)).await.map_err(db_err)
}

pub(super) async fn bidding_on(db: &Db, me: i64) -> Result<Vec<Contract>, CallError> {
    query::list(db, Filter::Bidding(me, STATUS_OUTSTANDING)).await.map_err(db_err)
}

pub(super) async fn in_progress(db: &Db, me: i64) -> Result<Vec<Contract>, CallError> {
    query::list(db, Filter::InProgress(me, STATUS_IN_PROGRESS)).await.map_err(db_err)
}

pub(super) async fn assigned_to(db: &Db, me: i64, corp: i64) -> Result<Vec<Contract>, CallError> {
    query::list(db, Filter::Assigned(me, corp, STATUS_OUTSTANDING, filetime_now())).await.map_err(db_err)
}

/// `CollectMyPageInfo`: the counts on the contracts start page.
pub(super) async fn page_info(c: &Client) -> CallResult {
    let db = &c.db;
    let me = c.session.int("charid").unwrap_or(0);
    let corp = selected_owner(c, true).await?.unwrap_or(0);
    let open = outstanding_counts(db, me, live_corp(c).await?.unwrap_or(-1)).await?;
    let assigned = assigned_to(db, me, corp).await?;
    Ok(keyval([
        ("numOutstandingContracts", Value::Int(open.my_char_total)),
        ("numOutstandingContractsNonCorp", Value::Int(open.non_corp_for_my_char)),
        ("numOutstandingContractsForCorp", Value::Int(open.non_corp_for_my_corp)),
        (
            "outstandingContracts",
            Value::list(assigned.iter().map(|k| Value::list([Value::Int(k.issuer), Value::Int(k.issuer_corp), Value::Int(k.assignee), Value::Int(k.kind as i64)]))),
        ),
        ("numRequiresAttention", Value::Int(needing_attention(db, me).await?.len() as i64)),
        ("numRequiresAttentionCorp", Value::Int(if corp != 0 { needing_attention(db, corp).await?.len() as i64 } else { 0 })),
        ("numBiddingOn", Value::Int(bidding_on(db, me).await?.len() as i64)),
        ("numBiddingOnCorp", Value::Int(if corp != 0 { bidding_on(db, corp).await?.len() as i64 } else { 0 })),
        ("numInProgress", Value::Int(in_progress(db, me).await?.len() as i64)),
        ("numInProgressCorp", Value::Int(if corp != 0 { in_progress(db, corp).await?.len() as i64 } else { 0 })),
    ]))
}

/// `GetLoginInfo`: what the neocom blinks for after login.
pub(super) async fn login_info(c: &Client) -> CallResult {
    let db = &c.db;
    let me = c.session.int("charid").unwrap_or(0);
    let corp = selected_owner(c, true).await?.unwrap_or(0);
    let attention = DbRowDescriptor::new([("contractID", DbType::I8), ("forCorp", DbType::Bool)]);
    let mut seen = std::collections::HashSet::new();
    let mut needs = Vec::new();
    let mut progress = Vec::new();
    for (owner, for_corp) in [(corp, true), (me, false)] {
        if owner == 0 { continue; }
        for k in needing_attention(db, owner).await? {
            if seen.insert(k.id) { needs.push(attention.row(&[Value::Int(k.id), Value::Bool(for_corp)])); }
        }
        for k in in_progress(db, owner).await? {
            if k.kind == TYPE_COURIER && !progress.iter().any(|(id, _)| *id == k.id) { progress.push((k.id, k.keyval(&c.sde))); }
        }
    }
    Ok(keyval([
        ("assignedToMe", Value::list(assigned_to(db, me, corp).await?.iter().map(|k| k.keyval(&c.sde)))),
        ("needsAttention", Value::List(needs)),
        ("inProgress", Value::list(progress.into_iter().map(|(_, value)| value))),
    ]))
}

/// `GetMyContractEscrow`: ISK held by the character's contracts and top bids, and the
/// retail value of their escrowed items.
pub(super) async fn escrow(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let r = crate::dao::contracts::reports::escrow(&c.db, me, TYPE_AUCTION, STATUS_OUTSTANDING, STATUS_IN_PROGRESS).await.map_err(db_err)?;
    Ok(keyval([("iskEscrow", Value::Float(r.isk)), ("itemsEscrow", Value::Float(r.items))]))
}

/// Resolve the selected station custody without requiring take authority for reads.
async fn picker_root(ex: &(impl Executor + ?Sized), sde: &Sde, me: i64, owner: i64, station: i64, for_corp: bool, flag: i32, take: bool) -> Result<Option<(i64, i32)>, CallError> {
    if sde.map.station(station).is_none() { return Ok(None); }
    if !for_corp { return Ok(Some((station, FLAG_HANGAR))); }
    let flag = if flag == FLAG_HANGAR { infinity_core::consts::FLAG_CORP_SAG1 } else { flag };
    let allowed = if take { super::super::inventory::access::may_take_corp_hangar(ex, me, owner, station, flag).await? } else { super::super::inventory::access::may_view_corp_hangar(ex, me, owner, station, flag).await? };
    if !allowed { return Ok(None); }
    let location = if flag == infinity_core::consts::FLAG_CORP_DELIVERIES { Some(station) } else { super::super::offices::office_at(ex, owner, station).await? };
    Ok(location.map(|location| (location, flag)))
}

async fn picker_container(ex: &(impl Executor + ?Sized), sde: &Sde, id: i64, owner: i64, root: (i64, i32), for_corp: bool) -> Result<bool, CallError> {
    let ancestors = crate::dao::items::deliveries::ancestry(ex, id).await.map_err(db_err)?;
    if ancestors.is_empty() || ancestors.iter().any(|row| row.owner_id != owner) { return Ok(false); }
    if for_corp {
        Ok(super::super::inventory::access::corporate_custody(ex, Some(sde), id).await? == Some((owner, root.0, root.1)))
    } else {
        Ok(ancestors.iter().any(|row| row.location_id == root.0 && row.flag_id == root.1))
    }
}

pub(super) async fn items_in(c: &Client, station: i64, container: Option<i64>, for_corp: bool, flag: i32) -> CallResult {
    let Some(owner) = selected_owner(c, for_corp).await? else { return Ok(Value::List(vec![])); };
    let me = c.session.int("charid").unwrap_or(0);
    if container.is_none() && for_corp {
        let mut items = Vec::new();
        for division in (infinity_core::consts::FLAG_CORP_SAG1..infinity_core::consts::FLAG_CORP_SAG1 + infinity_core::consts::CORP_HANGAR_DIVISIONS).chain(std::iter::once(infinity_core::consts::FLAG_CORP_DELIVERIES)) {
            if let Some(root) = picker_root(&c.db, &c.sde, me, owner, station, true, division, false).await? {
                items.extend(load_items(&c.db, &c.sde, ItemQuery::OwnerLocationFlag(owner, root.0, root.1)).await.map_err(db_err)?);
            }
        }
        return Ok(Value::list(items.iter().map(Item::row)));
    }
    let Some(root) = picker_root(&c.db, &c.sde, me, owner, station, for_corp, flag, false).await? else { return Ok(Value::List(vec![])); };
    let items = if let Some(id) = container {
        if !picker_container(&c.db, &c.sde, id, owner, root, for_corp).await? { return Ok(Value::List(vec![])); }
        load_items(&c.db, &c.sde, ItemQuery::OwnerLocation(owner, id)).await.map_err(db_err)?
    } else {
        load_items(&c.db, &c.sde, ItemQuery::OwnerLocationFlag(owner, root.0, root.1)).await.map_err(db_err)?
    };
    Ok(Value::list(items.iter().map(Item::row)))
}

pub(super) async fn container_counts(c: &Client, station: i64, ids: &[i64], for_corp: bool, flag: i32) -> CallResult {
    let Some(owner) = selected_owner(c, for_corp).await? else { return Ok(Value::Dict(vec![])); };
    let me = c.session.int("charid").unwrap_or(0);
    let Some(root) = picker_root(&c.db, &c.sde, me, owner, station, for_corp, flag, false).await? else { return Ok(Value::Dict(vec![])); };
    let mut authorized = Vec::new();
    for &id in ids {
        if picker_container(&c.db, &c.sde, id, owner, root, for_corp).await? { authorized.push(id); }
    }
    let rows = crate::dao::contracts::reports::container_counts(&c.db, &authorized, owner).await.map_err(db_err)?;
    Ok(Value::Dict(rows.iter().map(|r| (Value::Int(r.location_id), Value::Int(r.count))).collect()))
}

/// Split an authorized root stack in the selected custody.
pub(super) async fn split_stack(c: &Client, station_id: i64, item_id: i64, quantity: i64, for_corp: bool, flag: i32) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let sde = &c.sde;
    let me = c.session.int("charid").unwrap_or(0);
    let Some(owner) = selected_owner(c, for_corp).await? else { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); };
    let _trading = lock().await;
    db.transaction(
        |db, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
            let sde = &*sde;
            if for_corp && !crate::dao::characters::members::active_ids(db, owner).await.map_err(db_err)?.contains(&me) {
                return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION));
            }
            let root = picker_root(db, sde, me, owner, station_id, for_corp, flag, true).await?.ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
            let item = load_items(db, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.pop().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
            if item.owner_id != owner || item.location_id != root.0 || item.flag_id != root.1 || item.singleton || quantity < 1 || quantity >= item.quantity {
                return Err(notify_error(messages::STACK_CANNOT_BE_SPLIT));
            }
            let id = super::super::inventory::stacks::split_stack(db, &item, quantity, owner, root.0, root.1, false).await?;
            let old_quantity = item.quantity;
            let left = Item { quantity: old_quantity - quantity, ..item };
            let change = vec![(IX_STACK_SIZE, Value::Int(old_quantity as i64))];
            if for_corp {
                super::super::inventory::notifications::notify_corp_item_change(db, outbox, &left, change).await?;
                let new_item = load_items(db, sde, ItemQuery::Id(id)).await.map_err(db_err)?.pop().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                super::super::inventory::notifications::notify_corp_item_change(db, outbox, &new_item, vec![(IX_LOCATION_ID, Value::Int(LOCATION_JUNKYARD))]).await?;
            } else {
                outbox.notify_pilot(me, "OnItemChange", vec![left.row(), super::super::inventory::notifications::item_change_dict(change), Value::None]);
                if crate::dao::items::transfers::docked(db, me).await.map_err(db_err)? == Some(station_id) {
                    let new_item = load_items(db, sde, ItemQuery::Id(id)).await.map_err(db_err)?.pop().ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?;
                    outbox.notify_pilot(me, "OnItemChange", vec![new_item.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(LOCATION_JUNKYARD))]), Value::None]);
                }
            }
            Ok(Value::Int(id))
        })},
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await
}

#[cfg(test)] mod tests;

