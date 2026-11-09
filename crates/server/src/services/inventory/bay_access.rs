//! Shared ship-bay access policy for `invControllers.py` and ship configuration.
//! Settings come from owner-bound game rows, corporation membership from the database,
//! and fleet membership from the existing live fleet authority. Physical reachability,
//! private holds and durable fleet membership are not provided here.
use infinity_core::consts::{FLAG_FLEET_HANGAR, FLAG_SHIP_HANGAR};
use crate::db::executor::Executor;
use crate::dao::{characters::members, items::ship_configuration};
use crate::services::{CallError, db_err, fleets};

#[derive(Clone, Copy, Default)]
pub(in crate::services) struct Affiliation { pub fleet: bool, pub corporation: bool }
pub(in crate::services) async fn affiliation(ex: &(impl Executor + ?Sized), actor: i64, owner: i64) -> Result<Affiliation, CallError> {
    if actor <= 0 || owner <= 0 { return Ok(Affiliation::default()); }
    let fleet = fleets::same_fleet(actor, owner);
    let a = members::organization(ex, actor).await.map_err(db_err)?;
    let b = members::organization(ex, owner).await.map_err(db_err)?;
    let corporation = a.zip(b).is_some_and(|(a, b)| a.corporation_id > 0 && a.corporation_id == b.corporation_id);
    Ok(Affiliation { fleet, corporation })
}
#[derive(Clone, Copy, Default)]
pub(in crate::services) struct Permissions { pub owner: bool, pub fleet_hangar: bool, pub maintenance: bool }
impl Permissions {
    pub(in crate::services) fn allows(self, flag: i32) -> bool {
        self.owner || match flag { FLAG_FLEET_HANGAR => self.fleet_hangar, FLAG_SHIP_HANGAR => self.maintenance, _ => false }
    }
}
/// Apply the owner-bound sharing switches to already-resolved current affiliations.
pub(in crate::services) fn permissions_from(actor: i64, actual_owner: i64, settings: Option<&ship_configuration::Settings>, affiliation: Affiliation) -> Permissions {
    if actor > 0 && actor == actual_owner { return Permissions { owner: true, ..Permissions::default() }; }
    if actor <= 0 || actual_owner <= 0 { return Permissions::default(); }
    let Some(settings) = settings.filter(|s| s.owner_id == actual_owner) else { return Permissions::default() };
    Permissions { owner: false,
        fleet_hangar: settings.fleet_hangar_fleet && affiliation.fleet || settings.fleet_hangar_corp && affiliation.corporation,
        maintenance: settings.maintenance_fleet && affiliation.fleet || settings.maintenance_corp && affiliation.corporation }
}

/// Resolve all hull grants for one request; keys must be distinct and carry actual owners.
pub(in crate::services) async fn permissions_many(ex: &(impl Executor + ?Sized), actor: i64, hulls: &[(i64, i64)]) -> Result<std::collections::HashMap<(i64, i64), Permissions>, CallError> {
    let mut result = std::collections::HashMap::new();
    let mut pending = Vec::new();
    let mut ids = Vec::new();
    let ships: Vec<_> = hulls.iter().filter_map(|&(ship, owner)|
        (actor > 0 && owner > 0 && actor != owner).then_some(ship)).collect();
    let settings: std::collections::HashMap<_, _> = if ships.is_empty() {
        std::collections::HashMap::new()
    } else {
        ship_configuration::get_many(ex, &ships).await.map_err(db_err)?.into_iter()
            .map(|row| (row.ship_item_id, row.settings)).collect()
    };
    for &(ship, owner) in hulls {
        if actor <= 0 || owner <= 0 || actor == owner {
            result.insert((ship, owner), permissions_from(actor, owner, None, Affiliation::default()));
        } else if let Some(settings) = settings.get(&ship).copied().filter(|s| s.owner_id == owner) {
            pending.push((ship, owner, settings));
            ids.push(owner);
        } else {
            result.insert((ship, owner), permissions_from(actor, owner, None, Affiliation::default()));
        }
    }
    if !pending.is_empty() {
        ids.push(actor);
        ids.sort_unstable();
        ids.dedup();
        let organizations = members::organizations(ex, &ids).await.map_err(db_err)?;
        let actor_corp = organizations.get(&actor).copied().filter(|id| *id > 0);
        for (ship, owner, settings) in pending {
            let affiliation = Affiliation {
                fleet: fleets::same_fleet(actor, owner),
                corporation: actor_corp.is_some() && actor_corp == organizations.get(&owner).copied(),
            };
            result.insert((ship, owner), permissions_from(actor, owner, Some(&settings), affiliation));
        }
    }
    Ok(result)
}
#[cfg(test)] mod tests;
