//! Character identity from the server-side session.
//! `carbon/common/script/net/machoNet.py:3954-3957` binds the new charid to the
//! client and releases the old one in `ProcessSessionChange`. The character ID
//! comes from the session, not the database; this snapshot carries no transport
//! state or roles.
//! Character and corporation IDs of zero mean the respective identity is absent.

use crate::client::Client;
use crate::session::Session;

#[derive(Clone, Copy)]
pub struct Identity {
    pub char_id: i64,
    pub ship_id: Option<i64>,
    pub corp_id: i64,
    pub station_id: Option<i64>,
    pub system_id: Option<i64>,
}

impl Identity {
    pub fn of_client(c: &Client) -> Self {
        Self::from_session(&c.session)
    }

    fn from_session(session: &Session) -> Self {
        Self {
            char_id: session.int("charid").unwrap_or(0),
            ship_id: session.int("shipid"),
            corp_id: session.int("corpid").unwrap_or(0),
            station_id: session.int("stationid"),
            system_id: session.int("solarsystemid2"),
        }
    }
}
