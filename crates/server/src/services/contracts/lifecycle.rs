//! Contract lifecycle mechanics for contractProxy, mirroring eve/client/script/ui/shared/neocom/contracts.
//! Uses game contracts and items through database access and immutable SDE types and stations.
//! Requested exchange items retain their identity, state and contents when handed over.
//! Expiry, courier duration, item counts and text lengths use the configured contracts policy.
//! Status transitions are guarded; overdue couriers are completed or failed manually.
//! Auction candidates are rechecked in their settlement transaction.

use super::*;
use crate::dao::contracts::lifecycle as contract_dao;

/// `CreateContract(ContractStruct, confirm)` -> [contractID]: one contract, or one per item
/// for a multi-contract (fitted ships).
pub(super) async fn create(c: &Client, info: &Value) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let sde = Arc::clone(&c.sde);
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let int = |k: &str| field(info, k).and_then(|v| v.as_int()).unwrap_or(0);
    let money = |k: &str| round2(field(info, k).and_then(|v| v.as_float()).unwrap_or(0.0));
    let text = |k: &str, max: usize| value_text(field(info, k), max);
    let for_corp = truthy(field(info, "forCorp"));
    let wallet_key = if for_corp {
        i32::try_from(c.session.int("corpAccountKey").unwrap_or(0)).map_err(|_| notify_error(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED))?
    } else { ACCOUNT_CASH };
    let owner = ownership::actor_owner(me, corp, for_corp, wallet_key);
    let source_flag = i32::try_from(int("startStationDivision")).unwrap_or(0);
    let kind = int("contractType") as i32;
    if ![TYPE_ITEM_EXCHANGE, TYPE_AUCTION, TYPE_COURIER].contains(&kind) {
        return Err(notify_error(messages::CONTRACT_TYPE_REQUIRED));
    }
    let (price, reward, collateral) = (money("price"), money("reward"), money("collateral"));
    if !price.is_finite() || !reward.is_finite() || !collateral.is_finite() || price < 0.0 || reward < 0.0 || collateral < 0.0 {
        return Err(notify_error(messages::CONTRACT_AMOUNT_NEGATIVE));
    }
    let start = station(&c.sde, int("startStationID"))?;
    let end = if kind == TYPE_COURIER { Some(station(&c.sde, int("destinationID"))?) } else { None };
    let assignee = int("assignedToID");
    if assignee == owner.id {
        return Err(notify_error(messages::CONTRACT_SELF_ASSIGNMENT_FORBIDDEN));
    }
    let policy = &crate::config::game().contracts;
    let minutes = i128::from(int("minutesExpire")).clamp(
        i128::from(policy.expiry_min_minutes),
        i128::from(policy.expiry_max_days) * i128::from(FILETIME_DAY / FILETIME_MINUTE),
    );
    let num_days = if kind == TYPE_COURIER {
        i32::try_from(int("numDays").clamp(policy.courier_min_days, policy.courier_max_days)).unwrap_or(i32::MAX)
    } else { 0 };
    let (title, description) = (
        text("title", usize::try_from(policy.title_max_len).unwrap_or(usize::MAX)),
        text("description", usize::try_from(policy.description_max_len).unwrap_or(usize::MAX)),
    );
    let mut items: Vec<(i64, i64)> = field(info, "itemList")
        .and_then(|v| v.items())
        .unwrap_or(&[])
        .iter()
        .filter_map(|e| match e.items()? {
            [id, qty, ..] => Some((id.as_int()?, qty.as_int().unwrap_or(1))),
            _ => None,
        })
        .collect();
    let mut requested = Vec::new();
    if kind == TYPE_ITEM_EXCHANGE {
        if let Some(value) = field(info, "requestItemTypeList") {
            let entries = value.items().ok_or_else(|| notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING))?;
            for entry in entries {
                let (type_id, qty) = match entry.items() {
                    Some([t, q, ..]) => (t.as_int(), q.as_int()),
                    _ => (None, None),
                };
                let type_id = type_id.and_then(|v| i32::try_from(v).ok()).filter(|v| *v > 0)
                    .ok_or_else(|| notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING))?;
                let qty = qty.and_then(|v| i32::try_from(v).ok()).filter(|v| *v > 0)
                    .ok_or_else(|| notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING))?;
                if sde.types.get(type_id).is_none() || requested.iter().any(|(t, _)| *t == type_id) {
                    return Err(notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING));
                }
                requested.push((type_id, qty));
            }
        }
    }
    match kind {
        TYPE_AUCTION if items.is_empty() => return Err(notify_error(messages::AUCTION_REQUIRES_ITEMS)),
        TYPE_AUCTION if collateral > 0.0 && collateral < price => return Err(notify_error(messages::BUYOUT_BELOW_START_PRICE)),
        TYPE_COURIER if items.is_empty() => return Err(notify_error(messages::COURIER_REQUIRES_ITEMS)),
        TYPE_ITEM_EXCHANGE if items.is_empty() && requested.is_empty() => return Err(notify_error(messages::ITEM_EXCHANGE_REQUIRES_OFFER_OR_REQUEST)),
        _ => {}
    }
    if items.len() as u128 > policy.max_items as u128 || requested.len() as u128 > policy.max_items as u128 {
        return Err(notify_error(messages::CONTRACT_ITEM_LIMIT_EXCEEDED));
    }
    let multi = truthy(field(info, "multiContract")) && items.len() > 1;
    items.sort_unstable_by_key(|(id, _)| *id);
    let groups: Vec<Vec<(i64, i64)>> = if multi { items.iter().map(|i| vec![*i]).collect() } else { vec![items] };

    let _trading = lock().await;
    let now = filetime_now();
    let start_id = start.id;
    let end_id = end.map(|s| s.id).unwrap_or(0);
    let input = Arc::new((groups, requested, title, description));
    let created = db.transaction(
        |db, outbox: &mut Outbox| {
            let input = Arc::clone(&input);
            let sde = Arc::clone(&sde);
            Box::pin(async move {
    let (groups, requested, title, description) = &*input;
    if for_corp { ownership::authorize_corporate_action(db, me, corp, wallet_key).await?; }
    let source = custody::issuer_source(db, me, corp, for_corp, start_id, source_flag).await?;
    let counts = shared::outstanding_counts(db, me, corp, STATUS_OUTSTANDING).await.map_err(db_err)?;
    let internal = assignee != 0 && (assignee == corp || crate::dao::characters::members::active_ids(db, corp).await.map_err(db_err)?.contains(&assignee));
    let open = match (for_corp, internal) {
        (true, true) => counts.my_corp_total,
        (true, false) => counts.non_corp_for_my_corp,
        (false, true) => counts.my_char_total,
        (false, false) => counts.non_corp_for_my_char,
    };
    let cap = if internal { MAX_INTERNAL_CONTRACTS } else { max_outstanding(db, me, for_corp).await? };
    if open + groups.len() as i64 > cap {
        return Err(notify_error(&messages::contract_limit(open, cap)));
    }
    // Validate the captured source before any escrow mutation.
    let mut loaded = HashMap::new();
    for (id, qty) in groups.iter().flatten() {
        let item = load_items(db, &sde, ItemQuery::Id(*id)).await.map_err(db_err)?.pop().ok_or_else(|| notify_error(messages::CONTRACT_ITEM_MISSING))?;
        if item.owner_id != source.owner || item.location_id != source.location || item.flag_id != source.flag {
            return Err(notify_error(messages::CONTRACT_ITEM_MUST_BE_IN_START_HANGAR));
        }
        if item.type_id == TYPE_PLASTIC_WRAP && kind != TYPE_COURIER {
            return Err(notify_error(messages::COURIER_PACKAGE_NOT_TRADEABLE));
        }
        if *qty < 1 || (!item.singleton && *qty > item.quantity as i64) {
            return Err(notify_error(messages::CONTRACT_ITEM_QUANTITY_INSUFFICIENT));
        }
        if loaded.insert(*id, item).is_some() {
            return Err(notify_error(messages::CONTRACT_ITEM_QUANTITY_INSUFFICIENT));
        }
    }
    let roots: Vec<_> = loaded.keys().copied().collect();
    let tree = crate::dao::trades::descendants(db, &roots).await.map_err(db_err)?;
    if crate::dao::industry::has_active_blueprint_job(db, &tree).await.map_err(db_err)? {
        return Err(notify_error(messages::industry::BLUEPRINT_ACTIVE));
    }
    let escrow_isk = if kind == TYPE_AUCTION { 0.0 } else { reward };
    let skill_rows = crate::dao::skills::levels::levels_of(db, me, &[crate::consts::SKILL_ACCOUNTING, crate::consts::SKILL_BROKER_RELATIONS]).await.map_err(db_err)?;
    let skill = |id| skill_rows.iter().find(|row| row.type_id == id).map_or(0, |row| row.level.clamp(0, crate::consts::dogma::MAX_SKILL_LEVEL));
    let fees = super::fees::calculate(kind, assignee != 0, price, reward, minutes, skill(crate::consts::SKILL_ACCOUNTING), skill(crate::consts::SKILL_BROKER_RELATIONS));
    let creation_fee = fees.brokers_fee + fees.deposit + if kind == TYPE_ITEM_EXCHANGE { fees.sales_tax } else { 0.0 };
    let mut created = vec![];
    for group in groups {
        ownership::debit(db, outbox, owner, escrow_isk + creation_fee).await?;
        let id = contract_dao::insert(db, &contract_dao::NewContract {
            kind, issuer: me, issuer_corp: corp, assignee, start: start_id, end: end_id,
            price, reward, collateral, title, description, num_days,
            status: STATUS_OUTSTANDING, issued: now, expires: expiry_time(now, minutes),
            paid_deposit: fees.deposit,
            auction_tax_rate: if kind == TYPE_AUCTION { fees.sales_tax_rate } else { 0.0 },
            auction_tax_maximum: if kind == TYPE_AUCTION { fees.sales_tax_maximum } else { 0.0 },
            for_corp, issuer_wallet_key: wallet_key, issuer_hangar_flag: source.flag, issuer_source_location: source.location,
        }).await.map_err(db_err)?;
        for (item_id, qty) in group {
            if let Some(item) = loaded.remove(item_id) {
                let quantity = if item.singleton { 1 } else { i32::try_from(*qty).map_err(|_| notify_error(messages::CONTRACT_ITEM_QUANTITY_INSUFFICIENT))? };
                let root = if !item.singleton && quantity < item.quantity {
                    let root = super::super::inventory::split_stack(db, &item, quantity, source.owner, source.location, source.flag, false).await?;
                    let remaining = Item { quantity: item.quantity - quantity, ..item.clone() };
                    if for_corp {
                        super::super::inventory::notify_corp_item_change(db, outbox, &remaining, vec![(IX_STACK_SIZE, Value::Int(i64::from(item.quantity)))]).await?;
                    } else {
                        outbox.notify_pilot(me, "OnItemChange", vec![remaining.row(), Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int(i64::from(item.quantity)))]), Value::None]);
                    }
                    root
                } else {
                    if !crate::dao::items::inventory::move_item(db, &item, item.location_id, item.flag_id, item.owner_id, item.singleton).await.map_err(db_err)? {
                        return Err(notify_error(messages::CONTRACT_ITEM_MISSING));
                    }
                    item.item_id
                };
                custody::hand_over(db, &sde, outbox, &[root], custody::Custody { owner: source.owner, location: -id, flag: FLAG_HANGAR }, source.location).await?;
                shared::insert_item(db, id, root, item.type_id, quantity).await.map_err(db_err)?;
            }
        }
        for (type_id, qty) in requested {
            contract_dao::insert_requested_item(db, id, *type_id, *qty).await.map_err(db_err)?;
        }
        let volume = volume(db, &sde, id).await?;
        contract_dao::update_volume(db, id, volume).await.map_err(db_err)?;
        ownership::tell(db, outbox, assignee, "OnContractAssigned", id).await?;
        created.push(Value::Int(id));
    }
    outbox.notify_pilot(me, "OnContractCreated", vec![]);
    Ok(created)
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await?;
    for id in &created {
        info!("contracts: {me} created contract {id:?} (type {kind}) at {start_id}");
    }
    Ok(Value::List(created))
}

