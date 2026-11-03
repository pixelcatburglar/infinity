//! Station reprocessing service using canonical SDE materials and game prices.
//! Successful reprocessing tax debits emit wallet events after transaction commit.
use super::*;
use crate::services::inventory;

// ---------------------------------------------------------------- reprocessing

/// The pilot's yield multiplier for reprocessing `type_id`: ores and ice train
/// Reprocessing (+3%), Reprocessing Efficiency (+2%) and their processing skill (+2%) per
/// level; everything else Scrapmetal Processing (+2%).
fn skill_multiplier(skills: &crate::services::skills::effective::SkillSnapshot, sde: &crate::sde::Sde, type_id: i32) -> f64 {
    let own_skill = sde.types.attribute(type_id, ATTR_PROCESSING_SKILL);
    let level = |skill: i32| skills.level(skill) as f64;
    match own_skill {
        Some(s) => (1.0 + 0.03 * level(SKILL_REPROCESSING)) * (1.0 + 0.02 * level(SKILL_REPROCESSING_EFFICIENCY)) * (1.0 + 0.02 * level(s as i32)),
        None => 1.0 + 0.02 * level(SKILL_SCRAPMETAL),
    }
}

struct Quote {
    type_id: i32,
    /// Units left over (less than a portion).
    leftover: i32,
    /// (typeID, received, lost, ISK tax)
    materials: Vec<(i32, i64, i64, f64)>,
}

fn output_overflow() -> CallError {
    notify_error(messages::REPROCESS_OUTPUT_STACK_LIMIT)
}

pub(super) fn add_output(total: i64, amount: i64) -> Result<i64, CallError> {
    total.checked_add(amount).ok_or_else(output_overflow)
}

pub(super) fn output_stack_quantity(current: Option<i32>, amount: i64) -> Result<i32, CallError> {
    let amount = i32::try_from(amount).map_err(|_| output_overflow())?;
    current.unwrap_or(0).checked_add(amount).ok_or_else(output_overflow)
}

