//! Grid topology rules: floor cells, neighbour inheritance, release and
//! monotonic labels.

use super::*;
use crate::space::system::tests::ball;

#[test]
fn edge_coordinates_floor_or_leave_no_cell() {
    let mut interest = Interest::default();
    // Both -1 and -BUBBLE_EDGE floor into axis cell -1: one box, one label.
    interest.place(1, [-1.0, -1.0, -1.0]);
    interest.place(2, [-BUBBLE_EDGE, -BUBBLE_EDGE, -BUBBLE_EDGE]);
    assert_eq!(interest.bubble_of(1), interest.bubble_of(2));
    interest.place(3, [1.0, 0.0, 0.0]);
    let key = Interest::key([-1.0, -1.0, -1.0]).unwrap();
    assert_eq!((key.x, key.y, key.z), (-1, -1, -1));
    let key = Interest::key([1.0, 0.0, 0.0]).unwrap();
    assert_eq!((key.x, key.y, key.z), (0, 0, 0));
    assert_eq!(interest.bubble_of(1), interest.bubble_of(3));
    interest.place(4, [f64::NAN, 0.0, 0.0]);
    assert!(interest.bubble_of(4).is_none());
}

#[test]
fn last_occupant_frees_the_box_and_label_without_reuse() {
    let mut interest = Interest::default();
    interest.place(1, [0.0, 0.0, 0.0]);
    let first = interest.bubble_of(1).unwrap();
    interest.remove(1);
    assert!(interest.bubble_of(1).is_none());
    assert!(!interest.labels.contains_key(&first));
    interest.place(2, [3.0 * BUBBLE_EDGE, 0.0, 0.0]);
    let second = interest.bubble_of(2).unwrap();
    assert!(second.0 > first.0);
}

#[test]
fn neighbours_inherit_far_boxes_start_fresh_and_labels_never_merge() {
    let mut interest = Interest::default();
    interest.place(1, [0.0, 0.0, 0.0]);
    interest.place(2, [BUBBLE_EDGE, 0.0, 0.0]);
    let a = interest.bubble_of(1).unwrap();
    assert_eq!(interest.bubble_of(2), Some(a));
    // Two empty cells away: no labelled neighbour, a fresh monotonic id.
    interest.place(3, [3.0 * BUBBLE_EDGE, 0.0, 0.0]);
    let b = interest.bubble_of(3).unwrap();
    assert_ne!(a, b);
    assert!(b.0 > a.0);
    // A bridging box adopts the first labelled neighbour only: no merge.
    interest.place(4, [2.0 * BUBBLE_EDGE, 0.0, 0.0]);
    assert_eq!(interest.bubble_of(4), Some(a));
    assert_eq!(interest.bubble_of(3), Some(b));
    interest.place(5, [0.0, BUBBLE_EDGE, 0.0]);
    assert_eq!(interest.bubble_of(5), Some(a));
}

#[test]
fn global_and_cloaked_balls_occupy_boxes() {
    // Global and cloaked balls occupy boxes like any other ball.
    let mut s = System::new(0, Arc::default(), 0);
    let mut cloaked = ball(1, None, 0.0);
    cloaked.cloak = 1;
    let mut global = ball(2, None, 2.0);
    global.global = true;
    s.insert(cloaked);
    s.insert(global);
    assert_eq!(s.bubble_of(1), s.bubble_of(2));
    s.take(1);
    assert!(s.bubble_of(1).is_none());
    assert!(s.bubble_of(2).is_some());
}

#[test]
fn local_pair_matches_visibility_with_borrowed_source() {
    let mut s = System::new(0, Arc::default(), 0);
    s.insert(ball(1, Some(100), 0.0));
    s.insert(ball(2, None, 1.0));
    s.insert(ball(3, None, 3.0 * BUBBLE_EDGE));
    let mut global = ball(4, None, 4.0 * BUBBLE_EDGE);
    global.global = true;
    s.insert(global);
    assert!(s.local_pair_from(s.ball(1).unwrap(), s.ball(1).unwrap()));
    assert!(s.local_pair_from(s.ball(1).unwrap(), s.ball(2).unwrap()));
    assert!(!s.local_pair_from(s.ball(1).unwrap(), s.ball(3).unwrap()));
    assert!(s.local_pair_from(s.ball(1).unwrap(), s.ball(4).unwrap()));
    // An uninserted ball fixture stands in for the missing id in both
    // directions: absent target membership and absent source token refuse.
    let absent = ball(99, None, 0.0);
    assert!(!s.local_pair_from(s.ball(1).unwrap(), &absent));
    assert!(!s.local_pair_from(&absent, s.ball(1).unwrap()));
    s.balls.get_mut(&2).unwrap().cloak = 1;
    assert!(!s.local_pair_from(s.ball(1).unwrap(), s.ball(2).unwrap()));
    assert!(!s.visible_to(100, 2));
    s.balls.get_mut(&1).unwrap().cloak = 1;
    assert!(s.local_pair_from(s.ball(1).unwrap(), s.ball(1).unwrap()));
    assert!(s.local_pair_from(s.ball(1).unwrap(), s.ball(4).unwrap()));
    let source = s.balls.remove(&1).unwrap();
    assert!(s.local_pair_from(&source, s.ball(4).unwrap()));
    assert!(!s.local_pair_from(&source, s.ball(2).unwrap()));
    s.balls.insert(1, source);
    assert!(s.visible_to(100, 4));
    assert!(!s.visible_to(100, 3));
}
