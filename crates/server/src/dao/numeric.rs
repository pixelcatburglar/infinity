//! Plain numeric(36,2) ISK text adapted to `db::Error::Progress`.
//! Market and daily-goal rows call this after their own column reads; there is
//! no direct client mirror or query here. Domain refusals and transactions stay
//! with the owning services.
use crate::{db, isk::IskAmount};

pub(super) fn parse_isk(value: &str) -> Result<IskAmount, db::Error> {
    IskAmount::parse_decimal(value).map_err(|e| db::Error::Progress(e.into()))
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