fn expiry_time(now: i64, minutes: i128) -> i64 {
    (i128::from(now) + minutes * i128::from(FILETIME_MINUTE)).min(i128::from(i64::MAX)) as i64
}

async fn volume(db: &(impl crate::db::executor::Executor + ?Sized), sde: &crate::sde::Sde, id: i64) -> Result<f64, CallError> {
    Ok(sum_volume(shared::items(db, &[id]).await.map_err(db_err)?.iter()
        .filter(|item| item.in_crate)
        .filter_map(|item| sde.types.get(item.type_id as i32).and_then(|t| t.volume).map(|volume| volume * item.quantity as f64))))
}

fn sum_volume(volumes: impl Iterator<Item = f64>) -> f64 {
    // Match SQL COALESCE(SUM, 0) sign on an empty set.
    volumes.fold(0.0, |total, v| total + v)
}

/// `AcceptContract(contractID, forCorp)` -> the contract.
pub(super) async fn accept(c: &Client, id: i64, for_corp: bool) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let wallet_key = if for_corp { i32::try_from(c.session.int("corpAccountKey").unwrap_or(0)).map_err(|_| notify_error(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED))? } else { ACCOUNT_CASH };
    let owner = ownership::actor_owner(me, corp, for_corp, wallet_key);
    let _trading = lock().await;
    let now = filetime_now();
    let sde = Arc::clone(&c.sde);
    db.transaction(
        |db, outbox: &mut Outbox| {
            let sde = Arc::clone(&sde);
            Box::pin(async move {
            let mut k = contract(db, id).await?;
            if for_corp { ownership::authorize_corporate_action(db, me, corp, wallet_key).await?; }
            if k.status != STATUS_OUTSTANDING || k.expired(now) {
                return Err(user_error(messages::MSG_CON_CONTRACT_NOT_OUTSTANDING, vec![]));
            }
            if k.issuer_owner().id == owner.id {
                return Err(notify_error(messages::CONTRACT_SELF_ACCEPTANCE_FORBIDDEN));
            }
            if !k.available_to(db, me, corp).await? {
                return Err(notify_error(messages::CONTRACT_ASSIGNED_TO_OTHER));
            }
            match k.kind {
                TYPE_ITEM_EXCHANGE => {
                    // The requested items must all be in the acceptor's hangar at the start station.
                    let wanted = contract_dao::requested_items(db, id).await.map_err(db_err)?;
                    let source = if for_corp && !wanted.is_empty() {
                        custody::issuer_source(db, me, corp, true, k.start, crate::consts::FLAG_CORP_DELIVERIES).await?
                    } else { custody::receiving(owner.id, k.start) };
                    let mut takes = vec![];
                    let mut types = std::collections::HashSet::new();
                    for w in &wanted {
                        let (type_id, qty) = (w.type_id, w.quantity);
                        if type_id <= 0 || qty <= 0 || !types.insert(type_id) || sde.types.get(type_id).is_none() {
                            return Err(notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING));
                        }
                        let stacks = load_items(db, &sde, ItemQuery::OwnerLocationFlagTypeDescending(source.owner, source.location, source.flag, type_id)).await.map_err(db_err)?;
                        let have: i64 = stacks.iter().map(|s| if s.singleton { 1 } else { i64::from(s.quantity.max(0)) }).sum();
                        if have < i64::from(qty) {
                            return Err(notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING));
                        }
                        let mut left = qty;
                        for item in stacks {
                            if left == 0 { break; }
                            let n = if item.singleton { 1 } else { item.quantity.min(left) };
                            if n <= 0 { continue; }
                            left -= n;
                            takes.push((item, n));
                        }
                    }
                    takes.sort_unstable_by_key(|(item, _)| item.item_id);
                    let ids: Vec<_> = takes.iter().map(|(item, _)| item.item_id).collect();
                    let tree = crate::dao::trades::descendants(db, &ids).await.map_err(db_err)?;
                    if !crate::dao::trades::reserved(db, &tree).await.map_err(db_err)?.is_empty() {
                        return Err(notify_error(messages::trade::TRADE_ITEM_RESERVED));
                    }
                    if crate::dao::industry::has_active_blueprint_job(db, &tree).await.map_err(db_err)? {
                        return Err(notify_error(messages::industry::BLUEPRINT_ACTIVE));
                    }
                    ownership::debit(db, outbox, owner, k.price).await?;
                    let mut roots = Vec::new();
                    for (item, quantity) in takes {
                        let item_id = if !item.singleton && quantity < item.quantity {
                            let id = super::super::inventory::split_stack(db, &item, quantity, source.owner, source.location, source.flag, false).await?;
                            let remaining = Item { quantity: item.quantity - quantity, ..item.clone() };
                            if for_corp {
                                super::super::inventory::notify_corp_item_change(db, outbox, &remaining, vec![(IX_STACK_SIZE, Value::Int(i64::from(item.quantity)))]).await?;
                            } else {
                                outbox.notify_pilot(me, "OnItemChange", vec![remaining.row(), Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int(i64::from(item.quantity)))]), Value::None]);
                            }
                            id
                        } else {
                            if !crate::dao::items::inventory::move_item(db, &item, item.location_id, item.flag_id, item.owner_id, item.singleton).await.map_err(db_err)? {
                                return Err(notify_error(messages::CONTRACT_REQUIRED_ITEMS_MISSING));
                            }
                            item.item_id
                        };
                        roots.push(item_id);
                    }
                    custody::hand_over(db, &sde, outbox, &roots, custody::receiving(k.issuer_owner().id, k.start), source.location).await?;
                    ownership::credit(db, outbox, k.issuer_owner(), k.price).await?;
                    ownership::credit(db, outbox, owner, k.reward).await?;
                    let escrow = escrowed_ids(db, id).await?;
                    custody::hand_over(db, &sde, outbox, &escrow, custody::receiving(owner.id, k.start), -id).await?;
                    if !contract_dao::accept_exchange(db, id, STATUS_OUTSTANDING, owner.id, wallet_key, now, STATUS_FINISHED, now).await.map_err(db_err)? {
                        return Err(user_error(messages::MSG_CON_CONTRACT_NOT_OUTSTANDING, vec![]));
                    }
                    ownership::credit(db, outbox, k.issuer_owner(), k.paid_deposit).await?;
                    k.paid_deposit = 0.0;
                    k.acceptor = owner.id;
                    k.acceptor_wallet_key = wallet_key;
                    k.accepted = now;
                    k.completed = now;
                    k.status = STATUS_FINISHED;
                    ownership::tell(db, outbox, k.issuer_owner().id, "OnContractCompleted", id).await?;
                }
                TYPE_COURIER => {
                    ownership::debit(db, outbox, owner, k.collateral).await?;
                    let dest = custody::receiving(owner.id, k.start);
                    let wrap = contract_dao::insert_package(db, TYPE_PLASTIC_WRAP, owner.id, dest.location, dest.flag, &messages::contract_package_name(id)).await.map_err(db_err)?;
                    let item = load_items(db, &sde, ItemQuery::Id(wrap)).await.map_err(db_err)?.pop().ok_or_else(|| notify_error(messages::CONTRACT_ITEM_MISSING))?;
                    if for_corp {
                        super::super::inventory::notify_corp_item_change(db, outbox, &item, vec![(IX_LOCATION_ID, Value::Int(LOCATION_JUNKYARD))]).await?;
                    } else {
                        if transfers::docked(db, me).await.map_err(db_err)? == Some(k.start) {
                            outbox.notify_pilot(me, "OnItemChange", vec![item.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(LOCATION_JUNKYARD))]), Value::None]);
                        }
                    }
                    if !contract_dao::accept_courier(db, id, STATUS_OUTSTANDING, owner.id, wallet_key, now, STATUS_IN_PROGRESS, wrap).await.map_err(db_err)? {
                        return Err(user_error(messages::MSG_CON_CONTRACT_NOT_OUTSTANDING, vec![]));
                    }
                    ownership::credit(db, outbox, k.issuer_owner(), k.paid_deposit).await?;
                    k.paid_deposit = 0.0;
                    k.acceptor = owner.id;
                    k.acceptor_wallet_key = wallet_key;
                    k.accepted = now;
                    k.status = STATUS_IN_PROGRESS;
                    k.package = Some(wrap);
                    ownership::tell(db, outbox, k.issuer_owner().id, "OnContractAccepted", id).await?;
                }
                _ => return Err(notify_error(messages::AUCTION_ONLY_BIDDABLE)),
            }
            Ok(k.keyval(&sde))
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await.map(|value| {
        info!("contracts: {me} accepted contract {id}");
        value
    })
}

