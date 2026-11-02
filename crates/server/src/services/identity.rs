//! Character identity from the server-side session.
//! `carbon/common/script/net/machoNet.py:3954-3957` binds the new charid to the
//! client and releases the old one in `ProcessSessionChange`. The character ID
//! comes from the session, not the database; this snapshot carries neither transport
//! state nor location, roles, or affiliations.

use crate::client::Client;
use crate::session::Session;

pub struct Identity {
    pub char_id: i64,
}

impl Identity {
    pub fn of_client(c: &Client) -> Self {
        Self::from_session(&c.session)
    }

    fn from_session(session: &Session) -> Self {
        Self {
            char_id: session.int("charid").unwrap_or(0),
        }
    }
}
