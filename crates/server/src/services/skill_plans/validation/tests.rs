use super::*;

mod audit;

#[test]
fn text_units_match_narrow_python_unicode() {
    for (text, expected) in [("", 0), ("a", 1), ("中", 1), ("🚀", 2), ("a\u{301}", 2)] {
        assert_eq!(text_units(text), expected);
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn sql_and_rust_text_units_agree_at_boundaries() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    for text in [
        String::new(), "a".into(), "中".into(), "🚀".into(), "a\u{301}".into(),
        "中".repeat(50), "中".repeat(51), "🚀".repeat(25),
        format!("{}中", "🚀".repeat(25)), format!("{}🚀", "中".repeat(49)),
        "中".repeat(1000), "中".repeat(1001), "🚀".repeat(500),
        format!("{}中", "🚀".repeat(500)), format!("{}🚀", "中".repeat(999)),
        "a\u{301}".repeat(500), format!("{}a", "a\u{301}".repeat(500)),
    ] {
        let row = db.query_one("SELECT game.skill_plan_text_units($1) AS units", &[&text]).await.unwrap();
        assert_eq!(row.get::<_, i32>("units") as usize, text_units(&text), "{text:?}");
    }
}

#[test]
fn diamond_prerequisites_merge_to_highest_level_and_keep_zero() {
    let graph = BTreeMap::from([(1, vec![(2, 1), (3, 1), (5, 0)]), (2, vec![(4, 2)]), (3, vec![(4, 4)])]);
    let required = required_with(1, None, |id| graph.get(&id).into_iter().flatten().copied());
    assert_eq!(required, BTreeMap::from([(2, 1), (3, 1), (4, 4), (5, 0)]));
}
#[test]
fn synthetic_published_hidden_and_obsolete_skills_are_rejected() {
    for (group, obsolete) in [(Some(GROUP_CLOUD_COMPUTING), 0.0), (None, 1.0)] {
        assert!(!valid_skill_metadata(Some(CATEGORY_SKILL), group, Some(true), obsolete, false));
        assert!(!sequence_with(&[1], &[1],
            |_| valid_skill_metadata(Some(CATEGORY_SKILL), group, Some(true), obsolete, false),
            |_| std::iter::empty()));
    }
    assert!(sequence_with(&[1], &[1],
        |_| valid_skill_metadata(Some(CATEGORY_SKILL), None, Some(true), 0.0, false),
        |_| std::iter::empty()));
}

#[test]
fn cyclic_published_targets_and_steps_are_rejected() {
    assert!(!target_metadata(Some(true), true));
    assert!(target_metadata(Some(true), false));
    assert!(!valid_skill_metadata(Some(CATEGORY_SKILL), None, Some(true), 0.0, true));
    assert!(!sequence_with(&[1], &[0],
        |_| valid_skill_metadata(Some(CATEGORY_SKILL), None, Some(true), 0.0, true),
        |_| std::iter::once((1, 1))));
    let graph = BTreeMap::from([(1, vec![(2, 1)]), (2, vec![(1, 2)])]);
    assert_eq!(required_with(1, None, |id| graph.get(&id).into_iter().flatten().copied()), BTreeMap::from([(1, 2), (2, 1)]));
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn production_rules_and_differential_cases() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = &crate::sde::Sde::load(&db).await.unwrap();
    let skill = sde.types.iter().filter(|ty| valid_skill(sde, ty.type_id) && sde.skill_plans.requirements(ty.type_id).is_empty()).min_by_key(|ty| ty.type_id).unwrap();
    let id = skill.type_id;
    let rank = crate::services::skills::training::skill_type(sde, id).rank;
    assert!(sequence(sde, &[], &[]));
    assert!(sequence(sde, &[id, id], &[1, 2]));
    assert!(sequence(sde, &[id, id, id], &[1, 0, 2]));
    assert!(!sequence(sde, &[id, id], &[0, 0]));
    assert!(!sequence(sde, &[id, id], &[1, 1]));
    assert!(!sequence(sde, &[id], &[2]));
    assert!(target(sde, id, Some(1)));
    assert!(!target(sde, id, Some(0)));
    assert_eq!(required(sde, id, Some(2)).unwrap().get(&id), Some(&2));
    assert_eq!(text_units("🚀"), 2);
    let goal = crate::services::skills::formulas::sp_for_level(rank, 2);
    let trained = BTreeMap::from([(id, crate::dao::skills::training::Skill { type_id: id, level: 1, skill_points: goal / 2 })]);
    let queued = BTreeMap::from([(id, 5)]);
    let milestone = || crate::dao::skills::plan_milestones::Milestone { milestone_id: vec![1; 16], plan_id: vec![2; 16], kind: "skill".into(), type_id: id, level: Some(2), description: String::new() };
    let snapshot = super::super::progress::evaluate(sde, milestone(), &trained, &[], &queued, 0);
    assert!(!snapshot.completed);
    assert_eq!(snapshot.required_sp, goal);
    assert_eq!(snapshot.trained_sp, goal / 2);
    assert_eq!(snapshot.queued_sp, goal - goal / 2);
    let full = BTreeMap::from([(id, crate::dao::skills::training::Skill { type_id: id, level: 2, skill_points: goal })]);
    assert!(super::super::progress::evaluate(sde, milestone(), &full, &[], &queued, 0).completed);
    let empty = crate::dao::skills::plan_milestones::Milestone { level: None, kind: "type".into(), ..milestone() };
    assert!(!super::super::progress::evaluate(sde, empty, &full, &[], &queued, 0).completed);
    let empty_skills = BTreeMap::new();
    let zero_requirements = BTreeMap::from([(1, 0)]);
    let zero = super::super::progress::evaluate_requirements(&zero_requirements, |_| 1.0, &empty_skills, &[], &BTreeMap::new(), 0);
    assert_eq!(zero, (0, 0, 0, true));
    assert_eq!(super::super::progress::evaluate_requirements(&BTreeMap::new(), |_| 1.0, &empty_skills, &[], &BTreeMap::new(), 0), (0, 0, 0, false));
    let graph = BTreeMap::from([(1, vec![(2, 1), (3, 1), (5, 0)]), (2, vec![(4, 2)]), (3, vec![(4, 4)])]);
    let diamond = required_with(1, None, |id| graph.get(&id).into_iter().flatten().copied());
    assert_eq!(text_units("a\u{301}"), 2);
    // Every case carries the complete transitive metadata it needs; edges and maps
    // are arrays of numeric pairs rather than JSON objects with stringified keys.
    let metadata = |roots: &[i32]| {
        let mut pending = roots.to_vec();
        let mut nodes = BTreeMap::new();
        while let Some(node) = pending.pop() {
            if nodes.contains_key(&node) { continue; }
            let ty = sde.types.get(node).expect("fixture type exists");
            let edges = sde.skill_plans.requirements(node).to_vec();
            pending.extend(edges.iter().map(|(id, _)| *id));
            nodes.insert(node, serde_json::json!({
                "type_id": node, "name": ty.name, "category": ty.category_id,
                "group": ty.group_id, "published": ty.published,
                "obsolete": sde.types.attribute(node, ATTR_SKILL_IS_OBSOLETE).unwrap_or(0.0),
                "rank": crate::services::skills::training::skill_type(sde, node).rank,
                "prerequisites": edges
            }));
        }
        nodes.into_values().collect::<Vec<_>>()
    };
    let mut cases = Vec::new();
    let mut add_sequence = |name: &str, steps: Vec<(i32, i32)>| {
        let (ids, levels): (Vec<_>, Vec<_>) = steps.iter().copied().unzip();
        cases.push(serde_json::json!({"id":name,"op":"sequence","types":metadata(&ids),
            "inputs":{"steps":steps},"result":sequence(sde,&ids,&levels)}));
    };
    for (name, levels) in [
        ("empty_sequence", vec![]), ("ordered", vec![1, 2]),
        ("marker_before", vec![0, 1]), ("marker_after", vec![1, 0, 2]),
        ("duplicate_zero", vec![0, 0]), ("duplicate", vec![1, 1]),
        ("missing_level", vec![2]), ("reverse_level", vec![2, 1]),
    ] { add_sequence(name, levels.into_iter().map(|level| (id, level)).collect()); }
    let dependent = sde.types.iter().filter(|ty| {
        let edges = sde.skill_plans.requirements(ty.type_id);
        valid_skill(sde, ty.type_id) && edges.len() == 1 && edges[0].1 == 1
            && valid_skill(sde, edges[0].0) && sde.skill_plans.requirements(edges[0].0).is_empty()
    }).min_by_key(|ty| ty.type_id).expect("simple prerequisite fixture");
    let prerequisite = sde.skill_plans.requirements(dependent.type_id)[0].0;
    add_sequence("prerequisite_ordered", vec![(prerequisite, 1), (dependent.type_id, 1)]);
    add_sequence("prerequisite_missing", vec![(dependent.type_id, 1)]);
    add_sequence("prerequisite_reverse", vec![(dependent.type_id, 1), (prerequisite, 1)]);
    add_sequence("marker_missing_prerequisite", vec![(dependent.type_id, 0)]);
    add_sequence("marker_before_prerequisite_then_positive", vec![(dependent.type_id, 0), (prerequisite, 1), (dependent.type_id, 1)]);
    add_sequence("marker_before_prerequisite_without_positive", vec![(dependent.type_id, 0), (prerequisite, 1)]);
    assert!(!sequence(sde, &[dependent.type_id], &[0]));
    assert!(sequence(sde, &[dependent.type_id, prerequisite, dependent.type_id], &[0, 1, 1]));
    assert!(!sequence(sde, &[dependent.type_id, prerequisite], &[0, 1]));
    for (name, predicate) in [("unpublished", 2), ("non_skill", 3)] {
        let ty = sde.types.iter().filter(|ty| match predicate {
            2 => ty.category_id == Some(CATEGORY_SKILL) && ty.published != Some(true),
            _ => ty.category_id != Some(CATEGORY_SKILL),
        }).min_by_key(|ty| ty.type_id).expect("rejected fixture type");
        add_sequence(name, vec![(ty.type_id, 1)]);
    }
    let mut independent: Vec<_> = sde.types.iter().filter(|ty| valid_skill(sde, ty.type_id)
        && sde.skill_plans.requirements(ty.type_id).is_empty()).map(|ty| ty.type_id).collect();
    independent.sort_unstable();
    assert!(independent.len() >= 31, "need 31 independent skills for step boundary");
    let boundary: Vec<_> = independent[..31].iter().flat_map(|id| (1..=5).map(move |level| (*id, level))).collect();
    for n in [150, 151] {
        add_sequence(&format!("steps_{n}"), boundary[..n].to_vec());
    }
    drop(add_sequence);
    // Synthetic metadata: these published variants are absent from the locked SDE.
    for (name, group, obsolete) in [
        ("synthetic_hidden", Some(GROUP_CLOUD_COMPUTING), 0.0),
        ("synthetic_obsolete", None, 1.0),
    ] {
        let synthetic_id = 1;
        let result = sequence_with(&[synthetic_id], &[1],
            |_| valid_skill_metadata(Some(CATEGORY_SKILL), group, Some(true), obsolete, false),
            |_| std::iter::empty());
        assert!(!result);
        cases.push(serde_json::json!({"id":name,"op":"sequence","types":[{
            "type_id":synthetic_id,"name":name,"category":CATEGORY_SKILL,"group":group,
            "published":true,"obsolete":obsolete,"rank":1.0,"prerequisites":[]
        }],"inputs":{"steps":[[synthetic_id,1]]},"result":result,"fixture":"synthetic metadata"}));
    }
    let unavailable = cases.iter_mut().find(|case| case["id"] == "marker_before_prerequisite_without_positive").unwrap();
    unavailable["client_exception_expected"] = serde_json::json!(true);
    unavailable["source_evidence"] = serde_json::json!("skillPlanValidation.py:244-290 prerequisite and skill ordering; exception expectation requires runner confirmation");
    for level in [0, 1, 5, 6] {
        cases.push(serde_json::json!({"id":format!("skill_target_{level}"),"op":"target","types":metadata(&[id]),
            "inputs":{"type_id":id,"level":level},"result":target(sde,id,Some(level))}));
    }
    let asteroid = sde.types.iter().filter(|t| t.category_id == Some(infinity_core::consts::CATEGORY_ASTEROID) && t.published == Some(true)).min_by_key(|t| t.type_id).unwrap().type_id;
    let no_prereq = sde.types.iter().filter(|t| t.category_id != Some(CATEGORY_SKILL)
        && t.category_id != Some(infinity_core::consts::CATEGORY_ASTEROID) && t.published == Some(true)
        && sde.skill_plans.requirements(t.type_id).is_empty()).min_by_key(|t| t.type_id).unwrap().type_id;
    let citizen = sde.types.iter().filter(|t| t.name.as_deref() == Some("Citizen Miner") && t.published == Some(true)
        && sde.skill_plans.requirements(t.type_id).iter().any(|(_, level)| *level == 0))
        .min_by_key(|t| t.type_id).expect("published Citizen Miner with native zero edge").type_id;
    for (name, type_id) in [("skill_type", id), ("asteroid", asteroid), ("no_prerequisites", no_prereq), ("citizen_miner", citizen)] {
        cases.push(serde_json::json!({"id":name,"op":"target","types":metadata(&[type_id]),
            "inputs":{"type_id":type_id,"level":null},"result":target(sde,type_id,None)}));
        let result: Vec<_> = required(sde,type_id,None).unwrap().into_iter().collect();
        cases.push(serde_json::json!({"id":format!("{name}_required"),"op":"required","types":metadata(&[type_id]),
            "graph":metadata(&[type_id]),"inputs":{"type_id":type_id,"level":null},"result":result}));
    }
    let synthetic_types: Vec<_> = (1..=5).map(|node| serde_json::json!({
        "type_id":node,"category":CATEGORY_SKILL,"group":null,"published":true,"obsolete":0,
        "rank":1.0,"prerequisites":graph.get(&node).cloned().unwrap_or_default()
    })).collect();
    cases.push(serde_json::json!({"id":"diamond_max","op":"required","types":synthetic_types,
        "graph":graph.iter().map(|(k,v)| (*k,v)).collect::<Vec<_>>(),
        "inputs":{"type_id":1,"level":null},"result":diamond.into_iter().collect::<Vec<_>>()}));
    for (name, text, limit) in [
        ("name_50_astral", "🚀".repeat(25), MAX_NAME),
        ("name_51_astral_bmp", format!("{}中", "🚀".repeat(25)), MAX_NAME),
        ("description_1000_combining", "a\u{301}".repeat(500), MAX_DESCRIPTION),
        ("description_1001_combining", format!("{}a", "a\u{301}".repeat(500)), MAX_DESCRIPTION),
        ("name_50_bmp", "中".repeat(50), MAX_NAME),
        ("name_51_bmp", "中".repeat(51), MAX_NAME),
        ("name_51_straddling", format!("{}🚀", "中".repeat(49)), MAX_NAME),
        ("description_1000_bmp", "中".repeat(1000), MAX_DESCRIPTION),
        ("description_1001_bmp", "中".repeat(1001), MAX_DESCRIPTION),
        ("description_1000_astral", "🚀".repeat(500), MAX_DESCRIPTION),
        ("description_1001_astral_bmp", format!("{}中", "🚀".repeat(500)), MAX_DESCRIPTION),
        ("description_1001_straddling", format!("{}🚀", "中".repeat(999)), MAX_DESCRIPTION),
    ] {
        cases.push(serde_json::json!({"id":name,"op":"unicode","types":[],
            "inputs":{"text":text,"limit":limit},"result":{"count":text_units(&text)},
            "note":"UTF-16 code units from the same helper used by personal create and set_field"}));
    }
    for level in 0..=5 {
        cases.push(serde_json::json!({"id":format!("points_{level}"),"op":"points","types":metadata(&[id]),
            "inputs":{"type_id":id,"rank":rank,"level":level},
            "result":crate::services::skills::formulas::sp_for_level(rank,level)}));
    }
    for (name, requirements, trained, queue_level, expert_level) in [
        ("empty", BTreeMap::new(), BTreeMap::new(), 0, 0),
        ("zero_only", BTreeMap::from([(id, 0)]), BTreeMap::new(), 0, 0),
        ("partial_queue_lower", BTreeMap::from([(id, 2)]), BTreeMap::from([(id, crate::dao::skills::training::Skill { type_id:id, level:1, skill_points:goal / 2 })]), 1, 0),
        ("partial_queue_above", BTreeMap::from([(id, 2)]), BTreeMap::from([(id, crate::dao::skills::training::Skill { type_id:id, level:1, skill_points:goal / 2 })]), 5, 0),
        ("expert_not_trained", BTreeMap::from([(id, 2)]), BTreeMap::new(), 0, 5),
        ("full", BTreeMap::from([(id, 2)]), BTreeMap::from([(id, crate::dao::skills::training::Skill { type_id:id, level:2, skill_points:goal })]), 0, 0),
    ] {
        let queued = if queue_level == 0 { BTreeMap::new() } else { BTreeMap::from([(id, queue_level)]) };
        let result = super::super::progress::evaluate_requirements(&requirements, |_| rank, &trained, &[], &queued, 0);
        let trained_inputs: Vec<_> = trained.iter().map(|(id, skill)| (*id, skill.level, skill.skill_points)).collect();
        cases.push(serde_json::json!({"id":name,"op":"progress","types":metadata(&[id]),
            "inputs":{"requirements":requirements.into_iter().collect::<Vec<_>>(),"trained":trained_inputs,
                "queue":[],"queued":queued.into_iter().collect::<Vec<_>>(),"now":0,"expert_level":expert_level},
            "result":{"required_sp":result.0,"trained_sp":result.1,"queued_sp":result.2,"completed":result.3}}));
    }
    let row = crate::dao::skills::queue::QueueEntry { position:0, type_id:id, to_level:2,
        start_sp:0, dest_sp:goal, start_time:Some(0), end_time:Some(100) };
    let estimated = super::super::progress::evaluate_requirements(&BTreeMap::from([(id,2)]), |_| rank,
        &BTreeMap::new(), std::slice::from_ref(&row), &BTreeMap::from([(id,2)]), 50);
    cases.push(serde_json::json!({"id":"estimated_partial","op":"progress","types":metadata(&[id]),
        "inputs":{"requirements":[[id,2]],"trained":[],"queue":[{"position":0,"type_id":id,"to_level":2,
            "start_sp":0,"dest_sp":goal,"start_time":0,"end_time":100}],"queued":[[id,2]],"now":50,"expert_level":0},
        "result":{"required_sp":estimated.0,"trained_sp":estimated.1,"queued_sp":estimated.2,"completed":estimated.3}}));
    let citizen_snapshot = super::super::progress::evaluate(sde,
        crate::dao::skills::plan_milestones::Milestone { level:None, kind:"type".into(), type_id:citizen, ..milestone() },
        &BTreeMap::new(), &[], &BTreeMap::new(), 0);
    assert_eq!((citizen_snapshot.required_sp, citizen_snapshot.trained_sp, citizen_snapshot.queued_sp, citizen_snapshot.completed), (0,0,0,true));
    cases.push(serde_json::json!({"id":"citizen_miner_progress","op":"progress","types":metadata(&[citizen]),
        "graph":metadata(&[citizen]),"inputs":{"type_id":citizen,"level":null,"trained":[],"queue":[],"queued":[],"now":0},
        "result":{"required_sp":citizen_snapshot.required_sp,"trained_sp":citizen_snapshot.trained_sp,
            "queued_sp":citizen_snapshot.queued_sp,"completed":citizen_snapshot.completed}}));
    println!("{}", serde_json::json!({"cases":cases}));
}