/// `CompleteContract(contractID, status)`: deliver a courier package (finished), give one up
/// or call an overdue one in (failed), or turn down a contract assigned to you (rejected).
pub(super) async fn complete(c: &Client, id: i64, status: i32) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let sde = &c.sde;
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let session_wallet = i32::try_from(c.session.int("corpAccountKey").unwrap_or(0)).unwrap_or(0);
    let _trading = lock().await;
    let now = filetime_now();
    db.transaction(
        |db, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
            let sde = &*sde;
            let mut k = contract(db, id).await?;
            match status {
                STATUS_FINISHED => {
                    let members = if k.acceptor == corp && super::super::corps::is_player_corp(corp) { crate::dao::characters::members::active_ids(db, corp).await.map_err(db_err)? } else { Vec::new() };
                    if k.kind != TYPE_COURIER || k.status != STATUS_IN_PROGRESS || !(k.acceptor == me || members.contains(&me)) {
                        return Err(notify_error(messages::COURIER_DELIVERY_REQUIRES_ACCEPTOR));
                    }
                    let wrap = match k.package {
                        Some(w) => load_items(db, sde, ItemQuery::Id(w)).await.map_err(db_err)?.pop(),
                        None => None,
                    };
                    let Some(wrap) = wrap.filter(|w| w.location_id == k.end && (w.owner_id == me || (w.owner_id == k.acceptor && k.acceptor == corp && super::super::corps::is_player_corp(corp)))) else {
                        return Err(notify_error(messages::COURIER_PACKAGE_MUST_BE_AT_DESTINATION));
                    };
                    custody::remove_package(db, outbox, &wrap).await?;
                    let roots = escrowed_ids(db, id).await?;
                    custody::hand_over(db, sde, outbox, &roots, custody::receiving(k.issuer_owner().id, k.end), -id).await?;
                    ownership::credit(db, outbox, k.acceptor_owner(), round2(k.reward + k.collateral)).await?;
                    set_status(db, &mut k, STATUS_FINISHED, now).await?;
                    ownership::tell(db, outbox, k.issuer_owner().id, "OnContractCompleted", id).await?;
                }
                STATUS_FAILED => {
                    let issuer_action = (k.issuer_owner().id == me || (k.for_corp && k.issuer_corp == corp)) && k.overdue(now);
                    let acceptor_action = k.acceptor == me || (k.acceptor == corp && super::super::corps::is_player_corp(corp));
                    if k.kind != TYPE_COURIER || k.status != STATUS_IN_PROGRESS || !(acceptor_action || issuer_action) {
                        return Err(notify_error(messages::COURIER_CANNOT_BE_FAILED_NOW));
                    }
                    if acceptor_action && k.acceptor == corp { ownership::authorize_corporate_action(db, me, corp, k.acceptor_wallet_key).await?; }
                    else if issuer_action && k.for_corp { ownership::authorize_corporate_action(db, me, corp, k.issuer_wallet_key).await?; }
                    ownership::credit(db, outbox, k.issuer_owner(), round2(k.collateral + k.reward)).await?;
                    let wrap = match k.package {
                        Some(w) => load_items(db, sde, ItemQuery::Id(w)).await.map_err(db_err)?.pop(),
                        None => None,
                    };
                    match wrap {
                        Some(w) => {
                            let destination = custody::Custody { owner: w.owner_id, location: w.location_id, flag: w.flag_id };
                            custody::remove_package(db, outbox, &w).await?;
                            let roots = escrowed_ids(db, id).await?;
                            custody::hand_over(db, sde, outbox, &roots, destination, -id).await?;
                        }
                        None => destroy_escrow(db, id).await?,
                    }
                    set_status(db, &mut k, STATUS_FAILED, now).await?;
                    ownership::tell(db, outbox, if issuer_action { k.acceptor } else { k.issuer_owner().id }, "OnContractFailed", id).await?;
                }
                STATUS_REJECTED => {
                    if k.status != STATUS_OUTSTANDING || k.assignee == 0 || !(k.assignee == me || k.assignee == corp) {
                        return Err(notify_error(messages::CONTRACT_ASSIGNEE_REQUIRED_TO_REJECT));
                    }
                    if k.assignee == corp && super::super::corps::is_player_corp(corp) {
                        ownership::authorize_corporate_action(db, me, corp, session_wallet).await?;
                    }
                    set_status(db, &mut k, STATUS_REJECTED, now).await?;
                    ownership::tell(db, outbox, k.issuer_owner().id, "OnContractRejected", id).await?;
                }
                _ => return Err(notify_error(messages::CONTRACT_STATUS_TRANSITION_INVALID)),
            }
            Ok(Value::Bool(true))
        })},
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await.map(|value| {
        info!("contracts: {me} set contract {id} to status {status}");
        value
    })
}

