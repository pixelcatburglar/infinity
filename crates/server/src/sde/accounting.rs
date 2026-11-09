//! Validated accounting entry definitions from the native SDE, loaded once for
//! eve/client/script/ui/services/accountsvc.py and its entrytypes catalogue.
//! Missing or malformed definitions are rejected rather than synthesized.
use std::collections::BTreeMap;
use anyhow::{bail, Result};
use infinity_core::{consts::{REF_MARKET_TRANSACTION, REF_MARKET_ESCROW, REF_BROKER_FEE, REF_TRANSACTION_TAX, REF_MARKET_SECURITY_TAX}};
use crate::db::Db;
use crate::dao::sde::accounting::EntryType;

#[cfg_attr(test, derive(Default))]
pub(crate) struct Accounting { entries:BTreeMap<i32,EntryType> }
impl Accounting {
    pub(crate) async fn load(db:&Db)->Result<Self> { Self::from_rows(crate::dao::sde::accounting::load(db).await?) }
    fn from_rows(rows:Vec<EntryType>)->Result<Self> {
        let mut entries = BTreeMap::new();
        for entry in rows {
            if entry.name_id <= 0 { bail!("invalid accounting entry name ID"); }
            if entries.insert(entry.id,entry).is_some() { bail!("duplicate accounting entry type"); }
        }
        for id in [REF_MARKET_TRANSACTION,REF_MARKET_ESCROW,REF_BROKER_FEE,REF_TRANSACTION_TAX,REF_MARKET_SECURITY_TAX] {
            if !entries.contains_key(&id) { bail!("missing accounting entry type {id}"); }
        }
        Ok(Self { entries })
    }
    pub(crate) fn iter(&self)->impl Iterator<Item=&EntryType> { self.entries.values() }
}
#[cfg(test)]
mod tests;
