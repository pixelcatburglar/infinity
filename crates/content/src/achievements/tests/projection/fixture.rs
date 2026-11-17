//! Server-authored projection fixture; no catalogue data is copied into it.
use super::*;

#[test]
fn synthetic_all_fields_and_order() {
    let mut b = flatbuffers::FlatBufferBuilder::new();
    macro_rules! s { ($v:expr) => { Some(b.create_string($v)) }; }
    let akey = s!("annotation-z"); let aval = s!("value-z");
    let az = format::UiAnnotation::create(&mut b, &format::UiAnnotationArgs { key: akey, value: aval });
    let akey = s!("annotation-a"); let aval = s!("value-a");
    let aa = format::UiAnnotation::create(&mut b, &format::UiAnnotationArgs { key: akey, value: aval });
    let annotations = b.create_vector(&[az, aa]);
    let t1 = b.create_string("tag-z"); let t2 = b.create_string("tag-a");
    let tags = b.create_vector(&[t1, t2]);
    let v1 = b.create_string("value-z"); let v2 = b.create_string("value-a");
    let values = b.create_vector(&[v1, v2]);
    let vt = s!("value-type-z");
    let mz = format::MatcherValues::create(&mut b, &format::MatcherValuesArgs { value_type: vt, values: Some(values) });
    let vt = s!("value-type-a");
    let ma = format::MatcherValues::create(&mut b, &format::MatcherValuesArgs { value_type: vt, values: Some(values) });
    let matchers = b.create_vector(&[mz, ma]);
    let key = s!("key-z");
    let pz = format::ContributionParam::create(&mut b, &format::ContributionParamArgs { key, matchers: Some(matchers) });
    let key = s!("key-a");
    let pa = format::ContributionParam::create(&mut b, &format::ContributionParamArgs { key, matchers: Some(matchers) });
    let params = b.create_vector(&[pz, pa]);
    let method_id = s!("method-cumulative");
    let contribution = format::ContributionConfig::create(&mut b, &format::ContributionConfigArgs { method_id, parameters: Some(params) });

    let isk = format::IskReward::create(&mut b, &format::IskRewardArgs { quantity: 4294967301 }).as_union_value();
    let sp = format::SpReward::create(&mut b, &format::SpRewardArgs { quantity: 17 }).as_union_value();
    let item = format::ItemReward::create(&mut b, &format::ItemRewardArgs { type_id: 23, quantity: 29 }).as_union_value();
    let title_id = s!("title-opaque");
    let title = format::TitleReward::create(&mut b, &format::TitleRewardArgs { title_id }).as_union_value();
    let mut rewards = Vec::new();
    for (id, name, payload_type, payload) in [
        ("00000000-0000-0000-0000-000000000011", "reward-isk", format::RewardPayload::IskReward, isk),
        ("00000000-0000-0000-0000-000000000012", "reward-sp", format::RewardPayload::SpReward, sp),
        ("00000000-0000-0000-0000-000000000013", "reward-item", format::RewardPayload::ItemReward, item),
        ("00000000-0000-0000-0000-000000000014", "reward-title", format::RewardPayload::TitleReward, title),
    ] {
        let id = s!(id); let internal_name = s!(name);
        rewards.push(format::Reward::create(&mut b, &format::RewardArgs { id, internal_name, payload_type, payload: Some(payload) }));
    }
    let rewards = b.create_vector(&rewards);
    let id = s!("00000000-0000-0000-0000-000000000021"); let internal_name = s!("achievement-milestone");
    let am = format::AchievementMilestone::create(&mut b, &format::AchievementMilestoneArgs { id, target: 4294967302, category_points: 31, rewards: Some(rewards), internal_name });
    let achievement_milestones = b.create_vector(&[am]);
    let id = s!("00000000-0000-0000-0000-000000000022"); let internal_name = s!("category-milestone");
    let cm = format::CategoryMilestone::create(&mut b, &format::CategoryMilestoneArgs { id, target: 37, rewards: None, internal_name });
    let category_milestones = b.create_vector(&[cm]);
    let id = s!("00000000-0000-0000-0000-000000000031"); let icon_id = s!("achievement-icon"); let internal_name = s!("achievement-z");
    let az = format::Achievement::create(&mut b, &format::AchievementArgs { id, name: 41, description: 42, flavor_text: Some(0), icon_id, issuer_npc_corp: Some(0), contribution: Some(contribution), milestones: Some(achievement_milestones), release_date_unix_ms: Some(0), expiration_date_unix_ms: Some(99), ui_annotations: Some(annotations), tags: Some(tags), archived: true, internal_name });
    let id = s!("00000000-0000-0000-0000-000000000032"); let internal_name = s!("achievement-a");
    let aa = format::Achievement::create(&mut b, &format::AchievementArgs { id, internal_name, ..Default::default() });
    let achievements = b.create_vector(&[az, aa]);
    let id = s!("00000000-0000-0000-0000-000000000041"); let internal_name = s!("child-z");
    let cz = format::Category::create(&mut b, &format::CategoryArgs { id, internal_name, ..Default::default() });
    let id = s!("00000000-0000-0000-0000-000000000042"); let internal_name = s!("child-a");
    let ca = format::Category::create(&mut b, &format::CategoryArgs { id, internal_name, ..Default::default() });
    let children = b.create_vector(&[cz, ca]);
    let id = s!("00000000-0000-0000-0000-000000000043"); let icon_id = s!("category-icon"); let internal_name = s!("goals");
    let goals = format::Category::create(&mut b, &format::CategoryArgs { id, name: 51, description: 52, icon_id, milestones: Some(category_milestones), categories: Some(children), achievements: Some(achievements), internal_name, ui_annotations: Some(annotations) });
    let id = s!("00000000-0000-0000-0000-000000000044"); let internal_name = s!("challenges");
    let challenges = format::Category::create(&mut b, &format::CategoryArgs { id, internal_name, ..Default::default() });
    let id = s!("00000000-0000-0000-0000-000000000045"); let internal_name = s!("time-limited");
    let time_limited = format::Category::create(&mut b, &format::CategoryArgs { id, internal_name, ..Default::default() });

    let key = s!("key-z"); let type_ = s!("type-z"); let icon_id = s!("parameter-icon-z");
    let dpz = format::MethodParamDefinition::create(&mut b, &format::MethodParamDefinitionArgs { key, type_, accepted_value_types: Some(tags), optional: true, name: 61, description: 62, icon_id });
    let key = s!("key-a"); let type_ = s!("type-a"); let icon_id = s!("parameter-icon-a");
    let dpa = format::MethodParamDefinition::create(&mut b, &format::MethodParamDefinitionArgs { key, type_, accepted_value_types: Some(tags), optional: false, name: 63, description: 64, icon_id });
    let definitions = b.create_vector(&[dpz, dpa]);
    let cumulative = format::CumulativeMode::create(&mut b, &format::CumulativeModeArgs {}).as_union_value();
    let target_parameter = s!("key-z");
    let checklist = format::ChecklistMode::create(&mut b, &format::ChecklistModeArgs { target_parameter }).as_union_value();
    let target_value_type = s!("tag-z");
    let set = format::SetMode::create(&mut b, &format::SetModeArgs { target_value_type }).as_union_value();
    let mut methods = Vec::new();
    for (id, kind, progress) in [("method-set", format::ProgressConfig::SetMode, set), ("method-cumulative", format::ProgressConfig::CumulativeMode, cumulative), ("method-checklist", format::ProgressConfig::ChecklistMode, checklist)] {
        let id = s!(id); let icon_id = s!("method-icon");
        methods.push(format::ContributionMethodDefinition::create(&mut b, &format::ContributionMethodDefinitionArgs { id, name: 71, description: 72, icon_id, content_tags: Some(tags), progress_description: 73, parameters: Some(definitions), progress_type: kind, progress: Some(progress), ui_annotations: Some(annotations) }));
    }
    let methods = b.create_vector(&methods);
    let root = format::AchievementData::create(&mut b, &format::AchievementDataArgs { goals: Some(goals), challenges: Some(challenges), time_limited: Some(time_limited), contribution_methods: Some(methods) });
    format::finish_achievement_data_buffer(&mut b, root);
    let root = format::root_as_achievement_data(b.finished_data()).unwrap();
    let actual = json!({"Goals":root.goals().map(category),"Challenges":root.challenges().map(category),"TimeLimited":root.time_limited().map(category),"ContributionMethods":array!(root.contribution_methods(), method)});
    let expected = json!({
      "Goals": {"Id":"00000000-0000-0000-0000-000000000043","Name":51,"Description":52,"IconId":"category-icon","InternalName":"goals",
        "UiAnnotations":[{"Key":"annotation-z","Value":"value-z"},{"Key":"annotation-a","Value":"value-a"}],
        "Milestones":[{"Id":"00000000-0000-0000-0000-000000000022","Target":37,"InternalName":"category-milestone","Rewards":[]}],
        "Categories":[
          {"Id":"00000000-0000-0000-0000-000000000041","Name":0,"Description":0,"IconId":null,"InternalName":"child-z","UiAnnotations":[],"Milestones":[],"Categories":[],"Achievements":[]},
          {"Id":"00000000-0000-0000-0000-000000000042","Name":0,"Description":0,"IconId":null,"InternalName":"child-a","UiAnnotations":[],"Milestones":[],"Categories":[],"Achievements":[]}],
        "Achievements":[{"Id":"00000000-0000-0000-0000-000000000031","Name":41,"Description":42,"FlavorText":0,"IconId":"achievement-icon","IssuerNpcCorp":0,
          "Contribution":{"MethodId":"method-cumulative","Parameters":[
            {"Key":"key-z","Matchers":[{"ValueType":"value-type-z","Values":["value-z","value-a"]},{"ValueType":"value-type-a","Values":["value-z","value-a"]}]},
            {"Key":"key-a","Matchers":[{"ValueType":"value-type-z","Values":["value-z","value-a"]},{"ValueType":"value-type-a","Values":["value-z","value-a"]}]}]},
          "Milestones":[{"Id":"00000000-0000-0000-0000-000000000021","Target":4294967302u64,"CategoryPoints":31,"InternalName":"achievement-milestone","Rewards":[
            {"Id":"00000000-0000-0000-0000-000000000011","InternalName":"reward-isk","PayloadType":1,"Payload":{"Quantity":4294967301u64}},
            {"Id":"00000000-0000-0000-0000-000000000012","InternalName":"reward-sp","PayloadType":2,"Payload":{"Quantity":17}},
            {"Id":"00000000-0000-0000-0000-000000000013","InternalName":"reward-item","PayloadType":3,"Payload":{"TypeId":23,"Quantity":29}},
            {"Id":"00000000-0000-0000-0000-000000000014","InternalName":"reward-title","PayloadType":4,"Payload":{"TitleId":"title-opaque"}}]}],
          "ReleaseDateUnixMs":0,"ExpirationDateUnixMs":99,"UiAnnotations":[{"Key":"annotation-z","Value":"value-z"},{"Key":"annotation-a","Value":"value-a"}],"Tags":["tag-z","tag-a"],"Archived":true,"InternalName":"achievement-z"},
          {"Id":"00000000-0000-0000-0000-000000000032","Name":0,"Description":0,"FlavorText":null,"IconId":null,"IssuerNpcCorp":null,"Contribution":null,"Milestones":[],"ReleaseDateUnixMs":null,"ExpirationDateUnixMs":null,"UiAnnotations":[],"Tags":[],"Archived":false,"InternalName":"achievement-a"}]},
      "Challenges":{"Id":"00000000-0000-0000-0000-000000000044","Name":0,"Description":0,"IconId":null,"InternalName":"challenges","UiAnnotations":[],"Milestones":[],"Categories":[],"Achievements":[]},
      "TimeLimited":{"Id":"00000000-0000-0000-0000-000000000045","Name":0,"Description":0,"IconId":null,"InternalName":"time-limited","UiAnnotations":[],"Milestones":[],"Categories":[],"Achievements":[]},
      "ContributionMethods":[
        {"Id":"method-set","Name":71,"Description":72,"IconId":"method-icon","ContentTags":["tag-z","tag-a"],"ProgressDescription":73,"Parameters":[{"Key":"key-z","Type":"type-z","AcceptedValueTypes":["tag-z","tag-a"],"Optional":true,"Name":61,"Description":62,"IconId":"parameter-icon-z"},{"Key":"key-a","Type":"type-a","AcceptedValueTypes":["tag-z","tag-a"],"Optional":false,"Name":63,"Description":64,"IconId":"parameter-icon-a"}],"ProgressType":3,"Progress":{"TargetValueType":"tag-z"},"UiAnnotations":[{"Key":"annotation-z","Value":"value-z"},{"Key":"annotation-a","Value":"value-a"}]},
        {"Id":"method-cumulative","Name":71,"Description":72,"IconId":"method-icon","ContentTags":["tag-z","tag-a"],"ProgressDescription":73,"Parameters":[{"Key":"key-z","Type":"type-z","AcceptedValueTypes":["tag-z","tag-a"],"Optional":true,"Name":61,"Description":62,"IconId":"parameter-icon-z"},{"Key":"key-a","Type":"type-a","AcceptedValueTypes":["tag-z","tag-a"],"Optional":false,"Name":63,"Description":64,"IconId":"parameter-icon-a"}],"ProgressType":1,"Progress":{},"UiAnnotations":[{"Key":"annotation-z","Value":"value-z"},{"Key":"annotation-a","Value":"value-a"}]},
        {"Id":"method-checklist","Name":71,"Description":72,"IconId":"method-icon","ContentTags":["tag-z","tag-a"],"ProgressDescription":73,"Parameters":[{"Key":"key-z","Type":"type-z","AcceptedValueTypes":["tag-z","tag-a"],"Optional":true,"Name":61,"Description":62,"IconId":"parameter-icon-z"},{"Key":"key-a","Type":"type-a","AcceptedValueTypes":["tag-z","tag-a"],"Optional":false,"Name":63,"Description":64,"IconId":"parameter-icon-a"}],"ProgressType":2,"Progress":{"TargetParameter":"key-z"},"UiAnnotations":[{"Key":"annotation-z","Value":"value-z"},{"Key":"annotation-a","Value":"value-a"}]}
      ]
    });
    assert_eq!(actual, expected);
}
