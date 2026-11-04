//! Trade restriction, direct skill purchase and compressor type lists from `sde_native.typelist`.
//! Membership follows `evetypes/__init__.py:GetTypeIDsFromTypeList`; other lists are not loaded.
//! Startup fails if any requested trade, purchase or compressor list is missing.
use std::collections::{HashMap, HashSet};
use crate::{consts::{dogma::ATTR_COMPRESSIBLE_ITEMS_TYPE_LIST, TYPE_LIST_SKILLS_AVAILABLE_FOR_PURCHASE}, dao::sde::typelists as dao, db::Db};
use super::types::Types;

/// inventoryrestrictions/common/checks.py: non-tradable type list.
const NON_TRADABLE_LIST: i64 = 36;
/// inventoryrestrictions/common/checks.py: soulbound type list.
const SOULBOUND_LIST: i64 = 142;
/// evetypes/const.py: type list used by inventory trash restrictions.
const UNTRASHABLE_LIST: i64 = 298;
/// evetypes/const.py: type list used by fitting strip restrictions.
const UNSTRIPPABLE_LIST: i64 = 338;
/// Startup lists required by inventory restrictions and direct skill purchase availability.
const REQUIRED_LISTS: [i64; 5] = [NON_TRADABLE_LIST, SOULBOUND_LIST, TYPE_LIST_SKILLS_AVAILABLE_FOR_PURCHASE, UNTRASHABLE_LIST, UNSTRIPPABLE_LIST];

pub(crate) struct TypeLists(HashMap<i64, dao::TypeList>);

impl TypeLists {
    pub(crate) async fn load(db: &Db, types: &Types) -> anyhow::Result<Self> {
        let mut ids = HashSet::from(REQUIRED_LISTS);
        for value in types.iter().filter_map(|ty| ty.attribute(ATTR_COMPRESSIBLE_ITEMS_TYPE_LIST)) {
            anyhow::ensure!(value.is_finite() && value > 0.0 && value.fract() == 0.0 && value < i64::MAX as f64, "invalid compressor type-list attribute: {value}");
            ids.insert(value as i64);
        }
        let keys: Vec<String> = ids.iter().map(ToString::to_string).collect();
        Self::from_rows(&ids, dao::load(db, &keys).await?)
    }

    fn from_rows(ids: &HashSet<i64>, rows: Vec<dao::TypeList>) -> anyhow::Result<Self> {
        let lists: HashMap<_, _> = rows.into_iter().map(|list| (list.id, list)).collect();
        let mut missing: Vec<_> = ids.iter().filter(|id| !lists.contains_key(id)).copied().collect();
        missing.sort_unstable();
        anyhow::ensure!(missing.is_empty(), "missing requested type-list IDs: {missing:?}");
        Ok(Self(lists))
    }

    pub(crate) fn contains(&self, list: i64, type_id: i32, group: Option<i32>, category: Option<i32>) -> bool {
        let Some(list) = self.0.get(&list) else { return false };
        let in_category = category.is_some_and(|id| list.included_categories.contains(&id) && !list.excluded_categories.contains(&id));
        let in_group = group.is_some_and(|id| list.included_groups.contains(&id));
        let excluded_group = group.is_some_and(|id| list.excluded_groups.contains(&id));
        ((in_category || in_group) && !excluded_group || list.included_types.contains(&type_id)) && !list.excluded_types.contains(&type_id)
    }

    pub(crate) fn is_tradable(&self, type_id: i32, group_id: Option<i32>, category_id: Option<i32>) -> bool {
        ![NON_TRADABLE_LIST, SOULBOUND_LIST].into_iter().any(|list| self.contains(list, type_id, group_id, category_id))
    }
    pub(crate) fn is_untrashable(&self, type_id: i32, group_id: i32, category_id: i32) -> bool {
        self.contains(UNTRASHABLE_LIST, type_id, Some(group_id), Some(category_id))
    }
}

#[cfg(test)] mod tests;