/// The issuer takes an outstanding (or expired, or rejected) contract back with its escrow.
pub(super) async fn delete_one(c: &Client, id: i64) -> Result<(), CallError> {
    let (db, sde, world) = (&c.db, &c.sde, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let now = filetime_now();
    db.transaction(
        |db, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
            let sde = &*sde;
            let mut k = contract(db, id).await?;
            if !(if k.for_corp { k.issuer_corp == corp } else { k.issuer == me }) || !(k.status == STATUS_OUTSTANDING || k.status == STATUS_REJECTED) {
                return Err(notify_error(messages::CONTRACT_ISSUER_REQUIRED_TO_DELETE));
            }
            if k.for_corp { ownership::authorize_corporate_action(db, me, corp, k.issuer_wallet_key).await?; }
            if k.kind == TYPE_AUCTION && shared::top_bid(db, id).await.map_err(db_err)?.is_some() {
                return Err(notify_error(messages::BIDDED_AUCTION_CANNOT_BE_DELETED));
            }
            let roots = escrowed_ids(db, id).await?;
            let destination = custody::issuer_return(db, &k).await?;
            custody::hand_over(db, sde, outbox, &roots, destination, -id).await?;
            if k.kind != TYPE_AUCTION {
                ownership::credit(db, outbox, k.issuer_owner(), k.reward).await?;
            }
            set_status(db, &mut k, STATUS_DELETED, now).await?;
            Ok(())
        })},
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await?;
    info!("contracts: {me} deleted contract {id}");
    Ok(())
}

