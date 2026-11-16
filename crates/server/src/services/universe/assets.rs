//! Shared physical asset-root resolution for corporation assets and blueprints.
//! Reads batched game item/office ancestry and current corporation authority; static
//! locations and type metadata come from the loaded SDE. Structures and asset-safety
//! producers are not implemented, but persisted supported wraps are classified.
use std::collections::{BTreeMap, HashMap, HashSet};
use crate::client::Client;
use crate::dao::items::{assets as asset_dao, deliveries::{Ancestor, OfficeMetadata}, loading::Item};
use crate::services::{corps::permissions, db_err, items::enrich_item, notify_error, CallError};
use infinity_core::{consts::{CORP_HANGAR_DIVISIONS, FLAG_ASSET_SAFETY, FLAG_CAPSULEER_DELIVERIES, FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1, FLAG_HANGAR, LOCATION_JUNKYARD, ROLE_ACCOUNTANT, ROLE_DELIVERIES_VIEW, ROLE_DIRECTOR, ROLE_HANGAR_CAN_QUERY_1, ROLE_JUNIOR_ACCOUNTANT, TYPE_ASSET_SAFETY_WRAP}, messages};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::services) enum AssetRootKind { Office, Impounded, Deliveries, CapsuleerDeliveries, AssetWrap, Property }
impl AssetRootKind {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "offices" => Some(Self::Office), "impounded" => Some(Self::Impounded),
            "deliveries" => Some(Self::Deliveries), "capsuleerdeliveries" => Some(Self::CapsuleerDeliveries),
            "assetwraps" => Some(Self::AssetWrap), "property" => Some(Self::Property), _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::services) struct Root {
    pub kind: AssetRootKind,
    pub location: i64,
    pub container: i64,
    pub root: i64,
    pub flag: i32,
    pub station: Option<i64>,
    pub impounded: bool,
}
struct Node { item: Ancestor, office: Option<OfficeMetadata> }

pub(in crate::services) struct Snapshot<'a> {
    c: &'a Client,
    owner: i64,
    now: i64,
    authority: Option<permissions::Authority>,
    manager: bool,
    items: Vec<Item>,
    nodes: HashMap<i64, Node>,
    cache: HashMap<i64, Option<Root>>,
}

