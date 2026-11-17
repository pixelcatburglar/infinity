//! Station reprocessing service (bound reprocessingSvc, mirroring the client's
//! `reprocessing/util.py`) using canonical SDE materials and game prices.
//! Each request loads one character Fit at call time: ore and ice yields start at the
//! pilot's effective refining yield (base skills plus installed implants and active
//! boosters); items without a processing skill start at 1 and use Scrapmetal
//! Processing. Each quote re-reads the current state, so a removed implant or an
//! expired booster changes later requests.
//! Successful reprocessing tax debits emit wallet events after transaction commit.
//! Material quantities sent on the wire project to the client display limit;
//! stored amounts and stored tax debits stay exact.
//! Reprocess takes the economy lock, consumes sources under their item locks and
//! refuses corporation-locked or vote-reserved blueprint originals. Its output
//! goes to the pilot's hangar, the source's own location and flag or a division
//! of the character's corporation office, authorized once inside the transaction;
//! corporate inputs and structures are unsupported.
use super::*;
use crate::services::fit::PERCENT_SCALE;
use crate::services::inventory;
use infinity_core::consts::{dogma::ATTR_SKILL_LEVEL, CORP_HANGAR_DIVISIONS, FLAG_CORP_SAG1};

// ---------------------------------------------------------------- reprocessing

/// Client dogma/const.py attributeRefiningYieldPercentage: the character's effective
/// ore and ice yield, already modified by implant and booster effects.
const ATTR_REFINING_YIELD_PERCENTAGE: i32 = 378;
/// Client dogma/const.py attributeRefiningYieldMutator: a skill type's static
/// per-level yield bonus in percentage points.
const ATTR_REFINING_YIELD_MUTATOR: i32 = 379;

/// The pilot's yield multiplier for reprocessing `type_id`, mirroring
/// `reprocessing/util.py`: types with a processing skill start at the pilot's
/// effective refining yield; everything else starts at 1 and trains Scrapmetal
/// Processing. The chosen skill adds its SDE per-level percentage per level.
fn yield_multiplier(fit: &Fit, type_id: i32) -> f64 {
    let sde = fit.sde();
    let (base, skill) = match sde.types.attribute(type_id, ATTR_PROCESSING_SKILL) {
        Some(skill) => (fit.value(Key::Char, ATTR_REFINING_YIELD_PERCENTAGE), skill as i32),
        None => (1.0, SKILL_SCRAPMETAL),
    };
    base * (1.0 + fit.value(Key::Skill(skill), ATTR_SKILL_LEVEL) * sde.types.attribute(skill, ATTR_REFINING_YIELD_MUTATOR).unwrap_or(0.0) / PERCENT_SCALE)
}

struct Quote {
    type_id: i32,
    /// Units left over (less than a portion).
    leftover: i64,
    /// (typeID, received, lost, ISK tax)
    materials: Vec<(i32, i64, i64, f64)>,
}

fn output_overflow() -> CallError {
    notify_error(messages::REPROCESS_OUTPUT_STACK_LIMIT)
}

pub(super) fn add_output(total: i64, amount: i64) -> Result<i64, CallError> {
    total.checked_add(amount).ok_or_else(output_overflow)
}


