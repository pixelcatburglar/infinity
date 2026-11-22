//! Pure stack merge planning for
//! `eve/client/script/environment/invCache.py`.
//! Item rows come from `game.items` through the caller's already loaded snapshots
//! and type limits from the SDE. Planning does not read or write the database;
//! capture, locks, authorization and quantity writes stay with the caller.
//! Persistence is unchanged.

use crate::dao::items::quantities::QuantityChange;

use super::*;

/// Simulate transfers in request order, retaining the original snapshots for guarded writes.
pub(in crate::services::inventory) fn plan_merges(
    items: &[crate::dao::items::loading::Item],
    ops: &[(i64, i64, i64)],
) -> Result<std::collections::BTreeMap<i64, crate::dao::items::quantities::QuantityChange>, CallError> {
    let mut states: BTreeMap<i64, (&Item, Option<i64>)> = items.iter().map(|item| (item.item_id, (item, Some(item.quantity)))).collect();
    for &(source, dest, requested) in ops {
        if source == dest { continue; }
        let (Some(&(src, Some(src_qty))), Some(&(dst, Some(dst_qty)))) = (states.get(&source), states.get(&dest)) else { continue };
        if src.type_id != dst.type_id || src.singleton || dst.singleton || src_qty <= 0 { continue; }
        if (src.owner_id, src.location_id, src.flag_id) != (dst.owner_id, dst.location_id, dst.flag_id) { return Err(notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)); }
        let take = i128::from(requested.clamp(1, src_qty)).min(i128::from(infinity_core::consts::MAX_STACK_QUANTITY) - i128::from(dst_qty)) as i64;
        let total = dst_qty + take;
        states.get_mut(&source).unwrap().1 = if take == src_qty { None } else { Some(src_qty - take) };
        states.get_mut(&dest).unwrap().1 = Some(total);
    }
    Ok(states.into_iter().filter_map(|(id, (item, quantity))| {
        (quantity != Some(item.quantity)).then(|| (id, QuantityChange::new(item, quantity)))
    }).collect())
}

/// Item ids of unassembled stacks that share owner, type and flag with another snapshot.
pub(in crate::services::inventory) fn stack_write_ids(items: &[Item]) -> Vec<i64> {
    let mut groups = BTreeMap::<(i64, i32, i32), usize>::new();
    for item in items.iter().filter(|item| !item.singleton) {
        *groups.entry((item.owner_id, item.type_id, item.flag_id)).or_default() += 1;
    }
    items.iter().filter(|item| !item.singleton
        && groups[&(item.owner_id, item.type_id, item.flag_id)] > 1).map(|item| item.item_id).collect()
}

/// Fill each group's first destination, then switch the remainder onto the next source.
pub(in crate::services::inventory) fn stack_all_ops(items: &[Item]) -> Vec<(i64, i64, i64)> {
    let mut first: std::collections::HashMap<(i64, i32, i32), (i64, i64)> = Default::default();
    let mut ops = Vec::new();
    for item in items.iter().filter(|i| !i.singleton) {
        let key = (item.owner_id, item.type_id, item.flag_id);
        if let Some((dest, quantity)) = first.get_mut(&key) {
            if item.quantity <= 0 { continue; }
            let take = i128::from(item.quantity).min(i128::from(infinity_core::consts::MAX_STACK_QUANTITY) - i128::from(*quantity)) as i64;
            if take > 0 { ops.push((item.item_id, *dest, take)); }
            *quantity += take;
            if take < item.quantity { *dest = item.item_id; *quantity = item.quantity - take; }
        } else {
            first.insert(key, (item.item_id, item.quantity));
        }
    }
    ops
}
