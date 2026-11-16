//! Test-only normalized projection for comparison with the independent client reader.
use super::*;
use serde_json::{json, Value};

macro_rules! array {
    ($value:expr, $map:expr) => { $value.map(|v| v.into_iter().map($map).collect::<Vec<_>>()).unwrap_or_default() };
}

mod fixture;
fn annotation(v: format::UiAnnotation<'_>) -> Value { json!({"Key":v.key(),"Value":v.value()}) }
fn reward(v: format::Reward<'_>) -> Value {
    let payload = match v.payload_type() {
        format::RewardPayload::IskReward => json!({"Quantity":v.payload_as_isk_reward().unwrap().quantity()}),
        format::RewardPayload::SpReward => json!({"Quantity":v.payload_as_sp_reward().unwrap().quantity()}),
        format::RewardPayload::ItemReward => { let x = v.payload_as_item_reward().unwrap(); json!({"TypeId":x.type_id(),"Quantity":x.quantity()}) },
        format::RewardPayload::TitleReward => json!({"TitleId":v.payload_as_title_reward().unwrap().title_id()}),
        _ => unreachable!(),
    };
    json!({"Id":v.id(),"InternalName":v.internal_name(),"PayloadType":v.payload_type().0,"Payload":payload})
}
fn achievement(v: format::Achievement<'_>) -> Value {
    let contribution = v.contribution().map(|c| json!({"MethodId":c.method_id(),"Parameters":array!(c.parameters(), |p| json!({"Key":p.key(),"Matchers":array!(p.matchers(), |m| json!({"ValueType":m.value_type(),"Values":array!(m.values(), |s| s)}))}))}));
    json!({"Id":v.id(),"Name":v.name(),"Description":v.description(),"FlavorText":v.flavor_text(),"IconId":v.icon_id(),"IssuerNpcCorp":v.issuer_npc_corp(),"Contribution":contribution,
        "Milestones":array!(v.milestones(), |m| json!({"Id":m.id(),"Target":m.target(),"CategoryPoints":m.category_points(),"Rewards":array!(m.rewards(), reward),"InternalName":m.internal_name()})),
        "ReleaseDateUnixMs":v.release_date_unix_ms(),"ExpirationDateUnixMs":v.expiration_date_unix_ms(),"UiAnnotations":array!(v.ui_annotations(), annotation),"Tags":array!(v.tags(), |s| s),"Archived":v.archived(),"InternalName":v.internal_name()})
}
fn category(v: format::Category<'_>) -> Value {
    json!({"Id":v.id(),"Name":v.name(),"Description":v.description(),"IconId":v.icon_id(),"InternalName":v.internal_name(),"UiAnnotations":array!(v.ui_annotations(), annotation),
        "Milestones":array!(v.milestones(), |m| json!({"Id":m.id(),"Target":m.target(),"Rewards":array!(m.rewards(), reward),"InternalName":m.internal_name()})),
        "Categories":array!(v.categories(), category),"Achievements":array!(v.achievements(), achievement)})
}
fn method(v: format::ContributionMethodDefinition<'_>) -> Value {
    let progress = match v.progress_type() {
        format::ProgressConfig::CumulativeMode => json!({}),
        format::ProgressConfig::ChecklistMode => json!({"TargetParameter":v.progress_as_checklist_mode().unwrap().target_parameter()}),
        format::ProgressConfig::SetMode => json!({"TargetValueType":v.progress_as_set_mode().unwrap().target_value_type()}),
        _ => unreachable!(),
    };
    json!({"Id":v.id(),"Name":v.name(),"Description":v.description(),"IconId":v.icon_id(),"ContentTags":array!(v.content_tags(), |s| s),"ProgressDescription":v.progress_description(),
        "Parameters":array!(v.parameters(), |p| json!({"Key":p.key(),"Type":p.type_(),"AcceptedValueTypes":array!(p.accepted_value_types(), |s| s),"Optional":p.optional(),"Name":p.name(),"Description":p.description(),"IconId":p.icon_id()})),
        "ProgressType":v.progress_type().0,"Progress":progress,"UiAnnotations":array!(v.ui_annotations(), annotation)})
}
#[test]
fn achievement_catalog_projection() {
    let catalog = Achievements::load(crate::retail::cdn("achievements").unwrap()).unwrap();
    let root = catalog.root;
    let projection = json!({"Goals":root.goals().map(category),"Challenges":root.challenges().map(category),"TimeLimited":root.time_limited().map(category),"ContributionMethods":array!(root.contribution_methods(), method)});
    println!("ACHIEVEMENT_CATALOG_JSON={projection}");
}
