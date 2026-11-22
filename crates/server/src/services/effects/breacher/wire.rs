//! Wire shapes for breacher damage-over-time notifications and slim fields.
//! Mirrors the client's `dotWeapons/common/` protocol: `OnDotVictimUpdated`
//! (eve/client/script/remote/michelle.py) carries a Python `set` of
//! `DotApplicationInfo` instances, and the slim field name comes from
//! `dotRegistry.py`'s SLIM_ITEM_DOT_ATTR_NAME.
use super::{DotApplication, DOT_ACTIVE, DOT_IDLE};
use marshal::Value;

/// Client dotWeapons/common/dotRegistry.py: SLIM_ITEM_DOT_ATTR_NAME.
pub(super) const SLIM_DOT_FIELD: &str = "dotHpPercentageApplied";

/// `OnDotVictimUpdated` args: [targetID, set(DotApplicationInfo)]; an empty
/// slice encodes the clearing update.
pub(super) fn victim_update(target: i64, applications: &[DotApplication]) -> Vec<Value> {
    let items = applications.iter().map(|app| application_info(target, app));
    vec![Value::Int(target), Value::reduce("__builtin__.set", vec![Value::list(items)], None)]
}

/// One `dotWeapons.common.dotAppInfo.DotApplicationInfo` via the reduce
/// protocol with its `_`-prefixed state dict (the class takes no positional
/// arguments; whitelist.py admits it for unpickling).
fn application_info(target: i64, app: &DotApplication) -> Value {
    Value::reduce("dotWeapons.common.dotAppInfo.DotApplicationInfo", vec![], Some(Value::dict([
        ("_attackerID", app.attacker.map(Value::Int).unwrap_or(Value::None)),
        ("_weaponTypeID", Value::Int(i64::from(app.weapon_type))),
        ("_expiryTime", Value::Int(app.expires_ft)),
        ("_maxDamage", Value::Float(app.max_damage)),
        ("_maxHPNormalizedRatio", Value::Float(app.max_hp_fraction)),
        ("_activityState", Value::Int(if app.active { DOT_ACTIVE } else { DOT_IDLE })),
        ("_targetID", Value::Int(target)),
    ])))
}
