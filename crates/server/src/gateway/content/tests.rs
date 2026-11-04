use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn checkpoints_build() {
    let gw = super::super::tests::gateway().await;
    let ctx = HandlerContext::for_request(&gw, super::super::Caller::default()).await.unwrap();
    for (req, content) in [
        ("eve_public.achievement.definition.api.cdn.GetLatestRequest", "achievements"),
        ("eve_public.cosmetic.character.title.definition.api.GetLatestRequest", "titles"),
        ("eve_public.space.api.cdn.GetUpdateRequest", "maptags"),
    ] {
        let m = checkpoint(&ctx, req, content).unwrap();
        assert!(format!("{m}").contains(&format!("{}/content/{content}", gw.config.cdn_url())));
    }
}