/// `PlaceBid(contractID, amount, forCorp)`; a bid at the buyout wins at once.
pub(super) async fn place_bid(c: &Client, id: i64, amount: f64, for_corp: bool) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let sde = &c.sde;
    let amount = round2(amount);
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let wallet_key = if for_corp { i32::try_from(c.session.int("corpAccountKey").unwrap_or(0)).map_err(|_| notify_error(messages::DEPARTMENT_WALLET_WITHDRAW_UNAUTHORIZED))? } else { ACCOUNT_CASH };
    let owner = ownership::actor_owner(me, corp, for_corp, wallet_key);
    let _trading = lock().await;
    let now = filetime_now();
    let (amount, buyout) = db.transaction(
        |db, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
    let sde = &*sde;
    let mut k = contract(db, id).await?;
    if for_corp { ownership::authorize_corporate_action(db, me, corp, wallet_key).await?; }
    if k.kind != TYPE_AUCTION || k.status != STATUS_OUTSTANDING || k.expired(now) {
        return Err(user_error(messages::MSG_CON_CONTRACT_NOT_OUTSTANDING, vec![]));
    }
    if k.issuer_owner().id == owner.id {
        return Err(notify_error(messages::AUCTION_SELF_BID_FORBIDDEN));
    }
    if !k.available_to(db, me, corp).await? {
        return Err(notify_error(messages::AUCTION_ASSIGNED_TO_OTHER));
    }
    let mut amount = amount;
    let top = shared::top_bid(db, id).await.map_err(db_err)?;
    if !amount.is_finite() || amount < k.price || top.as_ref().is_some_and(|bid| amount <= bid.amount) {
        return Err(notify_error(messages::BID_MUST_EXCEED_CURRENT_AND_START));
    }
    let buyout = k.collateral > 0.0 && amount >= k.collateral;
    if buyout {
        amount = k.collateral;
    }
    ownership::debit(db, outbox, owner, amount).await?;
    if let Some(bid) = top {
        ownership::credit(db, outbox, ownership::FinancialOwner { id: bid.bidder_id, wallet_key: bid.wallet_key }, bid.amount).await?;
        ownership::tell(db, outbox, bid.bidder_id, "OnContractOutbid", id).await?;
    }
    contract_dao::insert_bid(db, id, owner.id, wallet_key, amount, now).await.map_err(db_err)?;
    if buyout {
        settle(db, sde, outbox, &mut k, owner, amount, now).await?;
        ownership::tell(db, outbox, k.issuer_owner().id, "OnContractBuyout", id).await?;
    }
    Ok((amount, buyout))
        })},
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await?;
    info!("contracts: {me} bid {amount} ISK on auction {id}");
    if buyout {
        info!("contracts: auction {id} went to {me} for {amount} ISK");
    }
    Ok(Value::Bool(true))
}

