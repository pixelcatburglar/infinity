//! Shared item authority for the skill item economy.
//! Item sources are the current personal hangar or the active owned ship's cargo.
//! The caller owns serialization, transaction and post-commit dispatch.
//! Timeline mutations reconcile owned online hulls in the settlement transaction.
//! Planetary colonies and clone-grade restrictions are not modelled; all characters
//! follow the existing Omega policy. Dormant clone implants protect prerequisites.
use super::{db_err, CallError};
use crate::{consts::{dogma::ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES, FLAG_CARGO, FLAG_HANGAR, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD}, dao::items::{loading::{Item, ItemQuery}, quantities::{self, QuantityChange}, transfers}, db::{executor::Executor, outbox::Outbox}, messages, sde::Sde, services::{items::load_items, notify_error}};
use marshal::Value;
use std::sync::Arc;
use crate::services::effects::online::{self, OnlineChanged};
use std::collections::BTreeMap;
/// Client appConst.py:2178-2185 skill-trading values.
const BUCKET: i64 = 500_000;
const FREE_ZONE: i64 = 5_000_000;
const LOW_BOUNDARY: i64 = 50_000_000;
const MEDIUM_BOUNDARY: i64 = 80_000_000;
const MAX_STACK: i64 = 10_000;
const SMALL_DIVISOR: i64 = 5;
/// Client appConst.py diminishing reductions, expressed as percentages.
const RATE_DENOMINATOR: i64 = 100;
const LOW_RATE: i64 = 80;
const MEDIUM_RATE: i64 = 60;
const HIGH_RATE: i64 = 30;
/// Client inventorycommon/const.py skill extractor.
const EXTRACTOR: i32 = 40519;
/// Client appConst.py minimum trained points for extraction.
const EXTRACTION_MINIMUM: i64 = 5_500_000;
/// Client inventorycommon/const.py injector type IDs.
const FULL_INJECTOR: i32 = 40520;
const MINI_INJECTOR: i32 = 43630;
const SMALL_INJECTOR: i32 = 45635;
/// Client dogma/const.py attributeContainedSkillPoints.
const CONTAINED_POINTS: i32 = 2461;
/// Client dogma/const.py attributeMaxCharacterSkillPointLimit.
const MAX_CHARACTER_POINTS: i32 = 2459;

struct Credit { item: i64, observed: Option<f64>, uses: i64 }

/// Credits are drawn only from boosters active at the captured time.
async fn credits(tx: &(impl Executor + ?Sized), sde: &Sde, char_id: i64, at: i64) -> Result<Vec<Credit>, CallError> {
    let boosters: Vec<_> = crate::dao::items::fit::brain_items(tx, char_id, at).await.map_err(db_err)?.into_iter().filter(|b| b.flag_id == crate::consts::FLAG_BOOSTER).collect();
    let ids: Vec<_> = boosters.iter().map(|b| b.item_id).collect();
    let overrides = crate::dao::items::dogma::overrides_for(tx, &ids).await.map_err(db_err)?;
    let mut result = Vec::new();
    for item in boosters {
        let observed = overrides.iter().find(|row| row.item_id == item.item_id && row.attribute_id == ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES).map(|row| row.value);
        let value = observed.or_else(|| sde.types.attribute(item.type_id, ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES)).unwrap_or(0.0);
        if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > i64::MAX as f64 { return Err(refusal()); }
        if value > 0.0 { result.push(Credit { item: item.item_id, observed, uses: value as i64 }); }
    }
    Ok(result)
}

/// Available charges use the shared as-of-time brain loader.
pub(crate) async fn available_credits(tx: &(impl Executor + ?Sized), sde: &Sde, char_id: i64, at: i64) -> Result<i64, CallError> {
    credits(tx, sde, char_id, at).await?.iter().try_fold(0_i64, |sum, credit| sum.checked_add(credit.uses).ok_or_else(refusal))
}

