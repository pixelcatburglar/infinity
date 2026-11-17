//! Player-visible text for the ordinary `crimewatch` service methods
//! (`eve/client/script/ui/services/crimewatchSvc.py`): the bound
//! `SetSafetyLevel` call and the character lookups it and `GetClientStates`
//! perform. GM `/cwatch` and `/naughty` replies live in `messages::gm`.

/// `SetSafetyLevel` received a non-integer argument, the wrong number of
/// arguments, or a value outside the client's 0..=2 safety enum.
pub const SAFETY_LEVEL_INVALID: &str = "安全等级无效。";
/// The calling character's row is missing or deleted.
pub const CHARACTER_UNAVAILABLE: &str = "当前角色不可用。";

/// A timer duration was negative, or could not be converted into a deadline.
pub const DURATION_INVALID: &str = "无效的时长。";
