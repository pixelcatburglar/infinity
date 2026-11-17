//! Immediate GM announcements through game notifications. The client handles
//! `OnRemoteMessage` in `eve/client/script/parklife/gameui.py`.
//! Targets come from the World notification registry, not a database; announcements
//! are not persisted or sent to offline pilots, and have no delivery or read acknowledgement.

use super::{slash_error, CallError, Client};
use infinity_core::messages;
use marshal::Value;

pub(super) fn command(c: &Client, args: &[String]) -> Result<String, CallError> {
    let joined = args.join(" ");
    let text = joined.trim();
    if text.is_empty() {
        return Err(slash_error(messages::gm::GM_ANNOUNCE_USAGE.into()));
    }
    let targets = c.world.online_pilots();
    let count = targets.len() as i64;
    if let Some((last, rest)) = targets.split_last() {
        let full = messages::gm::gm_announcement(text);
        let payload = vec![
            Value::str(messages::MSG_CUSTOM_NOTIFY),
            Value::dict([("notify", Value::unicode(full))]),
        ];
        for target in rest {
            c.world.notify_pilot(*target, "OnRemoteMessage", payload.clone());
        }
        c.world.notify_pilot(*last, "OnRemoteMessage", payload);
    }
    Ok(messages::gm::gm_announce_queued(count))
}

#[cfg(all(test, feature = "tests-slash"))] mod tests;
