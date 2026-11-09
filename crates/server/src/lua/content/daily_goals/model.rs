//! Typed, immutable daily content and its SDE references.
//! The client contribution configuration is described by `dailygoals/client/utils.py`.
//! Activation validates SDE references; retained content keeps its original references.
use std::collections::HashSet;
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use crate::{isk::IskAmount, sde::Sde};

/// Bound on authored collections and policy plans, independent of gameplay targets.
const MAX_ENTRIES: usize = 64;

/// Reference validation is performed at activation, not against a later SDE on reopen.
#[derive(Clone, Copy)]
pub(in crate::lua::content) enum ValidationContext<'a> { Activation(&'a Sde), Retained }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Definitions {
    version: String,
    source: String,
    ordinary: Vec<Template>,
    slot_pools: Vec<Vec<String>>,
    bonus: Template,
    monthly_base: Vec<Template>,
    monthly_omega: Vec<Template>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredDefinitions {
    version: String,
    source: String,
    ordinary: Vec<AuthoredTemplate>,
    slot_pools: Vec<Vec<String>>,
    bonus: AuthoredTemplate,
    monthly_base: Vec<AuthoredTemplate>,
    monthly_omega: Vec<AuthoredTemplate>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoredTemplate {
    category: Option<Category>,
    omega_required: Option<bool>,
    key: String,
    target: u64,
    career: Career,
    method: Method,
    rewards: Vec<Reward>,
}

impl AuthoredTemplate {
    fn classify(self, category: Category, omega_required: bool) -> anyhow::Result<Template> {
        ensure!(self.category.is_none_or(|v| v == category) && self.omega_required.is_none_or(|v| v == omega_required), "contradictory template classification");
        Ok(Template { category, omega_required, key: self.key, target: self.target, career: self.career, method: self.method, rewards: self.rewards })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Career { None, Enforcer, Explorer, Industrialist, SoldierOfFortune }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Category { #[default] Ordinary, Bonus, Month }

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Template {
    #[serde(default)]
    pub(crate) category: Category,
    #[serde(default)]
    pub(crate) omega_required: bool,
    pub(crate) key: String,
    pub(crate) target: u64,
    pub(crate) career: Career,
    pub(crate) method: Method,
    pub(crate) rewards: Vec<Reward>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Reward {
    Isk { amount: IskAmount },
    Evermark { amount: u64 },
    SkillPoints { amount: u32 },
    Item { type_id: i32, amount: u32 },
    Plex { whole_units: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Matcher {
    Any,
    Id { id: u64 },
    Type { id: i32 },
    Character { id: u64 },
    Corporation { id: u64 },
    Alliance { id: u64 },
    Faction { id: u64 },
    Station { id: u64 },
    Structure { id: u64 },
    Ordinary,
    Bonus,
    Month,
    Goal { uuid: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Owner { Any, Character, Corporation }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Period { Day, Month }

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SystemEntry { pub(crate) system: Matcher }
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CombatEntry { pub(crate) system: Matcher, pub(crate) ship: Matcher, pub(crate) identity: Matcher }
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MiningEntry { pub(crate) system: Matcher, pub(crate) ore: Matcher }
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScanEntry { pub(crate) system: Matcher, pub(crate) signature: Matcher }
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManufacturingEntry { pub(crate) owner: Owner, pub(crate) facility: Matcher, pub(crate) item: Matcher }
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoyaltyEntry { pub(crate) corporation: Matcher }
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompletionEntry { pub(crate) goal: Matcher, pub(crate) category: Matcher, pub(crate) period: Period }
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WarfareEntry { pub(crate) faction: Matcher, pub(crate) system: Matcher, pub(crate) archetype: Matcher }

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Method {
    KillNpc { entries: Vec<SystemEntry> },
    DamageShip { entries: Vec<CombatEntry> },
    MineOre { entries: Vec<MiningEntry> },
    CaptureFwComplex { entries: Vec<WarfareEntry> },
    DefendFwComplex { entries: Vec<WarfareEntry> },
    RemoteRepairArmor { entries: Vec<CombatEntry> },
    RemoteRepairShields { entries: Vec<CombatEntry> },
    ScanSignature { entries: Vec<ScanEntry> },
    ManufactureItem { entries: Vec<ManufacturingEntry> },
    CompleteGoal { entries: Vec<CompletionEntry> },
    SalvageWreck { entries: Vec<SystemEntry> },
    EarnLoyaltyPoints { entries: Vec<LoyaltyEntry> },
    SpaceJump,
}

impl Method {
    pub(crate) fn metadata_key(&self) -> &'static str {
        match self {
            Self::KillNpc { .. } => "KillNPC", Self::DamageShip { .. } => "DamageShip", Self::MineOre { .. } => "MineOre",
            Self::CaptureFwComplex { .. } => "CaptureFWComplex", Self::DefendFwComplex { .. } => "DefendFWComplex",
            Self::RemoteRepairArmor { .. } => "RemoteRepairArmor", Self::RemoteRepairShields { .. } => "RemoteRepairShields",
            Self::ScanSignature { .. } => "ScanSignature", Self::ManufactureItem { .. } => "ManufactureItem",
            Self::CompleteGoal { .. } => "CompleteGoal", Self::SalvageWreck { .. } => "SalvageWreck",
            Self::EarnLoyaltyPoints { .. } => "EarnLoyaltyPoints", Self::SpaceJump => "SpaceJump",
        }
    }
}
impl Definitions {
    pub(crate) fn ordinary(&self) -> &[Template] { &self.ordinary }
    pub(crate) fn slot_pools(&self) -> &[Vec<String>] { &self.slot_pools }
    pub(crate) fn bonus(&self) -> &Template { &self.bonus }
    pub(crate) fn monthly_base(&self) -> &[Template] { &self.monthly_base }
    pub(crate) fn monthly_omega(&self) -> &[Template] { &self.monthly_omega }
    pub(crate) fn template(&self, key: &str) -> Option<&Template> {
        self.ordinary.iter().chain(std::iter::once(&self.bonus)).chain(&self.monthly_base).chain(&self.monthly_omega).find(|t| t.key == key)
    }
    pub(in crate::lua::content) fn parse(data: &[u8], version: &str, context: ValidationContext<'_>) -> anyhow::Result<Self> {
        let authored: AuthoredDefinitions = serde_json::from_slice(data).context("invalid daily content")?;
        let value = Self {
            version: authored.version, source: authored.source, slot_pools: authored.slot_pools,
            ordinary: authored.ordinary.into_iter().map(|t| t.classify(Category::Ordinary, false)).collect::<anyhow::Result<_>>()?,
            bonus: authored.bonus.classify(Category::Bonus, false)?,
            monthly_base: authored.monthly_base.into_iter().map(|t| t.classify(Category::Month, false)).collect::<anyhow::Result<_>>()?,
            monthly_omega: authored.monthly_omega.into_iter().map(|t| t.classify(Category::Month, true)).collect::<anyhow::Result<_>>()?,
        };
        ensure!(value.version == version && !value.source.is_empty(), "invalid daily identity");
        ensure!(!value.ordinary.is_empty() && value.ordinary.len() <= MAX_ENTRIES && value.slot_pools.len() == 3, "invalid ordinary pools");
        ensure!(value.monthly_base.len() == 4 && value.monthly_omega.len() == 4, "invalid monthly tracks");
        ensure!(value.monthly_base.iter().zip(&value.monthly_omega).all(|(a, b)| a.target == b.target) && value.monthly_base.windows(2).all(|w| w[0].target < w[1].target), "invalid monthly targets");
        ensure!(matches!(value.bonus.rewards.first(), Some(Reward::SkillPoints { .. })), "invalid bonus reward");
        let mut keys = HashSet::new();
        for t in value.ordinary.iter().chain(std::iter::once(&value.bonus)).chain(&value.monthly_base).chain(&value.monthly_omega) {
            ensure!(!t.key.is_empty() && t.key.len() <= MAX_ENTRIES && keys.insert(&t.key) && t.target > 0, "invalid daily template identity");
            ensure!(!t.rewards.is_empty() && t.rewards.len() <= MAX_ENTRIES, "invalid reward list");
            for reward in &t.rewards {
                match reward {
                    Reward::Isk { amount } => ensure!(!amount.is_negative() && !amount.is_zero() && amount.split_precision().is_some(), "invalid ISK reward"),
                    Reward::Evermark { amount } => ensure!(*amount > 0 && *amount <= i64::MAX as u64, "invalid LP reward"),
                    Reward::SkillPoints { amount } => ensure!(*amount > 0, "invalid skill reward"),
                    Reward::Item { type_id, amount } => ensure!(*amount > 0 && *amount <= i32::MAX as u32 && *type_id > 0 && match context { ValidationContext::Activation(sde) => sde.types.get(*type_id).is_some(), ValidationContext::Retained => true }, "invalid item reward"),
                    Reward::Plex { whole_units } => ensure!(*whole_units > 0 && whole_units.checked_mul(infinity_core::consts::PLEX_HUNDREDTHS as u64).is_some_and(|v| v <= i64::MAX as u64), "invalid PLEX reward"),
                }
            }
            if let ValidationContext::Activation(sde) = context {
                ensure!(match t.category {
                    Category::Ordinary => crate::services::daily_goals::progress::source::supports(t.method.metadata_key()),
                    Category::Bonus | Category::Month => matches!(t.method, Method::CompleteGoal { .. }),
                }, "daily source unavailable");
                let metadata = sde.contribution_methods.get(t.method.metadata_key()).context("missing contribution method")?;
                ensure!(positive_message_ref(metadata.title_id) && positive_message_ref(metadata.description_id) && sde.localization.english(metadata.title_id).is_some() && sde.localization.english(metadata.description_id).is_some() && sde.localization.chinese(metadata.title_id).is_some() && sde.localization.chinese(metadata.description_id).is_some(), "invalid or missing method localization");
            }
            validate_method(&t.method, context)?;
        }
        let ordinary: HashSet<_> = value.ordinary.iter().map(|t| t.key.as_str()).collect();
        let mut pooled = HashSet::new();
        for pool in &value.slot_pools {
            ensure!(!pool.is_empty() && pool.len() <= MAX_ENTRIES && pool.iter().all(|key| ordinary.contains(key.as_str()) && pooled.insert(key)), "invalid or overlapping slot pool");
        }
        ensure!(value.ordinary.len() >= 4, "not enough distinct daily templates");
        Ok(value)
    }
}

fn positive_u32(id: u64) -> bool { id > 0 && u32::try_from(id).is_ok() }

fn positive_message_ref(id: i64) -> bool { u64::try_from(id).ok().is_some_and(positive_u32) }

fn validate_method(method: &Method, context: ValidationContext<'_>) -> anyhow::Result<()> {
    let bounded = |len| len > 0 && len <= MAX_ENTRIES;
    let system = |m: &Matcher| match m { Matcher::Any => true, Matcher::Id { id } => i64::try_from(*id).ok().is_some_and(|id| match context { ValidationContext::Activation(sde) => sde.map.system(id).is_some(), ValidationContext::Retained => id > 0 }), _ => false };
    let type_match = |m: &Matcher| match m { Matcher::Any => true, Matcher::Type { id } => match context { ValidationContext::Activation(sde) => sde.types.get(*id).is_some(), ValidationContext::Retained => *id > 0 }, _ => false };
    let id_match = |m: &Matcher| matches!(m, Matcher::Any | Matcher::Id { id: 1.. });
    let id_match_u32 = |m: &Matcher| match m { Matcher::Any => true, Matcher::Id { id } => positive_u32(*id), _ => false };
    let identity = |m: &Matcher| match m {
        Matcher::Any => true,
        Matcher::Character { id } | Matcher::Corporation { id } => positive_u32(*id),
        Matcher::Alliance { id } => *id > 0 && i32::try_from(*id).is_ok(),
        Matcher::Faction { id } => *id > 0,
        _ => false,
    };
    let combat = |v: &Vec<CombatEntry>| bounded(v.len()) && v.iter().all(|e| system(&e.system) && type_match(&e.ship) && identity(&e.identity));
    let valid = match method {
        Method::KillNpc { entries } | Method::SalvageWreck { entries } => bounded(entries.len()) && entries.iter().all(|e| system(&e.system)),
        Method::DamageShip { entries } | Method::RemoteRepairArmor { entries } | Method::RemoteRepairShields { entries } => combat(entries),
        Method::MineOre { entries } => bounded(entries.len()) && entries.iter().all(|e| system(&e.system) && type_match(&e.ore)),
        Method::CaptureFwComplex { entries } | Method::DefendFwComplex { entries } => bounded(entries.len()) && entries.iter().all(|e| system(&e.system) && id_match(&e.faction) && id_match_u32(&e.archetype)),
        Method::ScanSignature { entries } => bounded(entries.len()) && entries.iter().all(|e| system(&e.system) && id_match_u32(&e.signature)),
        Method::ManufactureItem { entries } => bounded(entries.len()) && entries.iter().all(|e| (match &e.facility { Matcher::Any => true, Matcher::Station { id } => i64::try_from(*id).ok().is_some_and(|id| match context { ValidationContext::Activation(sde) => sde.map.station(id).is_some(), ValidationContext::Retained => id > 0 }), Matcher::Structure { id } => *id > 0 && *id <= i64::MAX as u64, _ => false }) && type_match(&e.item)),
        Method::EarnLoyaltyPoints { entries } => bounded(entries.len()) && entries.iter().all(|e| id_match_u32(&e.corporation)),
        Method::CompleteGoal { entries } => bounded(entries.len()) && entries.iter().all(|e| (matches!(&e.goal, Matcher::Any) || matches!(&e.goal, Matcher::Goal { uuid } if crate::uuid_text::parse(uuid, crate::uuid_text::Case::Either).is_some())) && matches!(&e.category, Matcher::Any | Matcher::Ordinary | Matcher::Bonus | Matcher::Month)),
        Method::SpaceJump => true,
    };
    ensure!(valid, "invalid contribution matcher");
    Ok(())
}

#[cfg(test)]
mod tests;
