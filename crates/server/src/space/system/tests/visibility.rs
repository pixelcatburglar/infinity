use super::*;

#[test]
fn actor_visibility_is_independent_of_delivery_observer() {
    let mut s = System::new(1, Arc::default(), 1000);
    let mut ego = ball(101, Some(90_000_001), 0.0);
    ego.cloak = 1;
    s.insert(ego);
    s.insert(ball(102, Some(90_000_002), 1.0));
    let mut hidden = ball(103, Some(90_000_003), 2.0);
    hidden.cloak = 1;
    s.insert(hidden);
    let actor = 90_000_001;
    let check = |s: &System, delivery: bool| {
        for (target, visible) in [(101, true), (102, true), (103, false), (999, false)] {
            assert_eq!(s.visible_to(actor, target), visible);
            assert_eq!(s.fleet_broadcast_visible(actor, target), delivery && visible);
        }
        assert!(!s.visible_to(90_000_099, 102));
        assert!(!s.fleet_broadcast_visible(90_000_099, 102));
    };
    check(&s, false);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    s.observe(1_000_000_000_001, 101, tx);
    check(&s, true);
    drop(rx);
    check(&s, false);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    s.observe(1_000_000_000_001, 102, tx);
    check(&s, false);
}
