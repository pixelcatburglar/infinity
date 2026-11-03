use super::*;

#[test]
fn checkpoints_build() {
    let gw = Gateway { cdn_url: "http://127.0.0.1".into(), ..Default::default() };
    for (req, content) in [
        ("eve_public.achievement.definition.api.cdn.GetLatestRequest", "achievements"),
        ("eve_public.cosmetic.character.title.definition.api.GetLatestRequest", "titles"),
        ("eve_public.space.api.cdn.GetUpdateRequest", "maptags"),
    ] {
        let m = checkpoint(&gw, req, content).unwrap();
        assert!(format!("{m}").contains(&format!("http://127.0.0.1/content/{content}")));
    }
}
