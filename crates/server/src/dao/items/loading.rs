//! Inventory item row loading from `game.items`.
//! Quantity-ordered queries use quantity and item ID; other queries use item ID.
//! Dormant clone implants are excluded from all ordinary item queries.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Transactions, refusals, notifications and World are caller concerns.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

#[derive(Clone)]
pub(crate) struct Item {
    pub item_id: i64,
    pub type_id: i32,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i32,
    pub quantity: i32,
    pub singleton: bool,
    pub group_id: i64,
    pub category_id: i64,
    pub custom_info: Option<String>,
}

pub(crate) struct GameItem {
    pub item_id: i64, pub type_id: i32, pub owner_id: i64, pub location_id: i64,
    pub flag_id: i32, pub quantity: i32, pub singleton: bool, pub custom_info: Option<String>,
}

impl GameItem {
    fn from_row(r: &Row) -> Self {
        Self {
            item_id: r.get("item_id"), type_id: r.get("type_id"), owner_id: r.get("owner_id"),
            location_id: r.get("location_id"), flag_id: r.get("flag_id"), quantity: r.get("quantity"),
            singleton: r.get("singleton"), custom_info: r.get("custom_info"),
        }
    }
}

/// The existing item predicates, with parameters in their original SQL order.
pub(crate) enum ItemQuery<'a> {
    Id(i64), Ids(&'a [i64]), IdAt(i64, i64),
    IdsOwnerLocations(&'a [i64], i64, i64, i64),
    Location(i64), IdOrLocation(i64), LocationFlags(i64, &'a [i32]), LocationOwnerFlags(i64, i64, &'a [i32]),
    LocationOwner(i64, i64), Owner(i64), OwnerExceptSelf(i64), LocationExceptFlag(i64, i32),
    IdOrLocationExceptFlag(i64, i32), OwnerLocation(i64, i64), OwnerLocationFlag(i64, i64, i32),
    LocationFlagTypeOwnerStack(i64, i32, i32, i64),
    OwnerLocationFlagTypeDescending(i64, i64, i32, i32),
    OwnerLocationFlagTypeStackAscending(i64, i64, i32, i32),
    OwnerTypeTwoLocations(i64, i32, i64, i32, i64, i32),
}

pub(crate) async fn load_items(ex: &(impl Executor + ?Sized), query: ItemQuery<'_>) -> Result<Vec<GameItem>, db::Error> {
    use ItemQuery::*;
    let filter = match &query {
        Id(..) => "i.item_id = $1",
        Ids(..) => "i.item_id = ANY($1)",
        IdAt(..) => "i.item_id = $1 AND i.location_id = $2",
        IdsOwnerLocations(..) => "i.item_id = ANY($1) AND i.owner_id = $2 AND (i.location_id = $3 OR i.location_id = $4)",
        Location(..) => "i.location_id = $1",
        IdOrLocation(..) => "i.item_id = $1 OR i.location_id = $1",
        LocationFlags(..) => "i.location_id = $1 AND i.flag_id = ANY($2)",
        LocationOwnerFlags(..) => "i.location_id = $1 AND i.owner_id = $2 AND i.flag_id = ANY($3)",
        LocationOwner(..) => "i.location_id = $1 AND i.owner_id = $2",
        Owner(..) => "i.owner_id = $1",
        OwnerExceptSelf(..) => "i.owner_id = $1 AND i.item_id <> $1",
        LocationExceptFlag(..) => "i.location_id = $1 AND i.flag_id <> $2",
        IdOrLocationExceptFlag(..) => "i.item_id = $1 OR (i.location_id = $1 AND i.flag_id <> $2)",
        OwnerLocation(..) => "i.owner_id = $1 AND i.location_id = $2",
        OwnerLocationFlag(..) => "i.owner_id = $1 AND i.location_id = $2 AND i.flag_id = $3",
        LocationFlagTypeOwnerStack(..) => "i.location_id = $1 AND i.flag_id = $2 AND i.type_id = $3 AND i.owner_id = $4 AND NOT i.singleton",
        OwnerLocationFlagTypeDescending(..) => "i.owner_id = $1 AND i.location_id = $2 AND i.flag_id = $3 AND i.type_id = $4",
        OwnerLocationFlagTypeStackAscending(..) => "i.owner_id = $1 AND i.location_id = $2 AND i.flag_id = $3 AND i.type_id = $4 AND NOT i.singleton",
        OwnerTypeTwoLocations(..) => "i.owner_id = $1 AND i.type_id = $2 AND ((i.location_id = $3 AND i.flag_id = $4) OR (i.location_id = $5 AND i.flag_id = $6))",
    };
    let order = match &query {
        OwnerLocationFlagTypeDescending(..) => "i.quantity DESC, i.item_id",
        OwnerLocationFlagTypeStackAscending(..) => "i.quantity, i.item_id",
        _ => "i.item_id",
    };
    let sql = format!("SELECT i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton,
                        i.custom_info FROM game.items i WHERE ({filter}) AND i.jump_clone_id IS NULL ORDER BY {order}");
    let rows = match &query {
        Id(a) | Location(a) | IdOrLocation(a) | Owner(a) | OwnerExceptSelf(a) => ex.query(&sql, &[a]).await?,
        Ids(a) => ex.query(&sql, &[a]).await?,
        IdAt(a,b) | LocationOwner(a,b) | OwnerLocation(a,b) => ex.query(&sql, &[a,b]).await?,
        IdsOwnerLocations(a,b,c,d) => ex.query(&sql, &[a,b,c,d]).await?,
        LocationFlags(a,b) => ex.query(&sql, &[a,b]).await?,
        LocationOwnerFlags(a,b,c) => ex.query(&sql, &[a,b,c]).await?,
        LocationExceptFlag(a,b) | IdOrLocationExceptFlag(a,b) => ex.query(&sql, &[a,b]).await?,
        OwnerLocationFlag(a,b,c) => ex.query(&sql, &[a,b,c]).await?,
        LocationFlagTypeOwnerStack(a,b,c,d) => ex.query(&sql, &[a,b,c,d]).await?,
        OwnerLocationFlagTypeDescending(a,b,c,d) | OwnerLocationFlagTypeStackAscending(a,b,c,d) => ex.query(&sql, &[a,b,c,d]).await?,
        OwnerTypeTwoLocations(a,b,c,d,e,f) => ex.query(&sql, &[a,b,c,d,e,f]).await?,
    };
    Ok(rows.iter().map(GameItem::from_row).collect())
}