/// Server policy: each unit uses the rate at its starting total, rounding down;
/// one charge exempts one full or small injector, never a mini.
fn injector_yield(sde: &Sde, type_id: i32, quantity: i64, mut total: i64, mut charges: i64) -> Result<(i64, i64), CallError> {
    if quantity <= 0 || quantity > MAX_STACK || total < 0 { return Err(refusal()); }
    let (base, mini_limit) = match type_id {
        FULL_INJECTOR | SMALL_INJECTOR => {
            let fallback = if type_id == FULL_INJECTOR { BUCKET } else { BUCKET / SMALL_DIVISOR };
            let value = sde.types.attribute(type_id, CONTAINED_POINTS).unwrap_or(fallback as f64);
            if !value.is_finite() || value <= 0.0 || value.fract() != 0.0 || value >= i64::MAX as f64 { return Err(refusal()); }
            (value as i64, None)
        }
        MINI_INJECTOR => {
            let points = sde.types.attribute(type_id, CONTAINED_POINTS).ok_or_else(refusal)?;
            let limit = sde.types.attribute(type_id, MAX_CHARACTER_POINTS).ok_or_else(refusal)?;
            if !points.is_finite() || !limit.is_finite() || points <= 0.0 || limit <= 0.0 || points.fract() != 0.0 || limit.fract() != 0.0 || points > i64::MAX as f64 || limit > i64::MAX as f64 { return Err(refusal()); }
            (points as i64, Some(limit as i64))
        }
        _ => return Err(refusal()),
    };
    let mut credited = 0_i64;
    let mut used = 0_i64;
    for _ in 0..quantity {
        if mini_limit.is_some_and(|limit| total >= limit) { return Err(refusal()); }
        let points = if type_id == MINI_INJECTOR || total < FREE_ZONE { base } else if charges > 0 {
            charges -= 1; used += 1; base
        } else {
            let rate = if total < LOW_BOUNDARY { LOW_RATE } else if total < MEDIUM_BOUNDARY { MEDIUM_RATE } else { HIGH_RATE };
            (base / RATE_DENOMINATOR) * rate + (base % RATE_DENOMINATOR) * rate / RATE_DENOMINATOR
        };
        credited = credited.checked_add(points).ok_or_else(refusal)?;
        total = total.checked_add(points).ok_or_else(refusal)?;
    }
    Ok((credited, used))
}

pub(super) fn refusal() -> CallError { notify_error(messages::skills::INVALID_ECONOMY_REQUEST) }

pub(super) async fn source(tx: &(impl Executor + ?Sized), sde: &Sde, char_id: i64, ship: Option<i64>, item_id: i64, docked_only: bool) -> Result<Item, CallError> {
    let item = load_items(tx, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refusal)?;
    let station = transfers::docked(tx, char_id).await.map_err(db_err)?;
    let hangar = station.is_some_and(|id| item.location_id == id && item.flag_id == FLAG_HANGAR);
    let cargo = if !docked_only && ship == Some(item.location_id) && item.flag_id == FLAG_CARGO {
        load_items(tx, sde, ItemQuery::Id(item.location_id)).await.map_err(db_err)?.into_iter().any(|s| s.owner_id == char_id)
    } else { false };
    if item.owner_id != char_id || (!hangar && !cargo) || item.quantity <= 0 || !crate::dao::trades::reserved(tx, &[item_id]).await.map_err(db_err)?.is_empty() { return Err(refusal()); }
    Ok(item)
}

pub(super) async fn consume<E>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, item: Item, quantity: i32) -> Result<(), CallError> {
    if quantity <= 0 || quantity > item.quantity { return Err(refusal()); }
    let remaining = item.quantity - quantity;
    let mut changes = BTreeMap::new();
    changes.insert(item.item_id, QuantityChange::new(&item, (remaining > 0).then_some(remaining)));
    if !quantities::apply(tx, &changes).await.map_err(db_err)? { return Err(refusal()); }
    if remaining == 0 {
        let old = item.location_id;
        let gone = Item { location_id: LOCATION_JUNKYARD, ..item };
        outbox.notify_client("OnItemChange", vec![gone.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old))]), Value::None]);
    } else {
        let old = item.quantity;
        let current = Item { quantity: remaining, ..item };
        outbox.notify_client("OnItemChange", vec![current.row(), Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int(i64::from(old)))]), Value::None]);
    }
    Ok(())
}


pub(super) async fn quote(c: &crate::client::Client, char_id: i64, args: &[Value]) -> Result<Value, CallError> {
    let type_id = args.first().and_then(Value::as_int).and_then(|v| i32::try_from(v).ok()).ok_or_else(refusal)?;
    let quantity = args.get(1).and_then(Value::as_int).ok_or_else(refusal)?;
    let now = crate::client::filetime_now();
    let _lock = crate::services::market::lock().await;
    let sde = c.sde.clone();
    c.db.transaction(|tx, _: &mut Outbox| {
        let sde = sde.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
            let plan = super::timeline::plan_tx(tx, &sde, char_id, now).await?;
            let free = crate::dao::characters::attributes::free_points(tx, char_id).await.map_err(db_err)?;
            let total = plan.trained_points()?.checked_add(free).ok_or_else(refusal)?;
            let count = available_credits(tx, &sde, char_id, now).await?;
            Ok(Value::Int(injector_yield(&sde, type_id, quantity, total, count)?.0))
        })
    }, |value, _: Outbox| async move { Ok(value) }).await
}

