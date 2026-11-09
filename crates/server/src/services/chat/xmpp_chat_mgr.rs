//! XmppChatMgr RPCs used by `xmppchatclient/xmppchatsvc.py` and
//! `xmppchannelsettings.py`, backed by session facts and the authority over
//! `game.chat_channels`, `game.chat_channel_members` and `game.chat_system_motds`.
//! Occupants and messages remain runtime transport state. Passwords, expiring
//! restrictions, corporation/alliance ACL grants and proxy-room ownership are unsupported.
use super::{authority, super::{Call, CallResult, notify_error}};
use infinity_core::{messages::chat as text, session::OnlineSession};
use crate::client::Client;
use marshal::Value;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let session = OnlineSession::from_session(&c.session);
    let arg_text = |n: usize| call.args.get(n).and_then(Value::as_text).unwrap_or_default().into_owned();
    let arg_id = |n: usize| call.args.get(n).and_then(Value::as_int).unwrap_or(0);
    let result = match call.method {
        "Hostname" => Ok(Value::str(&c.cfg.chat.domain)),
        "GetDeprecatedPrefsFallback" => Ok(Value::str(&c.cfg.server.public_host)),
        "GetHelpChannel" => Ok(Value::str(authority::help(call.args.first().and_then(Value::as_text).as_deref()))),
        "GetRookieChannel" => Ok(Value::str(authority::ROOKIE)),
        "ResyncSystemChannelAccess" => Ok(Value::list(session.as_ref().map(|s| authority::system_channels(&c.sde, s)).unwrap_or_default().into_iter().map(Value::str))),
        "CreatePlayerOwnedChannel" => match session { Some(s) => authority::create_player(&c.db, s.character_id, &arg_text(0)).await.map(Value::str), None => Err(notify_error(text::FORBIDDEN)) },
        "CreatePrivateChannel" => match session { Some(s) => authority::create_private(&c.db, s.character_id, arg_id(0)).await.map(Value::str), None => Err(notify_error(text::FORBIDDEN)) },
        "AddCharacterToChannel" => match session { Some(s) => authority::invite(&c.db, &c.chat, &s, &arg_text(0), arg_id(1)).await.map(|()| Value::None), None => Err(notify_error(text::FORBIDDEN)) },
        // This caller-only recovery RPC cannot authorize a transfer from a different owner.
        "GrantChannelOwnership" => match session { Some(s) => match authority::affiliations(&c.db, &s, &arg_text(0)).await { Ok(entries) if entries.iter().any(|entry| entry.character_id == s.character_id && entry.affiliation == "owner") => Ok(Value::None), Ok(_) => Err(notify_error(text::FORBIDDEN)), Err(e) => Err(e) }, None => Err(notify_error(text::FORBIDDEN)) },
        "SetMotd" => match session {
            Some(s) => {
                let jid = arg_text(0);
                let suffix = format!("@conference.{}", c.cfg.chat.domain);
                match jid.strip_suffix(&suffix).filter(|room| !room.is_empty() && !room.contains('@') && !room.contains('/')) {
                    Some(room) => authority::system_motd(&c.db, &c.chat, &s, room, &arg_text(1)).await.map(|()| Value::None),
                    None => Err(notify_error(text::INVALID)),
                }
            }
            None => Err(notify_error(text::FORBIDDEN)),
        },
        _ => return None,
    };
    Some(result)
}
