use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn exact_claims_and_neighboring_requests() {
    validate().unwrap();
    let gw = super::super::tests::gateway().await;
    let context = HandlerContext::for_request(&gw, super::super::Caller::default()).await.unwrap();
    assert_eq!(CLAIMS.len(), 25);
    let expected = [
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
        "eve_public.cosmetic.character.title.definition.api.GetLatestRequest",
        "eve_public.space.api.cdn.GetUpdateRequest",
        "eve_public.career.goal.api.GetDefinitionsRequest",
        "eve_public.career.goal.api.GetAllRequest",
        "eve_public.pirate.corruption.api.GetStageThresholdsRequest",
        "eve_public.pirate.suppression.api.GetStageThresholdsRequest",
        "eve_public.character.GetNamesByUserRequest",
        "eve_public.plex.vault.api.BalanceRequest",
        "eve_public.cosmetic.ship.api.GetRequest",
        "eve_public.cosmetic.ship.api.GetAllInBubbleRequest",
        "eve_public.character.skill.plan.CreateRequest",
        "eve_public.character.skill.plan.GetAllRequest",
        "eve_public.character.skill.plan.GetRequest",
        "eve_public.character.skill.plan.GetSharedRequest",
        "eve_public.character.skill.plan.GetActiveRequest",
        "eve_public.character.skill.plan.SetActiveRequest",
        "eve_public.character.skill.plan.DeleteRequest",
        "eve_public.character.skill.plan.SetNameRequest",
        "eve_public.character.skill.plan.SetDescriptionRequest",
        "eve_public.character.skill.plan.SetSkillRequirementsRequest",
        "eve_public.character.skill.plan.milestone.GetAllRequest",
        "eve_public.character.skill.plan.milestone.CreateRequest",
        "eve_public.character.skill.plan.milestone.DeleteRequest",
        "eve_public.character.skill.plan.milestone.SetDescriptionRequest",
    ];
    for name in expected { assert!(CLAIMS.iter().any(|(claim, _)| *claim == name), "{name}"); }
    for (name, _) in CLAIMS {
        let req = DynamicMessage::new(message(name).unwrap());
        assert!(matches!(handle(&context, name, &req).await, Dispatch::Claimed(_)), "{name}");
    }
    let req = DynamicMessage::new(message(CLAIMS[0].0).unwrap());
    for name in ["eve_public.character.skill.plan.FutureRequest", "eve_public.character.skill.plan.milestone.GetRequest", "eve_public.achievement.api.FutureRequest", "eve_public.character.FutureRequest"] {
        assert!(matches!(handle(&context, name, &req).await, Dispatch::Unclaimed));
    }
}

#[test]
fn actual_rust_claims_are_distinct_from_daily_reservation() {
    assert!(is_implemented("eve_public.character.GetNamesByUserRequest"));
    assert!(is_implemented("eve_public.character.skill.plan.GetRequest"));
    assert!(!is_implemented("eve_public.dailygoal.api.GetRequest"));
    assert!(!is_implemented("eve_public.character.skill.plan.FutureRequest"));
    assert!(super::super::daily_goals::supported("eve_public.dailygoal.api.GetRequest"));
}