pub(super) async fn inject(c: &mut crate::client::Client, char_id: i64, args: &[Value], check: bool) -> Result<Value, CallError> {
    let id = args.first().and_then(Value::as_int).ok_or_else(refusal)?;
    let quantity = args.get(1).and_then(Value::as_int).ok_or_else(refusal)?;
    let qty = i32::try_from(quantity).map_err(|_| refusal())?;
    let now = crate::client::filetime_now();
    let ship = c.session.int("shipid");
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone();
        Box::pin(async move {
            inject_tx(tx, outbox, &sde, char_id, ship, id, quantity, qty, now, check).await
        })
    }, |value, outbox| async move { crate::scheduler::wake(); online::dispatch_changed(value, outbox, c) }).await
}

pub(super) async fn convert(c: &mut crate::client::Client, char_id: i64, args: &[Value], split: bool) -> Result<Value, CallError> {
    let id = args.first().and_then(Value::as_int).ok_or_else(refusal)?;
    let quantity = args.get(1).and_then(Value::as_int).ok_or_else(refusal)?;
    let qty = i32::try_from(quantity).map_err(|_| refusal())?;
    let ship = c.session.int("shipid");
    let now = crate::client::filetime_now();
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = sde.clone();
        Box::pin(async move {
            convert_tx(tx, outbox, &sde, char_id, ship, id, quantity, qty, now, split).await
        })
    }, |value, outbox| async move { crate::scheduler::wake(); online::dispatch_changed(value, outbox, c) }).await
}

/// The opaque client token has no local verifier; the session and item authority
/// protect this operation, not token authentication.
pub(super) async fn extract(c: &mut crate::client::Client, char_id: i64, args: &[Value]) -> Result<Value, CallError> {
    let entries = match args.first() { Some(Value::Dict(entries)) => entries, _ => return Err(refusal()) };
    let mut selected = BTreeMap::new();
    for (key, value) in entries {
        let type_id = key.as_int().and_then(|v| i32::try_from(v).ok()).ok_or_else(refusal)?;
        let points = value.as_int().filter(|v| *v > 0).ok_or_else(refusal)?;
        if selected.insert(type_id, points).is_some() { return Err(refusal()); }
    }
    if selected.values().try_fold(0_i64, |sum, points| sum.checked_add(*points)).ok_or_else(refusal)? != BUCKET { return Err(refusal()); }
    let id = args.get(1).and_then(Value::as_int).ok_or_else(refusal)?;
    match args.get(2) {
        Some(Value::None) => return Err(CallError::User { msg: messages::MSG_TOKEN_REQUIRED_FOR_SKILL_EXTRACTION.into(), dict: vec![] }),
        None => return Err(refusal()),
        Some(_) => {}
    }
    let ship = c.session.int("shipid");
    let now = crate::client::filetime_now();
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let selected = selected.clone();
        let sde = sde.clone();
        Box::pin(async move {
            extract_tx(tx, outbox, &sde, char_id, ship, id, now, &selected).await
        })
    }, |value, outbox| async move { crate::scheduler::wake(); online::dispatch_changed(value, outbox, c) }).await
}

