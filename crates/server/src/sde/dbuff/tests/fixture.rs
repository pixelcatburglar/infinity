//! Shared synthetic dbuff collections for database-free tests.
//! Callers supply the native JSON rows; the production parser compiles them.

use super::Dbuffs;

/// Compile caller-supplied native collection rows through the production parser.
pub(crate) fn collections(rows: Vec<crate::dao::sde::dbuff::Row>) -> anyhow::Result<Dbuffs> {
    Dbuffs::from_rows(rows)
}
