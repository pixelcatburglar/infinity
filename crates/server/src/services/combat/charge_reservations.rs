//! Charge stack allocation for clientDogmaLocation reload reservations in game.pending_reloads.
//! Tracks each consumed stack's inventory flag; source-location recovery is handled separately.
use super::*;

pub(super) fn reserve_load(items: &mut BTreeMap<i64, (Item, i64)>, selected: &[i64], type_id: i32, max: i64, have: i64) -> Option<(i64, BTreeMap<(i64, i32), i64>)> {
    let (need, sources) = reserve_quantities(items, selected, type_id, max.saturating_sub(have));
    let now = max - need;
    (now > have).then_some((now, sources))
}

pub(super) fn reserve_quantities(items: &mut BTreeMap<i64, (Item, i64)>, selected: &[i64], type_id: i32, mut need: i64) -> (i64, BTreeMap<(i64, i32), i64>) {
    let mut sources = BTreeMap::new();
    for id in selected {
        let (item, remaining) = items.get_mut(id).unwrap();
        if item.type_id != type_id || need <= 0 { continue; }
        let take = need.min((*remaining).max(0));
        *remaining -= take;
        need -= take;
        if take > 0 { *sources.entry((item.location_id, item.flag_id)).or_default() += take; }
    }
    (need, sources)
}