impl<'a> Snapshot<'a> {
    async fn authorize(c: &Client, owner: i64) -> Result<(Option<permissions::Authority>, bool), CallError> {
        let me = c.session.int("charid").unwrap_or(0);
        if me <= 0 || owner <= 0 {
            return Err(notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION));
        }
        let authority = if owner == me { None } else {
            let authority = permissions::load(&c.db, me).await?
                .filter(|authority| authority.live && authority.corporation == owner && crate::services::corps::is_player_corp(owner))
                .ok_or_else(|| notify_error(messages::INSUFFICIENT_ROLE_FOR_OPERATION))?;
            Some(authority)
        };
        let manager = authority.as_ref().is_some_and(|authority|
            permissions::allows(authority, owner, ROLE_ACCOUNTANT | ROLE_JUNIOR_ACCOUNTANT | ROLE_DIRECTOR)
                || permissions::active_ceo(authority, me));
        Ok((authority, manager))
    }

    fn from_facts(c: &'a Client, owner: i64, authority: Option<permissions::Authority>, manager: bool,
        items: Vec<Item>, ancestry: Vec<crate::dao::items::deliveries::LineageRow>) -> Self {
        let mut nodes = HashMap::new();
        for row in ancestry {
            nodes.entry(row.ancestor.item_id).or_insert(Node { item: row.ancestor, office: row.office });
        }
        Self { c, owner, now: crate::client::filetime_now(), authority, manager, items, nodes, cache: HashMap::new() }
    }

    pub(in crate::services) async fn load(c: &'a Client, owner: i64) -> Result<Self, CallError> {
        let (authority, manager) = Self::authorize(c, owner).await?;
        let facts = asset_dao::load(&c.db, owner).await.map_err(db_err)?;
        let items = facts.items.into_iter().filter_map(|item| enrich_item(&c.sde, item)).collect();
        Ok(Self::from_facts(c, owner, authority, manager, items, facts.ancestry))
    }

    pub(in crate::services) async fn blueprints(c: &'a Client, owner: i64, item: Option<i64>)
        -> Result<(Self, Vec<crate::dao::items::fitting::Blueprint>), CallError> {
        let (authority, manager) = Self::authorize(c, owner).await?;
        let rows = crate::dao::items::fitting::blueprints(&c.db, owner, item).await.map_err(db_err)?;
        let ids: Vec<_> = rows.iter().map(|row| row.item_id).collect();
        let ancestry = asset_dao::ancestry(&c.db, &ids).await.map_err(db_err)?;
        Ok((Self::from_facts(c, owner, authority, manager, Vec::new(), ancestry), rows))
    }

    pub(in crate::services) fn resolve(&mut self, id: i64) -> Option<Root> {
        let mut path = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor = id;
        let mut result = loop {
            if let Some(cached) = self.cache.get(&cursor) { break *cached; }
            if !seen.insert(cursor) { break None; }
            let Some(node) = self.nodes.get(&cursor) else { break None };
            let row = &node.item;
            if row.owner_id != self.owner || row.location_id <= 0 || row.location_id == LOCATION_JUNKYARD
                || (!row.singleton && row.quantity <= 0) || (row.singleton && row.quantity == 0) { break None; }
            if let Some(office) = &node.office {
                if row.type_id != infinity_core::consts::TYPE_OFFICE || !row.singleton
                    || office.corporation_id != self.owner || office.station_id != row.location_id
                    || self.c.sde.map.station(office.station_id).is_none() { break None; }
                let active = office.is_active(self.now);
                break Some(Root { kind: if active { AssetRootKind::Office } else { AssetRootKind::Impounded },
                    location: office.station_id, station: Some(office.station_id), container: cursor,
                    root: cursor, flag: 0, impounded: !active });
            }
            let at_station = self.c.sde.map.station(row.location_id).is_some();
            let at_system = self.c.sde.map.system(row.location_id).is_some();
            if at_station || at_system {
                let kind = if row.type_id == TYPE_ASSET_SAFETY_WRAP || row.flag_id == FLAG_ASSET_SAFETY { AssetRootKind::AssetWrap }
                    else if at_station && row.flag_id == FLAG_CORP_DELIVERIES { AssetRootKind::Deliveries }
                    else if at_station && row.flag_id == FLAG_CAPSULEER_DELIVERIES { AssetRootKind::CapsuleerDeliveries }
                    else if at_system || (self.owner == self.c.session.int("charid").unwrap_or(0) && at_station && row.flag_id == FLAG_HANGAR) { AssetRootKind::Property }
                    else { break None };
                break Some(Root { kind, location: row.location_id, station: at_station.then_some(row.location_id),
                    container: row.location_id, root: cursor, flag: row.flag_id, impounded: false });
            }
            let Some(parent) = self.nodes.get(&row.location_id) else { break None };
            if parent.item.owner_id != self.owner || (!parent.item.singleton && parent.office.is_none()) { break None; }
            if parent.office.is_some() {
                if !(FLAG_CORP_SAG1..FLAG_CORP_SAG1 + CORP_HANGAR_DIVISIONS).contains(&row.flag_id) { break None; }
            } else if parent.item.type_id != TYPE_ASSET_SAFETY_WRAP {
                let Some(ty) = self.c.sde.types.get(parent.item.type_id) else { break None };
                if ty.category_id != Some(infinity_core::consts::CATEGORY_SHIP)
                    && !ty.group_id.is_some_and(|group| group == infinity_core::consts::GROUP_WRECK
                        || infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&group)) { break None; }
            }
            path.push(cursor);
            cursor = row.location_id;
        };
        self.cache.insert(cursor, result);
        for child in path.into_iter().rev() {
            if let Some(root) = &mut result {
                let row = &self.nodes[&child].item;
                if root.container == row.location_id && root.root == row.location_id {
                    root.root = child;
                    root.flag = row.flag_id;
                }
                if row.type_id == TYPE_ASSET_SAFETY_WRAP || row.flag_id == FLAG_ASSET_SAFETY {
                    root.kind = AssetRootKind::AssetWrap;
                    root.root = child;
                    root.container = row.location_id;
                    root.flag = row.flag_id;
                }
            }
            self.cache.insert(child, result);
        }
        result
    }

    fn same_facts(&self, id: i64, facts: (i32, i64, i64, i32, i64, bool)) -> bool {
        self.nodes.get(&id).is_some_and(|node| {
            let row = &node.item;
            (row.type_id, row.owner_id, row.location_id, row.flag_id, row.quantity, row.singleton) == facts
        })
    }

    pub(in crate::services) fn matches_blueprint(&self, row: &crate::dao::items::fitting::Blueprint) -> bool {
        self.same_facts(row.item_id, (row.type_id, row.owner_id, row.location_id, row.flag_id, row.quantity, row.singleton))
    }

    pub(in crate::services) fn visible(&self, root: Root) -> bool {
        if self.authority.is_none() || self.manager { return true; }
        let Some(authority) = self.authority.as_ref() else { return false };
        let role = match root.kind {
            AssetRootKind::Office if root.root == root.container => (0..CORP_HANGAR_DIVISIONS).fold(0, |mask, i| mask | (ROLE_HANGAR_CAN_QUERY_1 << i)),
            AssetRootKind::Office if (FLAG_CORP_SAG1..FLAG_CORP_SAG1 + CORP_HANGAR_DIVISIONS).contains(&root.flag) => ROLE_HANGAR_CAN_QUERY_1 << (root.flag - FLAG_CORP_SAG1),
            AssetRootKind::Deliveries => ROLE_DELIVERIES_VIEW,
            _ => return false,
        };
        root.station.is_some_and(|station| permissions::allows_at(authority, self.owner, station, role))
    }

    fn for_each_entry(&mut self, mut visit: impl FnMut(&Item, Root)) {
        for index in 0..self.items.len() {
            let item = &self.items[index];
            let id = item.item_id;
            if !self.same_facts(id, (item.type_id, item.owner_id, item.location_id, item.flag_id, item.quantity, item.singleton)) { continue; }
            if let Some(root) = self.resolve(id) {
                if root.root == id && root.container != id && self.visible(root) {
                    visit(&self.items[index], root);
                }
            }
        }
    }

    pub(super) fn detail(&mut self, mode: AssetRootKind, location: i64) -> Vec<marshal::Value> {
        let mut rows = Vec::new();
        self.for_each_entry(|item, root| {
            if root.kind == mode && root.location == location { rows.push(item.row()); }
        });
        rows
    }

    pub(in crate::services) fn item_type(&self, id: i64) -> Option<i32> { self.nodes.get(&id).map(|node| node.item.type_id) }

    pub(super) fn locations(&mut self, mode: AssetRootKind, filter: Option<Filter>) -> Vec<marshal::Value> {
        let mut counts = BTreeMap::<i64, i64>::new();
        if mode == AssetRootKind::Office && filter.is_none() {
            for index in 0..self.items.len() {
                let item = &self.items[index];
                let id = item.item_id;
                if !self.nodes.get(&id).is_some_and(|node| node.office.is_some())
                    || !self.same_facts(id, (item.type_id, item.owner_id, item.location_id, item.flag_id, item.quantity, item.singleton)) { continue; }
                if let Some(root) = self.resolve(id) {
                    if root.kind == mode && self.visible(root) { counts.entry(root.location).or_default(); }
                }
            }
        }
        self.for_each_entry(|item, root| {
            if root.kind == mode && filter.is_none_or(|filter| filter.matches(item)) {
                *counts.entry(root.location).or_default() += 1;
            }
        });
        counts.into_iter().map(|(location, count)| {
            let system = self.c.sde.map.station(location).and_then(|station| station.solar_system_id)
                .or_else(|| self.c.sde.map.system(location).map(|_| location));
            crate::services::keyval([
                ("locationID", marshal::Value::Int(location)),
                ("solarsystemID", system.map(marshal::Value::Int).unwrap_or(marshal::Value::None)),
                ("itemCount", marshal::Value::Int(count)),
            ])
        }).collect()
    }
}

#[derive(Clone, Copy)]
pub(super) struct Filter { pub category: Option<i64>, pub group: Option<i64>, pub kind: Option<i64>, pub quantity: Option<i64> }
impl Filter {
    fn matches(self, item: &Item) -> bool {
        let shown = if item.singleton { 1 } else { crate::services::dogma::info::cap_client_quantity(item.quantity) };
        self.category.is_none_or(|id| id == item.category_id) && self.group.is_none_or(|id| id == item.group_id)
            && self.kind.is_none_or(|id| id == i64::from(item.type_id)) && self.quantity.is_none_or(|min| min <= shown)
    }
}

#[cfg(all(test, feature = "tests-content"))] mod tests;
