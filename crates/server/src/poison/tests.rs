use super::{STATE_EXIT_CODE, lock_or_exit};
use std::process::Command;
use std::sync::{Arc, Mutex};

const CHILD_MODE: &str = "INFINITY_POISON_TEST_MODE";

struct PoisonOnDrop(Arc<Mutex<()>>);
impl Drop for PoisonOnDrop {
    fn drop(&mut self) { drop(lock_or_exit(self.0.lock())); }
}

/// A genuinely poisoned mutex: a real holder panicked with the guard out.
fn poisoned_mutex() -> Arc<Mutex<()>> {
    let mutex = Arc::new(Mutex::new(()));
    let poisoner = Arc::clone(&mutex);
    let joined = std::thread::spawn(move || {
        let _guard = poisoner.lock().unwrap();
        panic!("test lock poisoner");
    }).join();
    assert!(joined.is_err() && mutex.is_poisoned());
    mutex
}

fn consume_direct() -> ! {
    drop(lock_or_exit(poisoned_mutex().lock()));
    std::process::exit(0);
}

fn consume_during_unwind() -> ! {
    let _consumer = PoisonOnDrop(poisoned_mutex());
    panic!("test outer panic");
}

#[test]
fn poisoned_shared_lock_exits_after_stderr() {
    match std::env::var(CHILD_MODE).as_deref() {
        Ok("direct") => consume_direct(),
        Ok("unwind") => consume_during_unwind(),
        Ok("containment") => crate::poison::containment_lost(crate::space::containment::ContainmentFailure::CleanupOwnerLost, 7, Some(11)),
        _ => {}
    }
    for mode in ["direct", "unwind", "containment"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "poison::tests::poisoned_shared_lock_exits_after_stderr"])
            .env(CHILD_MODE, mode)
            .output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(STATE_EXIT_CODE), "{mode}: {stderr}");
        assert!(stderr.contains(&format!("commit {}", crate::build_identity::commit())), "{mode}: {stderr}");
        if mode == "containment" {
            assert!(stderr.contains("CleanupOwnerLost") && stderr.contains("system 7"), "{mode}: {stderr}");
        } else {
            assert!(stderr.contains("shared lock poisoned"), "{mode}: {stderr}");
            assert!(stderr.contains("poison/tests.rs"), "{mode}: {stderr}");
        }
    }
}