/// An auction won: the items go to the winner at the start station, the bid to the issuer.
async fn settle<X: Send>(db: &(impl Executor + ?Sized), sde: &crate::sde::Sde, outbox: &mut Outbox<X>, k: &mut Contract, winner: ownership::FinancialOwner, amount: f64, now: i64) -> Result<(), CallError> {
    let roots = escrowed_ids(db, k.id).await?;
    custody::hand_over(db, sde, outbox, &roots, custody::receiving(winner.id, k.start), -k.id).await?;
    ownership::credit(db, outbox, k.issuer_owner(), amount).await?;
    ownership::credit(db, outbox, k.issuer_owner(), k.paid_deposit).await?;
    ownership::debit(db, outbox, k.issuer_owner(), super::fees::sales_tax_amount(amount, k.auction_tax_rate, k.auction_tax_maximum)).await?;
    k.acceptor = winner.id;
    k.acceptor_wallet_key = winner.wallet_key;
    set_status(db, k, STATUS_FINISHED, now).await?;
    contract_dao::clear_deposit(db, k.id).await.map_err(db_err)?;
    k.paid_deposit = 0.0;
    ownership::tell(db, outbox, k.issuer_owner().id, "OnContractCompleted", k.id).await?;
    ownership::tell(db, outbox, winner.id, "OnContractCompleted", k.id).await?;
    Ok(())
}