fn quote(
    sde: &crate::sde::Sde,
    fit: &Fit,
    item: &crate::dao::items::loading::Item,
) -> Result<Option<Quote>, CallError> {
    let portion = i64::from(sde.types.get(item.type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing type {}", item.type_id)))?.portion_size.unwrap_or(1).max(1));
    let quantity = if item.singleton { 1 } else { item.quantity };
    let batches = quantity / portion;
    if batches == 0 {
        return Ok(None);
    }
    let materials = sde.types.materials(item.type_id);
    if materials.is_empty() {
        return Ok(None);
    }
    let efficiency = (crate::config::game().mining.station_reprocessing_efficiency * yield_multiplier(fit, item.type_id)).min(1.0);
    let mut result = Vec::new();
    for r in materials {
        let total = r.quantity.unwrap_or(0).checked_mul(batches).ok_or_else(output_overflow)?;
        let received = (total as f64 * efficiency).floor() as i64;
        if total > 0 {
            result.push((r.material_type_id, received, total - received, received as f64 * sde.prices.get(r.material_type_id).map_or(0.0, |p| p.average) * crate::config::game().mining.station_reprocessing_tax));
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
            keyval([("typeID", Value::Int(*t as i64)), ("client", Value::Int(crate::services::dogma::info::cap_client_quantity(*received))), ("unrecoverable", Value::Int(crate::services::dogma::info::cap_client_quantity(*lost))), ("iskCost", Value::Float(*tax))])
        })
        .collect();
    keyval([
        ("itemID", Value::Int(item_id)),
        ("typeID", Value::Int(q.type_id as i64)),
        ("leftOvers", Value::Int(q.leftover)),
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
    let at = crate::client::filetime_now();
    match call.method {
        // GetQuotes(itemIDs, activeShipID) -> (stationTax, {typeID|None: stationEfficiency}, {itemID: quote})
        "GetQuotes" => {
            let fit = Fit::load_character_at(&c.db, Arc::clone(&c.sde), char_id, at).await?;
            let mut quotes = vec![];
            for item in reprocessable(c, &value_id_list(call.args.first())).await? {
                if let Some(q) = quote(&c.sde, &fit, &item)? {
                    quotes.push((Value::Int(item.item_id), quote_value(item.item_id, &q)));
                }
            }
            Ok(Value::tuple([Value::Float(crate::config::game().mining.station_reprocessing_tax), Value::Dict(vec![(Value::None, Value::Float(crate::config::game().mining.station_reprocessing_efficiency))]), Value::Dict(quotes)]))
        }
        "GetStationEfficiencyForTypeID" => Ok(Value::Float(crate::config::game().mining.station_reprocessing_efficiency)),
        "GetCharRefiningYieldPercentageForType" => {
            let type_id = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0) as i32;
            let fit = Fit::load_character_at(&c.db, Arc::clone(&c.sde), char_id, at).await?;
            Ok(Value::Float(yield_multiplier(&fit, type_id)))
        }
        // Reprocess(itemIDs, fromLocationID, ownerID, outputLocationID, outputFlagID)
        //   -> (reprocessed itemIDs, {typeID: quantity})
        "Reprocess" => {
            let station = c.session.int("stationid").ok_or_else(|| notify_error(messages::MUST_DOCK_TO_REPROCESS))?;
            let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
            // Missing or None selects the current hangar; explicit output values
            // must be integers, never truncated or silently defaulted.
            let out_location = match call.args.get(3) {
                None | Some(Value::None) => station,
                Some(value) => value.as_int().ok_or_else(refuse)?,
            };
            let out_flag = match call.args.get(4) {
                None | Some(Value::None) => FLAG_HANGAR,
                Some(value) => value.as_int().and_then(|v| i32::try_from(v).ok()).ok_or_else(refuse)?,
            };
            let items = reprocessable(c, &value_id_list(call.args.first())).await?;
            let fit = Fit::load_character_at(&c.db, Arc::clone(&c.sde), char_id, at).await?;
            let mut sources = Vec::new();
            let mut output = BTreeMap::<i32, i64>::new();
            let mut tax = 0.0;
            for item in items {
                let Some(q) = quote(&c.sde, &fit, &item)? else { continue };
                for (type_id, received, _, isk) in &q.materials {
                    let total = output.entry(*type_id).or_default();
                    *total = add_output(*total, *received)?;
                    tax += isk;
                }
                sources.push((item, q.leftover));
            }
            let identity = crate::services::identity::Identity::of_client(c);
            // The client's three output selections: the current hangar, the
            // source's own (location, flag) pair, or a corporation office
            // division; args[1] and args[2] are hints, never authority.
            let same_source = sources.iter().any(|(item, _)| item.location_id == out_location && item.flag_id == out_flag);
            let is_station = c.sde.map.station(out_location).is_some();
            let (container, expected_owner, held_hull) = if (FLAG_CORP_SAG1..FLAG_CORP_SAG1 + CORP_HANGAR_DIVISIONS).contains(&out_flag) {
                (inventory::access::Container::Item(out_location), identity.corp_id, false)
            } else if same_source && !is_station {
                if identity.ship_id != Some(out_location) { return Err(refuse()); }
                (inventory::access::Container::Item(out_location), char_id, true)
            } else if is_station && (out_flag == FLAG_HANGAR || same_source) {
                (inventory::access::Container::Hangar { station_id: out_location, owner_id: char_id }, char_id, false)
            } else {
                return Err(refuse());
            };
            // Reachability is sampled once outside the transaction; the ancestry
            // is rechecked inside it on every attempt.
            let reach = inventory::reachability::Reachability::capture(&identity, &c.db, &c.world, c.session.int("solarsystemid"),
                &[inventory::reachability::Endpoint { location: out_location, flag: out_flag }]).await?;

            // An economy mutation: the market guard serializes order writers and
            // is held until the post-commit dispatch has run.
            let _trading = crate::services::market::lock().await;
            let plan = Arc::new((sources, output));
            let db = c.db.clone();
            let sde = Arc::clone(&c.sde);
            let plan = db.transaction(
                |tx, outbox| {
                    let plan = Arc::clone(&plan);
                    let sde = Arc::clone(&sde);
                    let container = container.clone();
                    let reach = reach.clone();
                    Box::pin(async move {
                    // Consume the sources under real item locks: collect the final
                    // set at once, revalidate the expected owner/location/quantity,
                    // then screen blueprint originals for corporation locks and
                    // vote-case reservations before any quantity is written.
                    let source_ids: Vec<i64> = plan.0.iter().map(|(item, _)| item.item_id).collect();
                    // A same-location hull output joins the one initial lock plan.
                    let requests: Vec<_> = source_ids.iter().copied().chain(held_hull.then_some(out_location))
                        .map(|item_id| crate::dao::items::parent_locks::LockRequest {
                            item_id, strength: crate::dao::items::parent_locks::LockStrength::Update,
                        }).collect();
                    let locks = inventory::destination::lock_items(tx, &sde, &requests, &[]).await?;
                    if plan.0.iter().any(|(item, _)| !locks.row(item.item_id).is_some_and(|row|
                        row.type_id == item.type_id && row.owner_id == item.owner_id
                        && row.location_id == item.location_id && row.flag_id == item.flag_id
                        && row.quantity == item.quantity && row.singleton == item.singleton
                        && row.jump_clone_id.is_none())) {
                        return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY));
                    }
                    // Authorize the selected output once per attempt; all
                    // materials share this destination, owners included.
                    let destination = inventory::movement::authorize_destination(tx, &sde, &identity, &reach, &container, Some(out_flag)).await?
                        .ok_or_else(refuse)?;
                    if destination.location != out_location || destination.flag != out_flag || destination.item_owner != expected_owner {
                        return Err(refuse());
                    }
                    let blueprints = crate::services::corps::item_locks::locked_blueprints(&sde, &locks, &source_ids)?;
                    crate::services::corps::item_locks::ensure_mutable(tx, &blueprints).await?;
                    use crate::dao::items::quantities::{apply, QuantityChange};
                    let mut changes = BTreeMap::new();
                    for (item, leftover) in &plan.0 {
                        changes.insert(item.item_id, QuantityChange::new(item, (*leftover > 0).then_some(*leftover)));
                    }
                    if !apply(tx, &changes).await.map_err(db_err)? { return Err(notify_error(messages::INSUFFICIENT_ITEM_QUANTITY)); }
                    for (&id, change) in &changes {
                        if change.quantity.is_some() {
                            inventory::notifications::notify_item_change(tx, &sde, outbox, id, vec![(IX_STACK_SIZE, Value::Int(change.expected_quantity))]).await?;
                        } else {
                            let item = plan.0.iter().find(|(item, _)| item.item_id == id).expect("removed item is a source");
                            inventory::notifications::notify_item_removed(outbox, item.0.clone());
                        }
                    }
                    for (&type_id, &quantity) in &plan.1 {
                        if quantity <= 0 { continue; }
                        if held_hull {
                            crate::services::items::deposit_held(tx, &sde, outbox, &destination, type_id, quantity, 0,
                                Some(&locks), inventory::hold_storage::CapacitySource::Current).await?;
                        } else {
                            crate::services::items::deposit(tx, &sde, outbox, &destination, type_id, quantity, 0).await?;
                        }
                    }
                    if tax > 0.0 {
                        let amount = crate::isk::IskAmount::from_isk(tax)?;
                        crate::services::wallets::debit(tx, outbox, char_id, amount).await.map_err(|error| match error {
                            CallError::User { ref msg, .. } if msg == messages::MSG_NOT_ENOUGH_MONEY => notify_error(messages::INSUFFICIENT_ISK_FOR_REPROCESSING_TAX),
                            error => error,
                        })?;
                    }
                    Ok::<_, CallError>(Arc::clone(&plan))
                })
                },
                |value, outbox| async { crate::db::outbox::dispatch_client(value, outbox, c) },
            ).await?;
            let done: Vec<_> = plan.0.iter().map(|(item, _)| Value::Int(item.item_id)).collect();
            info!(user = c.login.user_id, "reprocessed {} items into {} material types ({tax:.0} ISK tax)", done.len(), plan.1.len());
            Ok(Value::tuple([Value::List(done), Value::Dict(plan.1.iter().map(|(&t, &q)| (Value::Int(t as i64), Value::Int(crate::services::dogma::info::cap_client_quantity(q)))).collect())]))
        }
        _ => Ok(Value::None),
    }
}

#[cfg(all(test, feature = "tests-economy"))] mod tests;
