//! Exercises the installed hook in an isolated test process.
use std::{env, process::Command};

#[test]
fn hook_logs_error_with_location() {
    const CHILD: &str = "INFINITY_PANIC_REPORT_TEST_CHILD";
    const NAME: &str = "panic_report::tests::hook_logs_error_with_location";
    if env::var_os(CHILD).is_some() {
        tracing_subscriber::fmt().without_time().with_ansi(false)
            .with_target(false).with_writer(std::io::stderr).init();
        super::install();
        panic!("panic hook probe");
    }
    let output = Command::new(env::current_exe().unwrap())
        .args(["--exact", NAME, "--nocapture"])
        .env(CHILD, "1").output().unwrap();
    assert!(!output.status.success());
    let log = String::from_utf8(output.stderr).unwrap();
    let errors: Vec<_> = log.lines().filter(|line| line.starts_with("ERROR ")).collect();
    assert_eq!(errors.len(), 1, "{log}");
    assert!(errors[0].contains("location=") && errors[0].contains(file!()), "{log}");
}
