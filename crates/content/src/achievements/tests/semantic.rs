use super::*;

fn verify(bytes: &[u8]) -> anyhow::Result<()> {
    let root = format::root_as_achievement_data(bytes)?;
    validate::validate(root)
}

#[test]
fn cumulative_method_accepts_absent_and_empty_parameters() {
    for explicit_empty in [false, true] {
        let mut b = flatbuffers::FlatBufferBuilder::new();
        let id = b.create_string("cumulative method");
        let parameters = if explicit_empty {
            Some(b.create_vector(&[] as &[flatbuffers::WIPOffset<format::MethodParamDefinition<'_>>]))
        } else { None };
        let progress = format::CumulativeMode::create(&mut b, &format::CumulativeModeArgs {});
        let method = format::ContributionMethodDefinition::create(&mut b, &format::ContributionMethodDefinitionArgs {
            id: Some(id), parameters, progress_type: format::ProgressConfig::CumulativeMode,
            progress: Some(progress.as_union_value()), ..Default::default()
        });
        let methods = b.create_vector(&[method]);
        let root = format::AchievementData::create(&mut b, &format::AchievementDataArgs {
            contribution_methods: Some(methods), ..Default::default()
        });
        format::finish_achievement_data_buffer(&mut b, root);
        let root = format::root_as_achievement_data(b.finished_data()).unwrap();
        assert_eq!(root.contribution_methods().unwrap().get(0).parameters().is_some(), explicit_empty);
        validate::validate(root).unwrap();
    }
}

#[test]
fn duplicate_tree_placement_is_rejected() {
    let mut b = flatbuffers::FlatBufferBuilder::new();
    let id = b.create_string("00000000-0000-0000-0000-000000000001");
    let category = format::Category::create(&mut b, &format::CategoryArgs { id: Some(id), ..Default::default() });
    let root = format::AchievementData::create(&mut b, &format::AchievementDataArgs { goals: Some(category), challenges: Some(category), ..Default::default() });
    format::finish_achievement_data_buffer(&mut b, root);
    assert!(verify(b.finished_data()).is_err());
}

#[test]
fn category_targets_must_be_positive_and_increasing() {
    for targets in [[0, 1], [1, 1], [2, 1], [1, 2]] {
        let mut b = flatbuffers::FlatBufferBuilder::new();
        let first_id = b.create_string("00000000-0000-0000-0000-000000000001");
        let second_id = b.create_string("00000000-0000-0000-0000-000000000002");
        let category_id = b.create_string("00000000-0000-0000-0000-000000000003");
        let first = format::CategoryMilestone::create(&mut b, &format::CategoryMilestoneArgs { id: Some(first_id), target: targets[0], ..Default::default() });
        let second = format::CategoryMilestone::create(&mut b, &format::CategoryMilestoneArgs { id: Some(second_id), target: targets[1], ..Default::default() });
        let milestones = b.create_vector(&[first, second]);
        let category = format::Category::create(&mut b, &format::CategoryArgs { id: Some(category_id), milestones: Some(milestones), ..Default::default() });
        let root = format::AchievementData::create(&mut b, &format::AchievementDataArgs { goals: Some(category), ..Default::default() });
        format::finish_achievement_data_buffer(&mut b, root);
        assert_eq!(verify(b.finished_data()).is_ok(), targets == [1, 2]);
    }
}

#[test]
fn reward_union_variants_and_ranges() {
    for kind in 1..=5 {
        for quantity in [0, 1] {
            let mut b = flatbuffers::FlatBufferBuilder::new();
            let id = b.create_string("00000000-0000-0000-0000-000000000001");
            let milestone_id = b.create_string("00000000-0000-0000-0000-000000000002");
            let category_id = b.create_string("00000000-0000-0000-0000-000000000003");
            let title = b.create_string("00000000-0000-0000-0000-000000000004");
            let payload = match kind {
                1 => format::IskReward::create(&mut b, &format::IskRewardArgs { quantity }).as_union_value(),
                2 => format::SpReward::create(&mut b, &format::SpRewardArgs { quantity }).as_union_value(),
                3 => format::ItemReward::create(&mut b, &format::ItemRewardArgs { type_id: 1, quantity }).as_union_value(),
                _ => format::TitleReward::create(&mut b, &format::TitleRewardArgs { title_id: Some(title) }).as_union_value(),
            };
            let reward = format::Reward::create(&mut b, &format::RewardArgs { id: Some(id), payload_type: format::RewardPayload(kind), payload: Some(payload), ..Default::default() });
            let rewards = b.create_vector(&[reward]);
            let milestone = format::CategoryMilestone::create(&mut b, &format::CategoryMilestoneArgs { id: Some(milestone_id), target: 1, rewards: Some(rewards), ..Default::default() });
            let milestones = b.create_vector(&[milestone]);
            let category = format::Category::create(&mut b, &format::CategoryArgs { id: Some(category_id), milestones: Some(milestones), ..Default::default() });
            let root = format::AchievementData::create(&mut b, &format::AchievementDataArgs { goals: Some(category), ..Default::default() });
            format::finish_achievement_data_buffer(&mut b, root);
            assert_eq!(verify(b.finished_data()).is_ok(), kind <= 4 && (kind == 4 || quantity > 0));
        }
    }
}

#[test]
fn progress_unions_and_declared_targets() {
    for mode in 1..=4 {
        for declared in [false, true] {
            let mut b = flatbuffers::FlatBufferBuilder::new();
            let method_id = b.create_string("opaque method");
            let key = b.create_string("opaque key");
            let param_type = b.create_string("opaque parameter type");
            let accepted = b.create_string(if declared { "achievement" } else { "other" });
            let accepted = b.create_vector(&[accepted]);
            let param = format::MethodParamDefinition::create(&mut b, &format::MethodParamDefinitionArgs {
                key: Some(key), type_: Some(param_type), accepted_value_types: Some(accepted), optional: false, ..Default::default()
            });
            let params = b.create_vector(&[param]);
            let value_type = b.create_string("achievement");
            let progress = match mode {
                1 => format::CumulativeMode::create(&mut b, &format::CumulativeModeArgs {}).as_union_value(),
                2 => format::ChecklistMode::create(&mut b, &format::ChecklistModeArgs { target_parameter: Some(key) }).as_union_value(),
                _ => format::SetMode::create(&mut b, &format::SetModeArgs { target_value_type: Some(value_type) }).as_union_value(),
            };
            let method = format::ContributionMethodDefinition::create(&mut b, &format::ContributionMethodDefinitionArgs {
                id: Some(method_id), parameters: Some(params), progress_type: format::ProgressConfig(mode), progress: Some(progress), ..Default::default()
            });
            let methods = b.create_vector(&[method]);
            let root = format::AchievementData::create(&mut b, &format::AchievementDataArgs { contribution_methods: Some(methods), ..Default::default() });
            format::finish_achievement_data_buffer(&mut b, root);
            assert_eq!(verify(b.finished_data()).is_ok(), mode == 1 || (mode <= 3 && declared));
        }
    }
}

#[test]
fn contribution_references_and_achievement_targets() {
    for case in 0..10 {
        let mut b = flatbuffers::FlatBufferBuilder::new();
        let category_id = b.create_string("00000000-0000-0000-0000-000000000001");
        let achievement_id = b.create_string("00000000-0000-0000-0000-000000000002");
        let milestone_id = b.create_string("00000000-0000-0000-0000-000000000003");
        let method_id = b.create_string("opaque method");
        let config_method = b.create_string(if case == 1 { "missing" } else { "opaque method" });
        let key = b.create_string("opaque key");
        let config_key = b.create_string(if case == 2 { "missing key" } else { "opaque key" });
        let ty = b.create_string("opaque type");
        let matcher_type = b.create_string(if case == 3 { "wrong" } else if matches!(case, 5 | 6) { "achievement" } else { "opaque value" });
        let accepted_type = b.create_string(if matches!(case, 5 | 6) { "achievement" } else { "opaque value" });
        let accepted = b.create_vector(&[accepted_type]);
        let param = format::MethodParamDefinition::create(&mut b, &format::MethodParamDefinitionArgs { key: Some(key), type_: Some(ty), accepted_value_types: Some(accepted), ..Default::default() });
        let method_params = b.create_vector(&[param]);
        let mode = format::CumulativeMode::create(&mut b, &format::CumulativeModeArgs {});
        let method = format::ContributionMethodDefinition::create(&mut b, &format::ContributionMethodDefinitionArgs { id: Some(method_id), parameters: Some(method_params), progress_type: format::ProgressConfig::CumulativeMode, progress: Some(mode.as_union_value()), ..Default::default() });
        let methods = b.create_vector(&[method]);
        let reference = b.create_string(if case == 5 { "00000000-0000-0000-0000-000000000009" } else { "00000000-0000-0000-0000-000000000002" });
        let values = b.create_vector(&[reference]);
        let matcher = format::MatcherValues::create(&mut b, &format::MatcherValuesArgs { value_type: Some(matcher_type), values: Some(values) });
        let matchers = b.create_vector(&[matcher]);
        let param = format::ContributionParam::create(&mut b, &format::ContributionParamArgs { key: Some(config_key), matchers: Some(matchers) });
        let params = b.create_vector(&[param]);
        let config = format::ContributionConfig::create(&mut b, &format::ContributionConfigArgs { method_id: Some(config_method), parameters: Some(params) });
        let milestone = format::AchievementMilestone::create(&mut b, &format::AchievementMilestoneArgs { id: Some(milestone_id), target: if case == 4 { 0 } else { 1 }, category_points: 0, ..Default::default() });
        let milestones = b.create_vector(&[milestone]);
        let achievement = format::Achievement::create(&mut b, &format::AchievementArgs { id: Some(achievement_id), contribution: Some(config), milestones: Some(milestones), release_date_unix_ms: if case >= 7 { Some(10) } else { None }, expiration_date_unix_ms: match case { 7 => Some(10), 8 => Some(9), _ => None }, ..Default::default() });
        let achievements = b.create_vector(&[achievement]);
        let category = format::Category::create(&mut b, &format::CategoryArgs { id: Some(category_id), achievements: Some(achievements), ..Default::default() });
        let root = format::AchievementData::create(&mut b, &format::AchievementDataArgs { goals: Some(category), contribution_methods: Some(methods), ..Default::default() });
        format::finish_achievement_data_buffer(&mut b, root);
        assert_eq!(verify(b.finished_data()).is_ok(), matches!(case, 0 | 9));
    }
}

#[test]
fn nested_categories_preserve_original_order() {
    let mut b = flatbuffers::FlatBufferBuilder::new();
    let a = b.create_string("00000000-0000-0000-0000-000000000001");
    let c = b.create_string("00000000-0000-0000-0000-000000000002");
    let parent = b.create_string("00000000-0000-0000-0000-000000000003");
    let a = format::Category::create(&mut b, &format::CategoryArgs { id: Some(a), ..Default::default() });
    let c = format::Category::create(&mut b, &format::CategoryArgs { id: Some(c), ..Default::default() });
    let children = b.create_vector(&[c, a]);
    let parent = format::Category::create(&mut b, &format::CategoryArgs { id: Some(parent), categories: Some(children), ..Default::default() });
    let root = format::AchievementData::create(&mut b, &format::AchievementDataArgs { goals: Some(parent), ..Default::default() });
    format::finish_achievement_data_buffer(&mut b, root);
    let root = format::root_as_achievement_data(b.finished_data()).unwrap();
    validate::validate(root).unwrap();
    let children = root.goals().unwrap().categories().unwrap();
    assert_eq!(children.get(0).id(), Some("00000000-0000-0000-0000-000000000002"));
    assert_eq!(children.get(1).id(), Some("00000000-0000-0000-0000-000000000001"));
}

#[test]
fn missing_union_payloads_are_rejected() {
    for progress in [format::ProgressConfig::NONE, format::ProgressConfig::CumulativeMode, format::ProgressConfig(99)] {
        let mut b = flatbuffers::FlatBufferBuilder::new();
        let id = b.create_string("method");
        let method = format::ContributionMethodDefinition::create(&mut b, &format::ContributionMethodDefinitionArgs {
            id: Some(id), progress_type: progress, progress: None, ..Default::default()
        });
        let methods = b.create_vector(&[method]);
        let root = format::AchievementData::create(&mut b, &format::AchievementDataArgs { contribution_methods: Some(methods), ..Default::default() });
        format::finish_achievement_data_buffer(&mut b, root);
        assert!(verify(b.finished_data()).is_err());
    }
}

#[test]
fn missing_reward_payload_is_rejected() {
    let mut b = flatbuffers::FlatBufferBuilder::new();
    let reward_id = b.create_string("00000000-0000-0000-0000-000000000001");
    let milestone_id = b.create_string("00000000-0000-0000-0000-000000000002");
    let category_id = b.create_string("00000000-0000-0000-0000-000000000003");
    let reward = format::Reward::create(&mut b, &format::RewardArgs { id: Some(reward_id), payload_type: format::RewardPayload::SpReward, payload: None, ..Default::default() });
    let rewards = b.create_vector(&[reward]);
    let milestone = format::CategoryMilestone::create(&mut b, &format::CategoryMilestoneArgs { id: Some(milestone_id), target: 1, rewards: Some(rewards), ..Default::default() });
    let milestones = b.create_vector(&[milestone]);
    let category = format::Category::create(&mut b, &format::CategoryArgs { id: Some(category_id), milestones: Some(milestones), ..Default::default() });
    let root = format::AchievementData::create(&mut b, &format::AchievementDataArgs { goals: Some(category), ..Default::default() });
    format::finish_achievement_data_buffer(&mut b, root);
    assert!(verify(b.finished_data()).is_err());
}
