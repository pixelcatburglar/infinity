//! Startup-only projections for the New Eden Store and seasonal login rewards.
//! Used by vgs/client/vgsClient.py and seasonalLoginCampaignService.py.
//! Sources are typed icons and native presentation, skins, market groups and campaigns.
//! Crates use their own shared catalogue; retail prices and other unconsumed native fields are not loaded here.
use crate::{db::{executor::Executor, Error}, sde::store::{Presentation, Icon, Skin, MarketGroup, Campaign}};

impl Presentation { fn from_row(r: &tokio_postgres::Row) -> Self { Self { type_id:r.get("type_id"), name_id:r.get("name_id") } } }
impl Icon { fn from_row(r: &tokio_postgres::Row) -> Self { Self { icon_id:r.get("icon_id"), path:r.get("path") } } }
impl Skin { fn from_row(r: &tokio_postgres::Row) -> Self { Self { type_id:r.get("type_id"), hull:r.get("hull") } } }
impl MarketGroup { fn from_row(r: &tokio_postgres::Row) -> Self { Self { id:r.get("id"), parent:r.get("parent"), name_id:r.get("name_id") } } }
impl Campaign { fn from_row(r: &tokio_postgres::Row) -> Self { Self { id:r.get("id"), alpha:crate::sde::store::parse_rewards(r.get("alpha")), omega:crate::sde::store::parse_rewards(r.get("omega")) } } }
pub(crate) async fn presentation(ex: &(impl Executor + ?Sized)) -> Result<Vec<Presentation>, Error> { Ok(ex.query("SELECT key::int AS type_id, (data->>'typeNameID')::bigint AS name_id FROM sde_native.types ORDER BY key::int", &[]).await?.iter().map(Presentation::from_row).collect()) }
pub(crate) async fn icons(ex: &(impl Executor + ?Sized)) -> Result<Vec<Icon>, Error> { Ok(ex.query("SELECT icon_id, icon_file AS path FROM sde.icons ORDER BY icon_id", &[]).await?.iter().map(Icon::from_row).collect()) }
pub(crate) async fn skins(ex: &(impl Executor + ?Sized)) -> Result<Vec<Skin>, Error> { Ok(ex.query("SELECT l.key::int AS type_id, (s.data->'types'->>0)::int AS hull FROM sde_native.skinlicenses l JOIN sde_native.skins s ON s.key = l.data->>'skinID' WHERE (s.data->>'visibleSerenity')::boolean AND (l.data->>'duration')::int = -1 ORDER BY l.key::int", &[]).await?.iter().map(Skin::from_row).collect()) }
pub(crate) async fn groups(ex: &(impl Executor + ?Sized)) -> Result<Vec<MarketGroup>, Error> { Ok(ex.query("SELECT key::int AS id, (data->>'parentGroupID')::int AS parent, (data->>'nameID')::bigint AS name_id FROM sde_native.marketgroups ORDER BY key::int", &[]).await?.iter().map(MarketGroup::from_row).collect()) }
pub(crate) async fn campaigns(ex: &(impl Executor + ?Sized)) -> Result<Vec<Campaign>, Error> { Ok(ex.query("SELECT key::int AS id, (SELECT string_agg((v->>'typeID') || ':' || COALESCE(v->>'quantity', '1'), ',' ORDER BY k::int) FROM jsonb_each(data->'alphaRewards') AS e(k,v)) AS alpha, (SELECT string_agg((v->>'typeID') || ':' || COALESCE(v->>'quantity', '1'), ',' ORDER BY k::int) FROM jsonb_each(data->'omegaRewards') AS e(k,v)) AS omega FROM sde_native.seasonalcampaignsloginrewards WHERE data ? 'alphaRewards' AND data ? 'omegaRewards' ORDER BY key::int DESC LIMIT 1", &[]).await?.iter().map(Campaign::from_row).collect()) }