async fn extract_tx(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<OnlineChanged>, sde: &Arc<Sde>, char_id: i64, ship: Option<i64>, id: i64, now: i64, selected: &BTreeMap<i32, i64>) -> Result<Value, CallError> {
            use crate::dao::skills::{history, queue, trading, training as skill_dao};
            crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
            if transfers::docked(tx, char_id).await.map_err(db_err)?.is_none() {
                return Err(CallError::User { msg: messages::MSG_SKILL_EXTRACTOR_NOT_DOCKED_IN_STATION.into(), dict: vec![("extractor".into(), Value::Int(i64::from(EXTRACTOR)))] });
            }
            let progress = super::completion::advance_tx(tx, outbox, sde, char_id, now).await?;
            let accrued = super::training::in_training_sp(tx, char_id, now).await?;
            skill_dao::write_skills(tx, char_id, &super::training::plan_skills(&[], accrued)).await.map_err(db_err)?;
            let item = source(tx, &sde, char_id, ship, id, true).await?;
            if item.type_id != EXTRACTOR { return Err(refusal()); }
            let mut skills = skill_dao::current(tx, char_id).await.map_err(db_err)?;
            skills.sort_by_key(|skill| skill.type_id);
            let trained = skills.iter().try_fold(0_i64, |sum, skill| sum.checked_add(skill.skill_points)).ok_or_else(refusal)?;
            if trained < EXTRACTION_MINIMUM || trained - BUCKET < FREE_ZONE {
                return Err(CallError::User { msg: messages::MSG_SKILL_EXTRACTION_NOT_ENOUGH_SP.into(), dict: vec![("limit".into(), Value::Int(BUCKET)), ("extractor".into(), Value::Int(i64::from(EXTRACTOR)))] });
            }
            let queued = queue::list(tx, char_id).await.map_err(db_err)?;
            let mut levels = BTreeMap::new();
            for skill in &skills {
                let removed = selected.get(&skill.type_id).copied().unwrap_or(0);
                if removed > skill.skill_points || (removed > 0 && queued.iter().any(|q| q.type_id == skill.type_id)) { return Err(refusal()); }
                if removed > 0 && !sde.types.get(skill.type_id).is_some_and(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true)) { return Err(refusal()); }
                if removed > 0 && skill.skill_points > super::formulas::sp_for_level(super::training::skill_type(&sde, skill.type_id).rank, i64::from(crate::consts::dogma::MAX_SKILL_LEVEL)) { return Err(refusal()); }
                let remaining = skill.skill_points - removed;
                levels.insert(skill.type_id, super::formulas::level_from_points(remaining, super::training::skill_type(&sde, skill.type_id).rank));
            }
            if selected.keys().any(|id| !levels.contains_key(id)) { return Err(refusal()); }
            // skillExtractorController.py:530-541: each tier protects its clone count.
            let clone_count = crate::dao::characters::clones::clones(tx, char_id).await.map_err(db_err)?.len();
            for (type_id, minimum) in [
                (crate::consts::SKILL_INFOMORPH_PSYCHOLOGY, clone_count.min(5)),
                (crate::consts::SKILL_ADVANCED_INFOMORPH_PSYCHOLOGY, clone_count.saturating_sub(5)),
                (crate::consts::SKILL_ELITE_INFOMORPH_PSYCHOLOGY, clone_count.saturating_sub(10)),
            ] {
                if minimum != 0 && selected.contains_key(&type_id) && levels.get(&type_id).copied().unwrap_or(0) < minimum as i32 { return Err(refusal()); }
            }
            let mut dependents: Vec<i32> = skills.iter().filter(|s| s.skill_points - selected.get(&s.type_id).copied().unwrap_or(0) > 0).map(|s| s.type_id).collect();
            dependents.extend(queued.iter().map(|q| q.type_id));
            dependents.extend(crate::dao::items::fit::brain_items(tx, char_id, now).await.map_err(db_err)?.into_iter().map(|item| item.type_id));
            dependents.extend(crate::dao::items::clones::implant_types(tx, char_id).await.map_err(db_err)?);
            if let Some(ship) = ship {
                let ships = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?;
                if let Some(item) = ships.into_iter().find(|item| item.owner_id == char_id) {
                    dependents.push(item.type_id);
                    dependents.extend(crate::dao::items::fit::modules(tx, ship).await.map_err(db_err)?.into_iter().map(|item| item.type_id));
                }
            }
            let mut visited = std::collections::BTreeSet::new();
            while let Some(dependent) = dependents.pop() {
                if !visited.insert(dependent) { continue; }
                for (required, level) in super::requirements::requirements(&sde, dependent) {
                    if selected.contains_key(&required) && levels.get(&required).copied().unwrap_or(0) < level { return Err(refusal()); }
                    dependents.push(required);
                }
            }
            let location = item.location_id;
            let flag = item.flag_id;
            consume(tx, outbox, item, 1).await?;
            for skill in &skills {
                if let Some(removed) = selected.get(&skill.type_id) {
                    let points = skill.skill_points - removed;
                    let level = levels[&skill.type_id];
                    if !trading::replace_skill(tx, char_id, skill.type_id, skill.level, skill.skill_points, level, points).await.map_err(db_err)? { return Err(refusal()); }
                    history::append(tx, char_id, skill.type_id, super::history::EXTRACTED, points, now).await.map_err(db_err)?;
                }
            }
            let output = transfers::insert(tx, FULL_INJECTOR, char_id, location, flag, 1).await.map_err(db_err)?;
            crate::services::inventory::notify_item_change(tx, &sde, outbox, output, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
            let changed: Vec<_> = selected.keys().copied().collect();
            super::completion::finish_tx(tx, outbox, sde, char_id, now, &progress, &changed).await?;
            Ok(Value::None)
}

