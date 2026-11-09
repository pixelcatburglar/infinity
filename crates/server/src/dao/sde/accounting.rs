//! Loads accounting entry names from sde_native.accountingentrytypes for the account
//! service mirrored by eve/client/script/ui/services/accountsvc.py and entrytypes.
//! This loader does not synthesize missing definitions.
use crate::db::{self, executor::Executor};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EntryType { pub(crate) id:i32, pub(crate) name_id:i32 }
impl EntryType {
    fn from_row(row:&tokio_postgres::Row)->Result<Self,db::Error> {
        Ok(Self { id:row.try_get("id")?, name_id:row.try_get("name_id")? })
    }
}
pub(crate) async fn load(ex:&(impl Executor + ?Sized))->Result<Vec<EntryType>,db::Error> {
    let rows = ex.query("SELECT key::integer AS id, (data->>'entryTypeNameID')::integer AS name_id FROM sde_native.accountingentrytypes ORDER BY key::integer", &[]).await?;
    rows.iter().map(EntryType::from_row).collect()
}
