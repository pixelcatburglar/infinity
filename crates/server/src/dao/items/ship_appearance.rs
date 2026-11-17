//! Joined physical-hull identity, name and appearance reads in `game.items`,
//! `game.ship_dirt` and `game.ship_kill_counts`. The ship service supplies the
//! client `cleanShipButton.py`/`shipKillCounter` protocols and destiny slim
//! items. Physical custody (singleton, quantity 1,
//! no jump clone) is part of the row predicate; ship-category validation is the
//! caller's SDE concern, not a static join. Timestamps and kill counts follow
//! the item, not its owner; transactions and authorization are caller concerns.
//! This DAO does not emit notifications or apply bay configuration policy.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;
use super::loading::GameItem;

/// The persisted physical custody of a hull, compared on guarded writes and
/// against the actor fence before a live slim update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ObservedHull {
    pub item_id: i64,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i32,
    pub type_id: i32,
}

impl From<&GameItem> for ObservedHull {
    fn from(item: &GameItem) -> Self {
        Self { item_id: item.item_id, owner_id: item.owner_id, location_id: item.location_id, flag_id: item.flag_id, type_id: item.type_id }
    }
}

/// One physical hull with its verbatim item name and persisted appearance; a
/// `None` timestamp means the hull has no `game.ship_dirt` row yet, and a `None`
/// name is the stored NULL, distinct from an empty name; fallback is the
/// caller's concern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredAppearance {
    pub hull: ObservedHull,
    pub name: Option<String>,
    pub dirt_timestamp: Option<i64>,
    /// Persisted player-kill count; a missing `game.ship_kill_counts` row is 0.
    pub player_kills: i64,
}

impl StoredAppearance {
    fn from_row(row: &Row) -> Self {
        Self {
            hull: ObservedHull {
                item_id: row.get("item_id"), owner_id: row.get("owner_id"), location_id: row.get("location_id"),
                flag_id: row.get("flag_id"), type_id: row.get("type_id"),
            },
            name: row.get("name"),
            dirt_timestamp: row.get("dirt_timestamp"),
            player_kills: row.get("player_kills"),
        }
    }
}

/// One joined read of the physical hull, its item name, persisted dirt
/// timestamp and player-kill count.
/// Never initializes a missing row; initialization is the caller's choice.
pub(crate) async fn load(ex: &(impl Executor + ?Sized), ship: i64) -> Result<Option<StoredAppearance>, db::Error> {
    Ok(ex.query_opt("SELECT i.item_id,i.owner_id,i.location_id,i.flag_id,i.type_id,i.name,d.dirt_timestamp,COALESCE(k.player_kills,0::bigint) AS player_kills FROM game.items i LEFT JOIN game.ship_dirt d ON d.ship_item_id=i.item_id LEFT JOIN game.ship_kill_counts k ON k.ship_item_id=i.item_id WHERE i.item_id=$1 AND i.singleton AND i.quantity=1 AND i.jump_clone_id IS NULL", &[&ship])
        .await?.as_ref().map(StoredAppearance::from_row))
}
