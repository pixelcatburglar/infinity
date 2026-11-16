//! Wire-decode and single-batch assertions for ship attribute publications.

use {crate::services::fit::Key, marshal::Value, std::sync::Arc};

/// A pilot notification channel for publication tests.
pub(in crate::space) type NotifyRx = tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>;

/// A registry with the owner (2) and a delegated pilot (3) online, and their channels.
pub(in crate::space) fn pilot_channels() -> (Arc<crate::space::Pilots>, NotifyRx, NotifyRx) {
    let registry = Arc::new(crate::space::Pilots::default());
    let (owner_tx, owner_rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(2, owner_tx);
    let (delegate_tx, delegate_rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(3, delegate_tx);
    (registry, owner_rx, delegate_rx)
}

/// Decode `OnModuleAttributeChanges` batches into (owner, item, attr, new, old).
pub(in crate::space) fn attr_batches(rx: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>) -> Vec<Vec<(i64, i64, i32, f64, f64)>> {
    let mut batches = Vec::new();
    while let Ok((name, args)) = rx.try_recv() {
        if name != "OnModuleAttributeChanges" { continue; }
        let [Value::List(rows)] = args.as_slice() else { panic!("expected attribute batch") };
        let batch: Vec<_> = rows.iter().map(|row| {
            let f = row.items().unwrap();
            (f[1].as_int().unwrap(), f[2].as_int().unwrap(), f[3].as_int().unwrap() as i32,
                f[5].as_float().filter(|v| v.is_finite()).unwrap(),
                f[6].as_float().filter(|v| v.is_finite()).unwrap())
        }).collect();
        assert!(batch.iter().enumerate().all(|(i, r)|
            !batch[..i].iter().any(|p| (p.0, p.1, p.2) == (r.0, r.1, r.2))),
            "duplicate (owner, item, attribute)");
        batches.push(batch);
    }
    batches
}

/// Exactly one pending attribute batch. `expected_key` is `Drone(id)` only
/// for that drone's own rows, or `Ship` for the receiving ship plus every
/// `Module` row the expected changes declare. Every expected old/new is
/// present. Returns the decoded batch.
pub(in crate::space) fn assert_batch(rx: &mut NotifyRx, receiving_item: i64, owner: i64,
    expected_key: Key, changes: &[(Key, i32, f64, f64)]) -> Vec<(i64, i64, i32, f64, f64)> {
    let batches = attr_batches(rx);
    assert_eq!(batches.len(), 1, "expected exactly one attribute publication");
    let batch = batches.into_iter().next().unwrap();
    let item_of = |key: Key| match (expected_key, key) {
        (Key::Drone(id), Key::Drone(row)) if row == id => id,
        (Key::Ship, Key::Ship) => receiving_item,
        (Key::Ship, Key::Module(id)) => id,
        _ => panic!("unexpected key {key:?} for {expected_key:?}"),
    };
    for &(key, attr, old, new) in changes {
        let item = item_of(key);
        assert!(batch.contains(&(owner, item, attr, new, old)), "missing row for attribute {attr}");
    }
    let allowed: Vec<i64> = match expected_key {
        Key::Drone(id) => vec![id],
        Key::Ship => std::iter::once(receiving_item).chain(changes.iter().filter_map(|&(key, ..)|
            match key { Key::Module(id) => Some(id), _ => None })).collect(),
        _ => panic!("expected key must be Drone or Ship"),
    };
    assert!(batch.iter().all(|row| row.0 == owner && allowed.contains(&row.1)),
        "batch rows must belong to the owner and the expected items");
    batch
}