async fn quote(
    c: &Client,
    sde: &crate::sde::Sde,
    skills: &crate::services::skills::effective::SkillSnapshot,
    item: &crate::dao::items::loading::Item,
) -> Result<Option<Quote>, CallError> {
    let portion = sde.types.get(item.type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing type {}", item.type_id)))?.portion_size.unwrap_or(1).max(1);
    let quantity = if item.singleton { 1 } else { item.quantity };
    let batches = quantity / portion;
    if batches == 0 {
        return Ok(None);
    }
    let materials = sde.types.materials(item.type_id);
    if materials.is_empty() {
        return Ok(None);
    }
    let ids: Vec<_> = materials.iter().map(|m| m.material_type_id).collect();
    let prices: HashMap<_, _> = crate::dao::items::mining::prices(&c.db, &ids).await.map_err(db_err)?.into_iter().map(|p| (p.type_id, p.average)).collect();
    let efficiency = (crate::config::game().mining.station_reprocessing_efficiency * skill_multiplier(skills, sde, item.type_id)).min(1.0);
    let mut result = Vec::new();
    for r in materials {
        let total = r.quantity.unwrap_or(0).checked_mul(batches as i64).ok_or_else(output_overflow)?;
        let received = (total as f64 * efficiency).floor() as i64;
        if total > 0 {
            result.push((r.material_type_id, received, total - received, received as f64 * prices.get(&r.material_type_id).copied().unwrap_or(0.0) * crate::config::game().mining.station_reprocessing_tax));
        }
    }
    let materials = result;
    Ok(Some(Quote { type_id: item.type_id, leftover: quantity % portion, materials }))
}

fn quote_value(item_id: i64, q: &Quote) -> Value {
    let recoverables = q
        .materials
        .iter()
        .map(|(t, received, lost, tax)| {
            keyval([("typeID", Value::Int(*t as i64)), ("client", Value::Int(*received)), ("unrecoverable", Value::Int(*lost)), ("iskCost", Value::Float(*tax))])
        })
        .collect();
    keyval([
        ("itemID", Value::Int(item_id)),
        ("typeID", Value::Int(q.type_id as i64)),
        ("leftOvers", Value::Int(q.leftover as i64)),
        ("recoverables", Value::List(recoverables)),
    ])
}

/// The pilot's items to reprocess: in its hangar at this station, or its ship here.
async fn reprocessable(c: &Client, ids: &[i64]) -> Result<Vec<crate::dao::items::loading::Item>, CallError> {
    let char_id = c.session.int("charid").unwrap_or(0);
    let station = c.session.int("stationid").unwrap_or(0);
    let ship = c.session.int("shipid").unwrap_or(0);
    crate::services::items::load_items(&c.db, &c.sde, crate::dao::items::loading::ItemQuery::IdsOwnerLocations(ids, char_id, station, ship)).await.map_err(db_err)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.bound {
        Some(reprocessing(c, call).await)
    } else {
        None
    }
}

/// Bound reprocessingSvc (Moniker('reprocessingSvc', stationID)).
async fn reprocessing(c: &mut Client, call: &Call<'_>) -> CallResult {
    let char_id = c.session.int("charid").unwrap_or(0);
    match call.method {
        // GetQuotes(itemIDs, activeShipID) -> (stationTax, {typeID|None: stationEfficiency}, {itemID: quote})
        "GetQuotes" => {
            let skills = crate::services::skills::effective::snapshot(&c.db, char_id).await?;
            let mut quotes = vec![];
            for item in reprocessable(c, &value_id_list(call.args.first())).await? {
                if let Some(q) = quote(c, &c.sde, &skills, &item).await? {
                    quotes.push((Value::Int(item.item_id), quote_value(item.item_id, &q)));
                }
            }
            Ok(Value::tuple([Value::Float(crate::config::game().mining.station_reprocessing_tax), Value::Dict(vec![(Value::None, Value::Float(crate::config::game().mining.station_reprocessing_efficiency))]), Value::Dict(quotes)]))
        }
        "GetStationEfficiencyForTypeID" => Ok(Value::Float(crate::config::game().mining.station_reprocessing_efficiency)),
        "GetCharRefiningYieldPercentageForType" => {
            let type_id = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0) as i32;
            let skills = crate::services::skills::effective::snapshot(&c.db, char_id).await?;
            Ok(Value::Float(skill_multiplier(&skills, &c.sde, type_id)))
        }
        // Reprocess(itemIDs, fromLocationID, ownerID, outputLocationID, outputFlagID)
        //   -> (reprocessed itemIDs, {typeID: quantity})
        "Reprocess" => {
            let station = c.session.int("stationid").ok_or_else(|| notify_error(messages::MUST_DOCK_TO_REPROCESS))?;
            let out_location = call.args.get(3).and_then(|v| v.as_int()).unwrap_or(station);
            let out_flag = call.args.get(4).and_then(|v| v.as_int()).map(|f| f as i32).unwrap_or(FLAG_HANGAR);
            let items = reprocessable(c, &value_id_list(call.args.first())).await?;
            let skills = crate::services::skills::effective::snapshot(&c.db, char_id).await?;
            let mut sources = Vec::new();
            let mut output = BTreeMap::<i32, i64>::new();
            let mut tax = 0.0;
            for item in items {
                let Some(q) = quote(c, &c.sde, &skills, &item).await? else { continue };
                for (type_id, received, _, isk) in &q.materials {
                    let total = output.entry(*type_id).or_default();
                    *total = add_output(*total, *received)?;
                    tax += isk;
                }
                sources.push((item, q.leftover));
            }
            let plan = Arc::new((sources, output));
            let db = c.db.clone();
            let sde = Arc::clone(&c.sde);
            let plan = db.transaction(
                |tx, outbox| {
                    let plan = Arc::clone(&plan);
                    let sde = Arc::clone(&sde);
                    Box::pin(async move {
                    use crate::dao::items::quantities::{apply, QuantityChange};
                    let mut changes = BTreeMap::new();
                    for (item, leftover) in &plan.0 {
                        changes.insert(item.item_id, QuantityChange::new(item, (*leftover > 0).then_some(*leftover)));
                    }
                    let mut inserts = BTreeMap::new();
                    for (&type_id, &amount) in &plan.1 {
                        if amount <= 0 { continue; }
                        let quantity = output_stack_quantity(None, amount)?;
                        let candidates = crate::services::items::load_items(tx, &sde, crate::dao::items::loading::ItemQuery::LocationFlagTypeOwnerStack(out_location, out_flag, type_id, char_id)).await.map_err(db_err)?;
                        let stack = candidates.into_iter().next();
                        if let Some(item) = stack {
                            let current = changes.get(&item.item_id).map_or(Some(item.quantity), |change| change.quantity);
                            let final_quantity = output_stack_quantity(current, amount)?;
                            changes.entry(item.item_id).and_modify(|change| change.quantity = Some(final_quantity))
                                .or_insert_with(|| QuantityChange::new(&item, Some(final_quantity)));
                        } else {
                            inserts.insert(type_id, quantity);
                        }
                    }
                    if !apply(tx, &changes).await.map_err(db_err)? { return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY)); }
                    for (&id, change) in &changes {
                        if change.quantity.is_some() {
                            inventory::notify_item_change(tx, &sde, outbox, id, vec![(IX_STACK_SIZE, Value::Int(change.expected_quantity as i64))]).await?;
                        } else {
                            let item = plan.0.iter().find(|(item, _)| item.item_id == id).expect("removed item is a source");
                            inventory::notify_item_removed(outbox, item.0.clone());
                        }
                    }
                    for (type_id, quantity) in inserts {
                        let id = crate::dao::items::transfers::insert(tx, type_id, char_id, out_location, out_flag, quantity).await.map_err(db_err)?;
                        inventory::notify_item_change(tx, &sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(0)), (IX_FLAG, Value::Int(out_flag as i64))]).await?;
                    }
                    if tax > 0.0 {
                        let balance = crate::dao::wallets::characters::debit(tx, char_id, tax).await.map_err(db_err)?.ok_or_else(|| notify_error(messages::INSUFFICIENT_ISK_FOR_REPROCESSING_TAX))?;
                        outbox.notify_client("OnAccountChange", vec![Value::str("cash"), Value::Int(char_id), Value::Float(balance)]);
                        outbox.event(crate::lua::events::GameEvent::WalletChanged { owner_id: char_id, account_key: crate::consts::ACCOUNT_CASH, balance, delta: -tax });
                    }
                    Ok::<_, CallError>(Arc::clone(&plan))
                })
                },
                |value, outbox| async { crate::db::outbox::dispatch_client(value, outbox, c) },
            ).await?;
            let done: Vec<_> = plan.0.iter().map(|(item, _)| Value::Int(item.item_id)).collect();
            info!(user = c.login.user_id, "reprocessed {} items into {} material types ({tax:.0} ISK tax)", done.len(), plan.1.len());
            Ok(Value::tuple([Value::List(done), Value::Dict(plan.1.iter().map(|(&t, &q)| (Value::Int(t as i64), Value::Int(q))).collect())]))
        }
        _ => Ok(Value::None),
    }
}