pub(super) async fn settle_expired(db: &Db, sde: &Arc<crate::sde::Sde>, world: &World) -> Result<(), CallError> {
    let _trading = lock().await;
    let now = filetime_now();
    let expired = contract_dao::expired_auctions_with_bids(db, TYPE_AUCTION, STATUS_OUTSTANDING, now).await.map_err(db_err)?;
    for k in expired {
        let id = k.id;
        let settled = settle_expired_candidate(db, sde, world, id, now).await?;
        if let Some((winner, amount)) = settled {
            info!("contracts: auction {id} went to {winner} for {amount} ISK");
        }
    }
    Ok(())
}

async fn settle_expired_candidate(db: &Db, sde: &Arc<crate::sde::Sde>, world: &World, id: i64, now: i64) -> Result<Option<(i64, f64)>, CallError> {
    db.transaction(
        |db, outbox: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
                let mut k = contract(db, id).await?;
                if k.kind != TYPE_AUCTION || k.status != STATUS_OUTSTANDING || !k.expired(now) {
                    return Ok(None);
                }
                if let Some(bid) = shared::top_bid(db, id).await.map_err(db_err)? {
                    settle(db, &sde, outbox, &mut k, ownership::FinancialOwner { id: bid.bidder_id, wallet_key: bid.wallet_key }, bid.amount, now).await?;
                    Ok(Some((bid.bidder_id, bid.amount)))
                } else {
                    Ok(None)
                }
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await
}

#[cfg(test)] mod tests;