async fn inject_tx(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<OnlineChanged>, sde: &Arc<Sde>, char_id: i64, ship: Option<i64>, id: i64, quantity: i64, qty: i32, now: i64, check: bool) -> Result<Value, CallError> {
            crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
            let item = source(tx, &sde, char_id, ship, id, false).await?;
            if qty <= 0 || qty > item.quantity { return Err(refusal()); }
            let plan = super::timeline::plan_tx(tx, sde, char_id, now).await?;
            let balance = crate::dao::characters::attributes::free_points(tx, char_id).await.map_err(db_err)?;
            let total = plan.trained_points()?.checked_add(balance).ok_or_else(refusal)?;
            let mut credits = credits(tx, sde, char_id, now).await?;
            let count = credits.iter().try_fold(0_i64, |sum, c| sum.checked_add(c.uses).ok_or_else(refusal))?;
            let (amount, mut used) = injector_yield(&sde, item.type_id, quantity, total, count)?;
            if check { return Ok(Value::None); }
            let progress = super::completion::apply_plan_tx(tx, outbox, sde, &plan).await?;
            let accrued = super::training::in_training_sp(tx, char_id, now).await?;
            crate::dao::skills::training::write_skills(tx, char_id, &super::training::plan_skills(&[], accrued)).await.map_err(db_err)?;
            let new_balance = balance.checked_add(amount).ok_or_else(refusal)?;
            consume(tx, outbox, item, qty).await?;
            if !crate::dao::characters::attributes::replace_free_points(tx, char_id, balance, new_balance).await.map_err(db_err)? { return Err(refusal()); }
            let total_used = used;
            for credit in &mut credits {
                let take = used.min(credit.uses);
                if take > 0 {
                    if !crate::dao::items::skills::spend_credit(tx, credit.item, ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES, credit.observed, (credit.uses - take) as f64).await.map_err(db_err)? { return Err(refusal()); }
                    used -= take;
                }
            }
            if total_used > 0 { outbox.notify_pilot(char_id, "OnNonDiminishingInjectionsUsed", vec![Value::Int(total_used)]); }
            outbox.notify_client("OnFreeSkillPointsChanged", vec![Value::Int(new_balance)]);
            super::completion::finish_tx(tx, outbox, sde, char_id, now, &progress, &[]).await?;
            Ok(Value::None)
}

async fn convert_tx(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<OnlineChanged>, sde: &Arc<Sde>, char_id: i64, ship: Option<i64>, id: i64, quantity: i64, qty: i32, now: i64, split: bool) -> Result<Value, CallError> {
            crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
            let progress = super::completion::advance_tx(tx, outbox, sde, char_id, now).await?;
            let accrued = super::training::in_training_sp(tx, char_id, now).await?;
            crate::dao::skills::training::write_skills(tx, char_id, &super::training::plan_skills(&[], accrued)).await.map_err(db_err)?;
            let item = source(tx, &sde, char_id, ship, id, false).await?;
            if qty <= 0 || qty > item.quantity || item.type_id != if split { FULL_INJECTOR } else { SMALL_INJECTOR } { return Err(refusal()); }
            if !split && quantity < SMALL_DIVISOR {
                return Err(CallError::User { msg: messages::MSG_COMBINE_SKILL_INJECTOR_TOO_FEW_INJECTORS.into(), dict: vec![("minQuantity".into(), Value::Int(SMALL_DIVISOR)), ("injectorType".into(), Value::Int(i64::from(SMALL_INJECTOR)))] });
            }
            let (target, count) = if split {
                (SMALL_INJECTOR, quantity.checked_mul(SMALL_DIVISOR).ok_or_else(refusal)?)
            } else {
                (FULL_INJECTOR, quantity / SMALL_DIVISOR)
            };
            let count = i32::try_from(count).map_err(|_| refusal())?;
            let location = item.location_id;
            let flag = item.flag_id;
            // skillsvc.py:1027-1030 permits nonmultiples; retain unused small injectors.
            let consumed = if split { qty } else { count * SMALL_DIVISOR as i32 };
            consume(tx, outbox, item, consumed).await?;
            let output = transfers::insert(tx, target, char_id, location, flag, count).await.map_err(db_err)?;
            crate::services::inventory::notify_item_change(tx, &sde, outbox, output, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
            super::completion::finish_tx(tx, outbox, sde, char_id, now, &progress, &[]).await?;
            Ok(Value::None)
}

#[cfg(test)] mod tests;
