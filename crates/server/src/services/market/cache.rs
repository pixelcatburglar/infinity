//! Encoding of objectCaching.CachedMethodCallResult for coherent market snapshots.
//! The client unmarshals the inner result and compares the revision/expiry token;
//! no server-side cache or invalidation messages are maintained here.
use crate::dao::market::versions::BookSnapshot;
use marshal::Value;

pub(super) fn wrap(inner: Value, snapshot: &BookSnapshot, regional_session: bool) -> Value {
    let mut details = vec![(Value::str("versionCheck"), Value::str("always"))];
    if regional_session {
        details.push((Value::str("sessionInfo"), Value::str("regionid")));
    }
    let version = Value::list([
        Value::Int(snapshot.changed_at.max(snapshot.latest_crossed_expiry.unwrap_or(0))),
        Value::tuple([Value::Int(snapshot.revision), snapshot.next_visible_expiry.map(Value::Int).unwrap_or(Value::None)]),
    ]);
    Value::Instance(
        Box::new(Value::str("carbon.common.script.net.objectCaching.CachedMethodCallResult")),
        Box::new(Value::tuple([Value::Dict(details), Value::Bytes(marshal::dumps(&inner)), version])),
    )
}

#[cfg(test)]
mod tests;
