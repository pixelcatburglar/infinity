//! Player-facing messages for Lua scripts and simulated pilot operations.
//! The client receives these via slash responses and CustomNotify; no data is read here.

pub(crate) const LUA_MOTD_HELP: &str = "用法：/motd 或 /motd set <公告>";
pub(crate) const LUA_MOTD_SAVED: &str = "公告已保存。";
pub(crate) const LUA_MOTD_EMPTY: &str = "暂无公告。";
pub(crate) const PILOT_OFFLINE: &str = "模拟飞行员不在线。";
pub(crate) const PILOT_NO_DEFAULT_EFFECT: &str = "该物品没有可用的默认效果。";
