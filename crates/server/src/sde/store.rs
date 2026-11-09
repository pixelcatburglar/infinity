//! Immutable presentation and seasonal data for the New Eden Store.
//! Used by vgs/client/vgsClient.py and seasonalLoginCampaignService.py.
//! Native presentation, groups and campaigns and typed icons are loaded once; campaign retro-claim eligibility is retained from SDE.
//! Crates use the shared catalogue. Expert prices are server-authored historical observations; other retail prices
//! and unsupported native fields remain outside this dataset.
pub(crate) mod expert_systems;
use crate::{dao::sde::store as dao, db::executor::Executor};
use super::{expert_systems::ExpertSystems, types::Types};

pub(crate) struct Presentation { pub type_id:i32, pub name_id:Option<i64> }
pub(crate) struct Icon { pub icon_id:i32, pub path:Option<String> }
pub(crate) struct MarketGroup { pub id:i32, pub parent:Option<i32>, pub name_id:Option<i64> }
pub(crate) struct Campaign { pub id:i32, pub alpha:Vec<(i32,i64)>, pub omega:Vec<(i32,i64)>, pub retro_claim:bool }
pub(crate) fn parse_rewards(s: Option<String>) -> Vec<(i32,i64)> {
    s.as_deref().unwrap_or_default().split(',').filter_map(|p| {
        let (t, q) = p.split_once(':')?;
        Some((t.parse().ok()?, q.parse().ok()?))
    }).collect()
}
#[cfg_attr(test, derive(Default))]
pub(crate) struct Store { presentation:Vec<Presentation>, icons:Vec<Icon>, groups:Vec<MarketGroup>, campaigns:Vec<Campaign>, expert_offers:Vec<expert_systems::ExpertOffer> }
impl Store {
    pub(crate) async fn load(ex: &(impl Executor + ?Sized), types: &Types, experts: &ExpertSystems) -> anyhow::Result<Self> {
        let expert_offers = expert_systems::parse(include_str!("../../content/expert_system_store.json"), |id| types.get(id).is_some(), |id| experts.get(id).is_some())?;
        let (presentation, icons, groups, campaigns) = tokio::try_join!(dao::presentation(ex), dao::icons(ex), dao::groups(ex), dao::campaigns(ex))?;
        Ok(Self { presentation, icons, groups, campaigns, expert_offers })
    }
    pub(crate) fn presentation(&self, id:i32) -> Option<&Presentation> { self.presentation.binary_search_by_key(&id, |r| r.type_id).ok().map(|i| &self.presentation[i]) }
    pub(crate) fn icon(&self, id:i32) -> Option<&str> { self.icons.binary_search_by_key(&id, |r| r.icon_id).ok().and_then(|i| self.icons[i].path.as_deref()) }
    pub(crate) fn group(&self, id:i32) -> Option<&MarketGroup> { self.groups.binary_search_by_key(&id, |r| r.id).ok().map(|i| &self.groups[i]) }
    pub(crate) fn expert_offers(&self) -> &[expert_systems::ExpertOffer] { &self.expert_offers }
    pub(crate) fn campaign(&self) -> Option<&Campaign> { self.campaigns.first() }
}
