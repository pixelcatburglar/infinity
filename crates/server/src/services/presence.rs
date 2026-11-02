//! Process-local pilot and chat presence for distributed session changes.
//! `carbon/common/script/net/machoNet.py:3954-3957` binds the new charid to the
//! client and releases the old one in `ProcessSessionChange`.
//! The caller supplies the complete chat snapshot and notification channel; no database
//! state, transport lifecycle, or cross-process presence is managed here.

use crate::chat::Hub;
use crate::session::ChatSession;
use crate::space::{NotifyTx, World};
use super::identity::Identity;

pub(crate) fn online(identity: &Identity, world: &World, chat: &Hub, notify_tx: NotifyTx, previous: Option<&ChatSession>, current: ChatSession) {
    if previous.map(|s| s.character_id) != Some(identity.char_id) {
        world.register_pilot(identity.char_id, notify_tx);
    }
    world.set_chat_session(identity.char_id, Some(current));
    chat.session_changed(identity.char_id);
}

pub(crate) fn offline(identity: &Identity, world: &World, chat: &Hub) {
    world.unregister_pilot(identity.char_id);
    chat.disconnect_character(identity.char_id);
}

#[cfg(test)]
mod tests;
