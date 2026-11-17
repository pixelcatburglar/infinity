use super::*;
use super::super::{control::TimerKind, State};
#[test]
fn timer_write_and_expiry_track_generation_per_kind() {
    for &kind in &TimerKind::ALL {
        let mut state = State::default();
        let first = write_deadline(&mut state, 1, kind, 100, false, 0).unwrap();
        assert!(write_deadline(&mut state, 1, kind, 200, true, 0).is_none());
        assert!((state.pilots[&1].until(kind), expiry(&state.pilots[&1], kind, first, 150)) == (200, Expiry::Pending));
        let shorter = write_deadline(&mut state, 1, kind, 50, false, 0).unwrap();
        assert!(shorter != first);
        let pilot = &state.pilots[&1];
        assert!(pilot.until(kind) == 50);
        assert!((expiry(pilot, kind, first, 60), expiry(pilot, kind, shorter, 40), expiry(pilot, kind, shorter, 60)) == (Expiry::Stale, Expiry::Pending, Expiry::Due));
        write_deadline(&mut state, 1, kind, 0, false, 0);
        assert!(expiry(&state.pilots[&1], kind, shorter, 60) == Expiry::Stale);
    }
}
