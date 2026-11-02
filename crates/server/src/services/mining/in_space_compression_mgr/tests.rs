use super::*;
use anyhow::{anyhow, ensure, Context as _};
use crate::{consts::dogma::ATTR_COMPRESSIBLE_ITEMS_TYPE_LIST, db::outbox::{Effect, Notification}, services::drones::tests::fixture::Fixture};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

fn call<'a>(method: &'a str, args: &'a [Value], kwargs: &'a Value) -> Call<'a> {
    Call { service: "inSpaceCompressionMgr", method, args, kwargs, bound: false, bind_params: None }
}

#[test]
fn request_requires_exactly_two_positive_integer_ids() {
    let none = Value::None;
    let empty = Value::Dict(vec![]);
    let good = [Value::Int(1), Value::Int(2)];
    assert_eq!(arguments(&call("CompressItemInSpace", &good, &none)).ok(), Some((1, 2)));
    assert_eq!(arguments(&call("CompressItemInSpace", &good, &empty)).ok(), Some((1, 2)));
    for args in [vec![], vec![Value::Int(1)], vec![Value::Int(1), Value::Int(2), Value::Int(3)], vec![Value::Int(0), Value::Int(2)], vec![Value::Int(1), Value::Int(-1)], vec![Value::Bool(true), Value::Int(2)], vec![Value::Int(1), Value::str("2")]] {
        assert!(arguments(&call("CompressItemInSpace", &args, &none)).is_err());
    }
    for kwargs in [Value::Int(1), Value::dict([("x", Value::Int(1))])] {
        assert!(arguments(&call("CompressItemInSpace", &good, &kwargs)).is_err());
    }
}

#[test]
fn eligible_requires_direct_owned_positive_stack_in_supported_hold() {
    let context = Context { character: 11, ship: 22, system: 33 };
    let mut source = Item { item_id: 44, type_id: 1, owner_id: 11, location_id: 22, flag_id: FLAG_CARGO, quantity: 1, singleton: false, group_id: 1, category_id: 1, custom_info: None };
    for flag in [FLAG_CARGO, FLAG_MINING_HOLD, FLAG_ASTEROID_HOLD, FLAG_ICE_HOLD, FLAG_GAS_HOLD] {
        source.flag_id = flag;
        assert!(eligible(&context, &source));
    }
    source.flag_id = FLAG_CARGO;
    let mut bad = source.clone(); bad.owner_id += 1; assert!(!eligible(&context, &bad));
    let mut bad = source.clone(); bad.location_id = source.item_id; assert!(!eligible(&context, &bad));
    let mut bad = source.clone(); bad.location_id = context.system; assert!(!eligible(&context, &bad));
    let mut bad = source.clone(); bad.flag_id = 0; assert!(!eligible(&context, &bad));
    let mut bad = source.clone(); bad.quantity = 0; assert!(!eligible(&context, &bad));
    let mut bad = source.clone(); bad.singleton = true; assert!(!eligible(&context, &bad));
}

#[test]
fn source_guard_tracks_exact_transaction_fields() {
    let source = Item { item_id: 44, type_id: 1, owner_id: 11, location_id: 22, flag_id: FLAG_CARGO, quantity: 1, singleton: false, group_id: 1, category_id: 1, custom_info: None };
    let expected = SourceGuard::from(&source);
    let mut changed = source.clone(); changed.item_id += 1; assert!(SourceGuard::from(&changed) != expected);
    let mut changed = source.clone(); changed.type_id += 1; assert!(SourceGuard::from(&changed) != expected);
    let mut changed = source.clone(); changed.owner_id += 1; assert!(SourceGuard::from(&changed) != expected);
    let mut changed = source.clone(); changed.location_id += 1; assert!(SourceGuard::from(&changed) != expected);
    let mut changed = source.clone(); changed.flag_id += 1; assert!(SourceGuard::from(&changed) != expected);
    let mut changed = source.clone(); changed.quantity += 1; assert!(SourceGuard::from(&changed) != expected);
    let mut changed = source.clone(); changed.singleton = true; assert!(SourceGuard::from(&changed) != expected);
    let mut changed = source.clone(); changed.group_id += 1; changed.category_id += 1; assert!(SourceGuard::from(&changed) == expected);
}

