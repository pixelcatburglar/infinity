//! Semantic integrity policy for verified retail ACHV tables read by
//! `generated.py:AchievementData`.
//! Positive increasing targets, reward ranges, references and contribution
//! declarations are server policy; producer readiness and reward fulfillment
//! are not checked here.
use std::collections::{HashMap, HashSet};
use anyhow::{bail, ensure, Context, Result};
use super::format as f;

fn required<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str> {
    let value = value.with_context(|| format!("missing {field}"))?;
    ensure!(!value.is_empty(), "empty {field}");
    Ok(value)
}

fn uuid(value: Option<&str>, field: &str) -> Result<String> {
    let value = required(value, field)?;
    let bytes = crate::uuid_text::parse(value, crate::uuid_text::Case::Either)
        .with_context(|| format!("invalid {field}"))?;
    ensure!(bytes.iter().any(|&byte| byte != 0), "zero {field}");
    Ok(crate::uuid_text::format(&bytes))
}

struct Validator<'a> {
    ids: HashSet<String>,
    methods: HashMap<&'a str, f::ContributionMethodDefinition<'a>>,
    achievement_ids: HashSet<String>,
    references: Vec<(String, String)>,
}

impl<'a> Validator<'a> {
    fn id(&mut self, id: Option<&str>) -> Result<String> {
        let id = uuid(id, "id")?;
        ensure!(self.ids.insert(id.clone()), "duplicate id or placement: {id}");
        Ok(id)
    }

    fn reward(&mut self, reward: f::Reward<'a>) -> Result<()> {
        self.id(reward.id())?;
        match reward.payload_type() {
            f::RewardPayload::IskReward => ensure!(reward.payload_as_isk_reward().context("missing ISK reward")?.quantity() > 0, "zero ISK reward"),
            f::RewardPayload::SpReward => ensure!(reward.payload_as_sp_reward().context("missing SP reward")?.quantity() > 0, "zero SP reward"),
            f::RewardPayload::ItemReward => { let item = reward.payload_as_item_reward().context("missing item reward")?; ensure!(item.type_id() > 0 && item.quantity() > 0, "invalid item reward"); },
            f::RewardPayload::TitleReward => { uuid(reward.payload_as_title_reward().context("missing title reward")?.title_id(), "title id")?; },
            _ => bail!("unknown reward payload"),
        }
        Ok(())
    }

    fn achievement(&mut self, achievement: f::Achievement<'a>) -> Result<()> {
        let id = self.id(achievement.id())?;
        self.achievement_ids.insert(id.clone());
        if let (Some(start), Some(end)) = (achievement.release_date_unix_ms(), achievement.expiration_date_unix_ms()) {
            ensure!(end > start, "invalid achievement dates");
        }
        let config = achievement.contribution().context("missing contribution")?;
        let method = self.methods.get(required(config.method_id(), "method id")?).context("unknown method")?;
        if let Some(params) = config.parameters() {
            for param in params {
                let key = required(param.key(), "parameter key")?;
                let definition = method.parameters().and_then(|v| v.into_iter().find(|p| p.key() == Some(key))).context("unknown matcher key")?;
                if let Some(matchers) = param.matchers() {
                    for matcher in matchers {
                        let ty = required(matcher.value_type(), "matcher value type")?;
                        ensure!(definition.accepted_value_types().is_some_and(|v| v.into_iter().any(|t| t == ty)), "unknown matcher type");
                        if ty == "achievement" {
                            if let Some(values) = matcher.values() {
                                for value in values { self.references.push((id.clone(), uuid(Some(value), "achievement reference")?)); }
                            }
                        }
                    }
                }
            }
        }
        let mut previous = 0;
        if let Some(milestones) = achievement.milestones() {
            for milestone in milestones {
                self.id(milestone.id())?;
                ensure!(milestone.target() > previous, "invalid achievement target ordering");
                previous = milestone.target();
                if let Some(rewards) = milestone.rewards() { for reward in rewards { self.reward(reward)?; } }
            }
        }
        Ok(())
    }

    fn category(&mut self, category: f::Category<'a>) -> Result<()> {
        self.id(category.id())?;
        let mut previous = 0;
        if let Some(milestones) = category.milestones() {
            for milestone in milestones {
                self.id(milestone.id())?;
                ensure!(milestone.target() > previous, "invalid category target ordering");
                previous = milestone.target();
                if let Some(rewards) = milestone.rewards() { for reward in rewards { self.reward(reward)?; } }
            }
        }
        if let Some(categories) = category.categories() { for child in categories { self.category(child)?; } }
        if let Some(achievements) = category.achievements() { for child in achievements { self.achievement(child)?; } }
        Ok(())
    }
}

pub(super) fn validate(root: f::AchievementData<'_>) -> Result<()> {
    let mut v = Validator { ids: HashSet::new(), methods: HashMap::new(), achievement_ids: HashSet::new(), references: Vec::new() };
    if let Some(methods) = root.contribution_methods() {
        for method in methods {
            let id = required(method.id(), "method id")?;
            ensure!(v.methods.insert(id, method).is_none(), "duplicate method id");
            let mut keys = HashSet::new();
            if let Some(params) = method.parameters() {
                for param in params { ensure!(keys.insert(required(param.key(), "parameter key")?), "duplicate parameter key"); required(param.type_(), "parameter type")?; }
            }
            match method.progress_type() {
                f::ProgressConfig::CumulativeMode => { method.progress_as_cumulative_mode().context("missing cumulative payload")?; },
                f::ProgressConfig::ChecklistMode => {
                    let mode = method.progress_as_checklist_mode().context("missing checklist payload")?;
                    let key = required(mode.target_parameter(), "checklist target")?;
                    ensure!(method.parameters().is_some_and(|params| params.into_iter().any(|p| p.key() == Some(key) && p.accepted_value_types().is_some_and(|types| types.into_iter().any(|t| t == "achievement")))), "invalid checklist target");
                },
                f::ProgressConfig::SetMode => {
                    let mode = method.progress_as_set_mode().context("missing set payload")?;
                    let ty = required(mode.target_value_type(), "set target")?;
                    ensure!(method.parameters().is_some_and(|params| params.into_iter().any(|p| p.accepted_value_types().is_some_and(|types| types.into_iter().any(|t| t == ty)))), "invalid set target");
                },
                _ => bail!("unknown progress payload"),
            }
        }
    }
    for category in [root.goals(), root.challenges(), root.time_limited()].into_iter().flatten() { v.category(category)?; }
    ensure!(v.references.iter().all(|(_, id)| v.achievement_ids.contains(id)), "orphan achievement reference");
    let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
    for (source, target) in &v.references { edges.entry(source).or_default().push(target); }
    fn visit<'a>(id: &'a str, edges: &HashMap<&'a str, Vec<&'a str>>, active: &mut HashSet<&'a str>, done: &mut HashSet<&'a str>) -> Result<()> {
        if done.contains(id) { return Ok(()); }
        ensure!(active.insert(id), "cyclic achievement references");
        if let Some(targets) = edges.get(id) { for target in targets { visit(target, edges, active, done)?; } }
        active.remove(id);
        done.insert(id);
        Ok(())
    }
    let mut active = HashSet::new();
    let mut done = HashSet::new();
    for source in edges.keys() { visit(source, &edges, &mut active, &mut done)?; }
    Ok(())
}
