//! Guarded physical Hub owner changes in `game.items`.
//! Positions remain authoritative on the item; authorization and transactions belong to the service.
//! This does not move items or walk inventory descendants.
use crate::db::{self, executor::Executor};

/// Preserve placement while changing a single deployed singleton's owner.
pub(crate) async fn transfer_owner(
    ex: &(impl Executor + ?Sized), item: i64, system: i64, type_id: i32, old_owner: i64, new_owner: i64,
) -> Result<bool, db::Error> {
    Ok(ex.execute(
        "UPDATE game.items SET owner_id=$5 WHERE item_id=$1 AND location_id=$2 AND type_id=$3 AND owner_id=$4 AND flag_id=0 AND quantity=1 AND singleton AND jump_clone_id IS NULL AND x IS NOT NULL AND y IS NOT NULL AND z IS NOT NULL",
        &[&item, &system, &type_id, &old_owner, &new_owner],
    ).await? == 1)
}

/// Place exactly the expected inventory singleton in space without creating an item.
pub(crate) async fn place(
    ex: &(impl Executor + ?Sized), item: &super::loading::Item,
    system: i64, owner: i64, position: crate::space::V3,
) -> Result<bool, db::Error> {
    if !position.iter().all(|coordinate| coordinate.is_finite()) || item.quantity != 1 {
        return Ok(false);
    }
    Ok(ex.execute(
        "UPDATE game.items SET owner_id=$8,location_id=$9,flag_id=0,singleton=TRUE,x=$10,y=$11,z=$12 WHERE item_id=$1 AND type_id=$2 AND owner_id=$3 AND location_id=$4 AND flag_id=$5 AND quantity=$6 AND singleton=$7 AND jump_clone_id IS NULL",
        &[&item.item_id, &item.type_id, &item.owner_id, &item.location_id, &item.flag_id,
          &item.quantity, &item.singleton, &owner, &system, &position[0], &position[1], &position[2]],
    ).await? == 1)
}