fn static_pair(f: &Fixture) -> anyhow::Result<(i32, i32, i64)> {
    for ty in f.sde.types.iter() {
        let Some(output) = f.sde.compression.get(ty.type_id) else { continue };
        for module in f.sde.types.iter() {
            let Some(list) = module.attribute(ATTR_COMPRESSIBLE_ITEMS_TYPE_LIST) else { continue };
            if list.is_finite() && list > 0.0 && list.fract() == 0.0 && f.sde.typelists.contains(list as i64, ty.type_id, ty.group_id, ty.category_id) {
                return Ok((ty.type_id, output, list as i64));
            }
        }
    }
    Err(anyhow!("real SDE has no supported compression pair"))
}

async fn stack(f: &Fixture, ty: i32, owner: i64, location: i64, flag: i32, quantity: i32) -> anyhow::Result<i64> {
    Ok(transfers::insert(&f.db, ty, owner, location, flag, quantity).await?)
}

async fn rpc(f: &mut Fixture, method: &str, args: &[Value]) -> CallResult {
    let kwargs = Value::None;
    crate::services::dispatch(&mut f.client, &call(method, args, &kwargs)).await
}

async fn install(f: &Fixture, ship: i64, list: i64, generation: Arc<AtomicBool>) -> anyhow::Result<()> {
    f.world.with(f.system, move |s| s.set_compressor(ship, ship, generation, list, 100.0)).await.context("system stopped")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn routed_conversion_holds_merge_replay_and_refusals() -> anyhow::Result<()> {
    let mut f = Fixture::new().await;
    let result: anyhow::Result<()> = async {
        let (source_type, output, list) = static_pair(&f)?;
        let ship = f.ship;
        let generation = Arc::new(AtomicBool::new(false));
        install(&f, f.ship, list, generation.clone()).await?;
        let before = crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await?;
        for (flag, count) in [(FLAG_CARGO, 1), (FLAG_MINING_HOLD, 3), (FLAG_ASTEROID_HOLD, i32::MAX), (FLAG_ICE_HOLD, 5), (FLAG_GAS_HOLD, 7)] {
            let id = stack(&f, source_type, f.owner, f.ship, flag, count).await?;
            let answer = rpc(&mut f, "CompressItemInSpace", &[Value::Int(id), Value::Int(ship)]).await.map_err(|e| anyhow!("compress: {e:?}"))?;
            let decoded = marshal::loads(&marshal::dumps(&answer)).map_err(|e| anyhow!("marshal: {e:?}"))?;
            let fields = decoded.items().context("compression tuple")?;
            ensure!(fields.len() == 6, "incorrect tuple length");
            let output_id = fields[3].as_int().context("output id")?;
            ensure!(decoded == Value::tuple([Value::Int(id), Value::Int(source_type.into()), Value::Int(count.into()), Value::Int(output_id), Value::Int(output.into()), Value::Int(count.into())]), "incorrect tuple: {decoded:?}");
            ensure!(item(&f.db, &f.sde, id).await.map_err(|e| anyhow!("load: {e:?}"))?.is_none(), "source remains");
            let made = item(&f.db, &f.sde, output_id).await.map_err(|e| anyhow!("load: {e:?}"))?.context("output missing")?;
            ensure!((made.owner_id, made.location_id, made.flag_id, made.quantity) == (f.owner, f.ship, flag, count), "wrong output custody");
            ensure!(rpc(&mut f, "CompressItemInSpace", &[Value::Int(id), Value::Int(ship)]).await.is_err(), "replay succeeded");
        }
        let existing = stack(&f, output, f.owner, f.ship, FLAG_CARGO, 8).await?;
        // The existing cargo output from the first conversion is also a merge candidate.
        let source = stack(&f, source_type, f.owner, f.ship, FLAG_CARGO, 3).await?;
        let answer = rpc(&mut f, "CompressItemInSpace", &[Value::Int(source), Value::Int(ship)]).await.map_err(|e| anyhow!("merge: {e:?}"))?;
        let parts = answer.items().context("merge tuple")?;
        ensure!(parts[5].as_int() == Some(3), "reported quantity is not created amount");
        let merged = parts[3].as_int().context("merge id")?;
        let merged_item = item(&f.db, &f.sde, merged).await.map_err(|e| anyhow!("load: {e:?}"))?.context("merged output")?;
        ensure!((merged == existing && merged_item.quantity == 11) || (merged != existing && merged_item.quantity == 4), "wrong merge quantity");
        let source = stack(&f, source_type, f.owner, f.ship, FLAG_GAS_HOLD, 1).await?;
        generation.store(true, Ordering::Release);
        ensure!(rpc(&mut f, "CompressItemInSpace", &[Value::Int(source), Value::Int(ship)]).await.is_err(), "stopped compressor admitted");
        generation.store(false, Ordering::Release);
        for enabled in [Value::Bool(true), Value::Bool(false)] {
            ensure!(rpc(&mut f, "SetAutoCompressionEnabled", &[Value::Int(ship), enabled]).await.is_err(), "automatic compression acknowledged");
        }
        ensure!(rpc(&mut f, "SetAutoCompressionEnabled", &[]).await.is_err(), "malformed automatic compression acknowledged");
        ensure!(rpc(&mut f, "CompressItemInSpace", &[]).await.is_err(), "malformed request admitted");
        ensure!(crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await? == before, "compression changed mining ledger");
        Ok(())
    }.await;
    f.cleanup().await;
    result
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn queued_notifications_rollback_without_dispatch() -> anyhow::Result<()> {
    let f = Fixture::new().await;
    let result: anyhow::Result<()> = async {
        let (source_type, output, _) = static_pair(&f)?;
        let id = stack(&f, source_type, f.owner, f.ship, FLAG_CARGO, 3).await?;
        let source = item(&f.db, &f.sde, id).await.map_err(|e| anyhow!("load: {e:?}"))?.context("source")?;
        let context = Context { character: f.owner, ship: f.ship, system: f.system };
        let expected_source = SourceGuard::from(&source);
        let before = crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await?;
        let called = Arc::new(AtomicBool::new(false));
        let dispatched = called.clone();
        let checked = Arc::new(AtomicBool::new(false));
        let _lock = market::lock().await;
        let result: Result<(), CallError> = f.db.transaction(|tx, outbox: &mut Outbox| {
            let context = context.clone(); let sde = f.sde.clone(); let checked = checked.clone();
            Box::pin(async move {
                compress_tx(tx, &sde, outbox, &context, expected_source).await?;
                let entries = std::mem::take(outbox).into_entries();
                let notifications: Vec<_> = entries.into_iter().filter_map(|entry| match entry { Effect::Notification(Notification::DirectClient { event, args }) => Some((event, args)), _ => None }).collect();
                if notifications.len() != 2 || notifications.iter().any(|(name, _)| name != "OnItemChange") {
                    return Err(CallError::Internal(anyhow!("unexpected compression notifications")));
                }
                let source_change = Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(context.ship))]);
                let output_change = Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(0))]);
                if notifications[0].1.get(1) != Some(&source_change) || notifications[1].1.get(1) != Some(&output_change) {
                    return Err(CallError::Internal(anyhow!("wrong old locations in notifications")));
                }
                checked.store(true, Ordering::Release);
                Err(CallError::Internal(anyhow!("intentional rollback after notifications")))
            })
        }, move |(), _: Outbox| async move { dispatched.store(true, Ordering::Release); Ok(()) }).await;
        ensure!(result.is_err() && checked.load(Ordering::Acquire) && !called.load(Ordering::Acquire), "failed transaction dispatched or did not reach deliberate failure");
        let restored = item(&f.db, &f.sde, id).await.map_err(|e| anyhow!("load: {e:?}"))?.context("source missing after rollback")?;
        ensure!(restored.quantity == 3 && restored.location_id == f.ship, "source changed after rollback");
        ensure!(transfers::stack(&f.db, f.ship, FLAG_CARGO, output, f.owner).await?.is_none(), "output survived rollback");
        ensure!(crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await? == before, "rollback changed ledger");
        Ok(())
    }.await;
    f.cleanup().await;
    result
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn routed_refusals_preserve_inventory_and_stale_helper_refuses() -> anyhow::Result<()> {
    let mut f = Fixture::new().await;
    let result: anyhow::Result<()> = async {
        let (source_type, output, list) = static_pair(&f)?;
        let ship = f.ship;
        let source = stack(&f, source_type, f.owner, ship, FLAG_CARGO, 2).await?;
        ensure!(rpc(&mut f, "CompressItemInSpace", &[Value::Int(source), Value::Int(ship)]).await.is_err(), "missing compressor admitted");
        let generation = Arc::new(AtomicBool::new(false));
        install(&f, ship, list, generation).await?;
        let (other, other_ship) = f.add_pilot().await;
        let foreign = stack(&f, source_type, other, ship, FLAG_CARGO, 2).await?;
        let nested = stack(&f, source_type, f.owner, source, FLAG_CARGO, 2).await?;
        let wrong_hold = stack(&f, source_type, f.owner, ship, 0, 2).await?;
        for id in [foreign, nested, wrong_hold] {
            ensure!(rpc(&mut f, "CompressItemInSpace", &[Value::Int(id), Value::Int(ship)]).await.is_err(), "invalid custody admitted");
            ensure!(item(&f.db, &f.sde, id).await.map_err(|e| anyhow!("load: {e:?}"))?.context("refused item missing")?.quantity == 2, "refused item changed");
        }
        f.client.session.apply(vec![("stationid", Value::Int(1))]);
        ensure!(rpc(&mut f, "CompressItemInSpace", &[Value::Int(source), Value::Int(ship)]).await.is_err(), "docked session admitted");
        f.client.session.apply(vec![("stationid", Value::None), ("shipid", Value::Int(other_ship))]);
        ensure!(rpc(&mut f, "CompressItemInSpace", &[Value::Int(source), Value::Int(ship)]).await.is_err(), "mismatched ship admitted");
        f.client.session.apply(vec![("shipid", Value::Int(ship))]);
        let overflow = stack(&f, output, f.owner, ship, FLAG_CARGO, i32::MAX).await?;
        ensure!(rpc(&mut f, "CompressItemInSpace", &[Value::Int(source), Value::Int(ship)]).await.is_err(), "overflow admitted");
        ensure!(item(&f.db, &f.sde, overflow).await.map_err(|e| anyhow!("load: {e:?}"))?.context("overflow output missing")?.quantity == i32::MAX, "overflow output changed");
        let expected = item(&f.db, &f.sde, source).await.map_err(|e| anyhow!("load: {e:?}"))?.context("source missing")?;
        ensure!(transfers::add(&f.db, source, 1).await?, "source stack missing");
        let context = Context { character: f.owner, ship, system: f.system };
        let mut outbox = Outbox::default();
        ensure!(compress_tx(&f.db, &f.sde, &mut outbox, &context, SourceGuard::from(&expected)).await.is_err(), "stale source admitted");
        ensure!(outbox.into_entries().is_empty(), "stale source produced notices");
        ensure!(item(&f.db, &f.sde, source).await.map_err(|e| anyhow!("load: {e:?}"))?.context("source missing")?.quantity == 3, "refusal changed source");
        Ok(())
    }.await;
    f.cleanup().await;
    result
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn fleet_facility_membership_type_list_and_surface_boundary() -> anyhow::Result<()> {
    let mut f = Fixture::new().await;
    let result: anyhow::Result<()> = async {
        let (source_type, _, list) = static_pair(&f)?;
        let ty = f.sde.types.get(source_type).context("source type")?;
        let wrong_list = f.sde.types.iter().filter_map(|module| module.attribute(ATTR_COMPRESSIBLE_ITEMS_TYPE_LIST))
            .find(|value| value.is_finite() && *value > 0.0 && value.fract() == 0.0 && !f.sde.typelists.contains(*value as i64, source_type, ty.group_id, ty.category_id))
            .context("no compressor list excluding source")? as i64;
        let (other, facility) = f.add_pilot().await;
        let ship = f.ship;
        let generation = Arc::new(AtomicBool::new(false));
        install(&f, facility, list, generation.clone()).await?;
        let id = stack(&f, source_type, f.owner, ship, FLAG_CARGO, 1).await?;
        ensure!(rpc(&mut f, "CompressItemInSpace", &[Value::Int(id), Value::Int(facility)]).await.is_err(), "nonfleet facility admitted");
        crate::services::drones::tests::resources::fleet::form_fleet(&mut f, other, facility).await?;
        install(&f, facility, wrong_list, generation.clone()).await?;
        ensure!(rpc(&mut f, "CompressItemInSpace", &[Value::Int(id), Value::Int(facility)]).await.is_err(), "unsupported type list admitted");
        install(&f, facility, list, generation).await?;
        let a = f.world.ball(f.system, ship).await.context("ship ball")?;
        let b = f.world.ball(f.system, facility).await.context("facility ball")?;
        let edge = 100.0 + f64::from(a.radius) + f64::from(b.radius);
        f.world.with(f.system, move |s| s.set_ball_position(facility, [edge, 0.0, 0.0], |_, _| true)).await.context("system stopped")?;
        ensure!(f.world.after(f.system, crate::space::seconds_to_ms(2.0)).await, "positioning stopped");
        let (source_pos, facility_pos) = f.world.with(f.system, move |s| s.sample_position(ship).zip(s.sample_position(facility))).await.context("system stopped")?.context("position sample missing")?;
        ensure!(source_pos == [0.0, 0.0, 0.0] && facility_pos == [edge, 0.0, 0.0], "boundary positions not settled");
        ensure!(crate::space::dist(source_pos, facility_pos) - f64::from(a.radius) - f64::from(b.radius) == 100.0, "incorrect boundary surface gap");
        rpc(&mut f, "CompressItemInSpace", &[Value::Int(id), Value::Int(facility)]).await.map_err(|e| anyhow!("boundary compression: {e:?}"))?;
        let outside = stack(&f, source_type, f.owner, ship, FLAG_CARGO, 1).await?;
        f.world.with(f.system, move |s| s.set_ball_position(facility, [edge + 1.0, 0.0, 0.0], |_, _| true)).await.context("system stopped")?;
        ensure!(f.world.after(f.system, crate::space::seconds_to_ms(2.0)).await, "positioning stopped");
        let (source_pos, facility_pos) = f.world.with(f.system, move |s| s.sample_position(ship).zip(s.sample_position(facility))).await.context("system stopped")?.context("position sample missing")?;
        ensure!(source_pos == [0.0, 0.0, 0.0] && facility_pos == [edge + 1.0, 0.0, 0.0], "outside positions not settled");
        ensure!(crate::space::dist(source_pos, facility_pos) - f64::from(a.radius) - f64::from(b.radius) == 101.0, "incorrect outside surface gap");
        ensure!(rpc(&mut f, "CompressItemInSpace", &[Value::Int(outside), Value::Int(facility)]).await.is_err(), "outside range admitted");
        ensure!(item(&f.db, &f.sde, outside).await.map_err(|e| anyhow!("load: {e:?}"))?.context("outside source missing")?.quantity == 1, "outside refusal changed source");
        Ok(())
    }.await;
    f.cleanup().await;
    result
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn helper_merge_reports_old_quantity_in_post_commit_outbox() -> anyhow::Result<()> {
    let f = Fixture::new().await;
    let result: anyhow::Result<()> = async {
        let (source_type, output, _) = static_pair(&f)?;
        let id = stack(&f, source_type, f.owner, f.ship, FLAG_CARGO, 3).await?;
        let output_id = stack(&f, output, f.owner, f.ship, FLAG_CARGO, 8).await?;
        let source = item(&f.db, &f.sde, id).await.map_err(|e| anyhow!("load: {e:?}"))?.context("source")?;
        let context = Context { character: f.owner, ship: f.ship, system: f.system };
        let expected_source = SourceGuard::from(&source);
        let _lock = market::lock().await;
        let (value, entries) = f.db.transaction(|tx, outbox: &mut Outbox| {
            let context = context.clone(); let sde = f.sde.clone();
            Box::pin(async move { compress_tx(tx, &sde, outbox, &context, expected_source).await })
        }, |value, outbox: Outbox| async move { Ok::<_, CallError>((value, outbox.into_entries())) }).await.map_err(|e| anyhow!("helper: {e:?}"))?;
        ensure!(value == Value::tuple([Value::Int(id), Value::Int(source_type.into()), Value::Int(3), Value::Int(output_id), Value::Int(output.into()), Value::Int(3)]), "incorrect merge result");
        let mut notices = Vec::new();
        for entry in entries {
            match entry {
                Effect::Notification(Notification::DirectClient { event, args }) => notices.push((event, args)),
                _ => return Err(anyhow!("unexpected compression outbox entry")),
            }
        }
        ensure!(notices.len() == 2 && notices.iter().all(|(event, _)| event == "OnItemChange"), "unexpected compression notifications");
        ensure!(notices[0].1.get(1) == Some(&Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(f.ship))])), "wrong source old location");
        ensure!(notices[1].1.get(1) == Some(&Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int(8))])), "wrong output old quantity");
        ensure!(item(&f.db, &f.sde, output_id).await.map_err(|e| anyhow!("load: {e:?}"))?.context("output")?.quantity == 11, "wrong merged quantity");
        Ok(())
    }.await;
    f.cleanup().await;
    result
}
