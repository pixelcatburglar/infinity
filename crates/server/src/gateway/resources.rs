//! Database access for handlers serving `requestBroker.py:_send_request`.
//! Uses the gateway's optional DAO database resource, not World; an absent
//! database remains an error rather than supplying fallback data.
use anyhow::{Context, Result};
use super::Gateway;

pub(super) fn db(gw: &Gateway) -> Result<&crate::db::Db> {
    gw.db.as_ref().context("gateway has no database")
}
