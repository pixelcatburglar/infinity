//! Configuration loading, validation, precedence and PostgreSQL conversion tests.

use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        loop {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("infinity-config-test-{}-{id}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("creating fixture directory: {error}"),
            }
        }
    }

    fn path(&self) -> PathBuf { self.0.join("config.toml") }

    fn write(&self, contents: &str) {
        std::fs::write(self.path(), contents).expect("writing configuration fixture");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn load_error(contents: &str) -> String {
    let fixture = Fixture::new();
    fixture.write(contents);
    match Config::load_with(&fixture.path(), |_| None) {
        Ok(_) => panic!("configuration unexpectedly accepted: {contents}"),
        Err(error) => format!("{error:#}"),
    }
}
#[test]
fn gm_spawn_policy_rejects_invalid_limits() {
    let mut config = Config::default();
    assert!(config.validate().is_ok());
    config.gm.spawn_distance_m = f64::NAN;
    assert!(config.validate().is_err());
    config.gm.spawn_distance_m = 0.0;
    config.gm.clear_radius_m = -1.0;
    assert!(config.validate().is_err());
    config.gm.clear_radius_m = 0.0;
    config.gm.max_spawn_count = 0;
    assert!(config.validate().is_err());
}


#[test]
fn trade_timeout_is_parsed_and_must_be_positive() {
    let fixture = Fixture::new();
    fixture.write("[trade]\nidle_timeout_secs = 42");
    let config = Config::load_with(&fixture.path(), |_| None).unwrap();
    assert_eq!(config.game_config().trade.idle_timeout_secs, 42);
    assert!(load_error("[trade]\nidle_timeout_secs = 0").contains("trade.idle_timeout_secs"));
    assert!(load_error("[trade]\nidle_timeout_secs = 9223372036854775807").contains("trade.idle_timeout_secs"));
}

#[test]
fn map_policy_defaults_custom_bounds_and_overflow() {
    let fixture = Fixture::new();
    fixture.write("");
    let defaults = Config::load_with(&fixture.path(), |_| None).unwrap().game_config();
    assert_eq!((defaults.map.history_retention_hours, defaults.map.maintenance_secs), (48, 3600));
    fixture.write("[map]\nhistory_retention_hours = 24\nmaintenance_secs = 1");
    let config = Config::load_with(&fixture.path(), |_| None).unwrap().game_config();
    assert_eq!((config.map.history_retention_hours, config.map.maintenance_secs), (24, 1));
    fixture.write("[map]\nhistory_retention_hours = 72\nmaintenance_secs = 120");
    let config = Config::load_with(&fixture.path(), |_| None).unwrap().game_config();
    assert_eq!((config.map.history_retention_hours, config.map.maintenance_secs), (72, 120));
    for key in ["history_retention_hours", "maintenance_secs"] {
        for value in ["0", "-1", "9223372036854775807"] {
            let error = load_error(&format!("[map]\n{key} = {value}"));
            assert!(error.contains(&format!("map.{key}")), "{error}");
        }
    }
    assert!(load_error("[map]\nhistory_retention_hours = 23").contains("map.history_retention_hours"));
    assert!(load_error("[map]\nunknown = 1").contains("unknown"));
}

#[test]
fn mining_ledger_policy_defaults_parsing_and_validation() {
    let fixture = Fixture::new();
    fixture.write("");
    let defaults = Config::load_with(&fixture.path(), |_| None).unwrap().game_config();
    assert_eq!((defaults.mining.ledger_retention_days, defaults.mining.ledger_maintenance_secs), (90, 3600));

    fixture.write("[mining]\nledger_retention_days = 45\nledger_maintenance_secs = 120");
    let config = Config::load_with(&fixture.path(), |_| None).unwrap().game_config();
    assert_eq!((config.mining.ledger_retention_days, config.mining.ledger_maintenance_secs), (45, 120));

    for key in ["ledger_retention_days", "ledger_maintenance_secs"] {
        for value in ["0", "-1", "9223372036854775807"] {
            let error = load_error(&format!("[mining]\n{key} = {value}"));
            assert!(error.contains(&format!("mining.{key}")), "{error}");
        }
    }
}

#[test]
fn salvage_policy_parses_and_rejects_invalid_bounds() {
    let fixture = Fixture::new();
    fixture.write("[salvage]\nsalvage_material_stacks = 2\nsalvage_quantity_min = 4\nsalvage_quantity_max = 5");
    let config = Config::load_with(&fixture.path(), |_| None).unwrap().game_config();
    assert_eq!((config.salvage.salvage_material_stacks, config.salvage.salvage_quantity_min, config.salvage.salvage_quantity_max), (2, 4, 5));
    assert!(load_error("[salvage]\nsalvage_material_stacks = 0").contains("salvage.salvage_material_stacks"));
    assert!(load_error("[salvage]\nsalvage_quantity_min = 0").contains("salvage.salvage_quantity_min"));
    assert!(load_error("[salvage]\nsalvage_quantity_min = 4\nsalvage_quantity_max = 3").contains("salvage.salvage_quantity_max"));
    assert!(load_error("[salvage]\nsalvage_quantity_max = 2147483648").contains("salvage.salvage_quantity_max"));
}

#[test]
fn unknown_fields_are_rejected_at_every_level() {
    for (contents, key) in [
        ("surprise = 1", "surprise"),
        ("[unknown_section]\nvalue = 1", "unknown_section"),
        ("[server]\nunknown_deployment = 1", "unknown_deployment"),
        ("[market]\nunknown_game = 1", "unknown_game"),
        ("[store]\ndaily_gift = { type_id = 1, quantity = 1, unexpected_gift = 1 }", "unexpected_gift"),
        ("[missions]\nrewards = [{ isk = 1, lp = 1, unexpected_reward = 1 }]", "unexpected_reward"),
    ] {
        let error = load_error(contents);
        assert!(error.contains(key), "expected {key} in {error}");
    }
}

#[test]
fn invalid_numeric_values_report_field_and_value() {
    for (contents, field, value) in [
        ("[market]\nsales_tax = 1.5", "market.sales_tax", "1.5"),
        ("[database]\npool_size = 0", "database.pool_size", "0"),
        ("[missions]\nspawn_range_m = [9, 2]", "missions.spawn_range_m", "9"),
        ("[crimewatch]\noutlaw_threshold = -11.0", "crimewatch.outlaw_threshold", "-11"),
        ("[characters]\nstarting_isk = 1e34", "characters.starting_isk", "10000000000000000000000000000000000"),
        ("[accounts]\nstarting_plex = -1", "accounts.starting_plex", "-1"),
        ("[characters]\nname_max_spaces = 0", "characters.name_max_spaces", "0"),
        ("[characters]\nfree_respecs = 0", "characters.free_respecs", "0"),
        ("[crimewatch]\nconcord_base_delay_secs = 0", "crimewatch.concord_base_delay_secs", "0"),
        ("[crimewatch]\nconcord_delay_per_security_secs = 0", "crimewatch.concord_delay_per_security_secs", "0"),
        ("[crimewatch]\nconcord_base_delay_secs = 6.5", "crimewatch.concord_base_delay_secs", "6.5"),
        ("[crimewatch]\nconcord_delay_per_security_secs = 28.5", "crimewatch.concord_delay_per_security_secs", "28.5"),
        ("[bounty]\nkill_right_lifetime_days = 0", "bounty.kill_right_lifetime_days", "0"),
        ("[bounty]\nkill_right_suspect_secs = 0", "bounty.kill_right_suspect_secs", "0"),
    ] {
        let error = load_error(contents);
        assert!(error.contains(field) && error.contains(value), "expected {field} and {value} in {error}");
    }
    let mut config = Config::default();
    config.market.sales_tax = f64::NAN;
    let error = config.validate().expect_err("NaN must be rejected").to_string();
    assert!(error.contains("market.sales_tax") && error.contains("NaN"), "{error}");
    config.market.sales_tax = f64::INFINITY;
    let error = config.validate().expect_err("infinity must be rejected").to_string();
    assert!(error.contains("market.sales_tax") && error.contains("inf"), "{error}");
}

#[test]
fn representation_overflows_are_rejected_with_the_key_and_value() {
    for (contents, key, value) in [
        ("[store]\ndaily_gift = { type_id = 2147483648, quantity = 1 }", "store.daily_gift.type_id", "2147483648"),
        ("[store]\ndaily_gift = { type_id = 1, quantity = 2147483648 }", "store.daily_gift.quantity", "2147483648"),
        ("[market]\nmax_order_days = 32768", "market.max_order_days", "32768"),
        ("[market]\norder_history_days = 9223372036854775807", "market.order_history_days", "9223372036854775807"),
        ("[characters]\ndelete_delay_secs = 922337203685", "characters.delete_delay_secs", "922337203685"),
        ("[contracts]\nexpiry_max_days = 9223372036854775807", "contracts.expiry_max_days", "9223372036854775807"),
        ("[contracts]\ncourier_max_days = 2147483648", "contracts.courier_max_days", "2147483648"),
        ("[npc]\nhacked_site_linger_secs = 9223372036854775807", "npc.hacked_site_linger_secs", "9223372036854775807"),
        ("[crimewatch]\nweapons_timer_secs = 9223372036854775807", "crimewatch.weapons_timer_secs", "9223372036854775807"),
        ("[crimewatch]\nconcord_base_delay_secs = 9223372036854775807", "crimewatch.concord_base_delay_secs", "9223372036854775807"),
        ("[crimewatch]\nconcord_delay_per_security_secs = 9223372036854775807", "crimewatch.concord_delay_per_security_secs", "9223372036854775807"),
        ("[space]\nposition_snapshot_secs = 9223372036854775807", "space.position_snapshot_secs", "9223372036854775807"),
        ("[bounty]\nkill_right_lifetime_days = 9223372036854775807", "bounty.kill_right_lifetime_days", "9223372036854775807"),
        ("[bounty]\nkill_right_suspect_secs = 9223372036854775807", "bounty.kill_right_suspect_secs", "9223372036854775807"),
    ] {
        let error = load_error(contents);
        assert!(error.contains(key) && error.contains(value), "{error}");
    }
    let error = load_error("[crimewatch]\nconcord_base_delay_secs = 7000000000000000\nconcord_delay_per_security_secs = 6000000000000000");
    assert!(error.contains("crimewatch.concord_base_delay_secs=7000000000000000") && error.contains("crimewatch.concord_delay_per_security_secs=6000000000000000"), "{error}");
    let mut config = Config::default();
    config.corporations.office_rent_base_isk = f64::MAX;
    config.corporations.office_rent_per_security_isk = f64::MAX;
    let error = config.validate().expect_err("overflowing rent is rejected").to_string();
    assert!(error.contains("corporations.office_rent_base_isk") && error.contains("corporations.office_rent_per_security_isk"), "{error}");
}

#[test]
fn millisecond_deadline_overflow_is_rejected_at_startup() {
    let unit = crate::space::seconds_to_ms(1.0) as i64;
    let seconds = i64::MAX / unit;
    let interval = seconds.checked_mul(unit).expect("interval must fit in i64");
    assert!(crate::space::now_ms().checked_add(interval).is_none());
    let error = load_error(&format!("[space]\nposition_snapshot_secs = {seconds}"));
    assert!(error.contains("space.position_snapshot_secs") && error.contains(&seconds.to_string()), "{error}");
}

#[test]
fn starting_balances_accept_their_ceiling_and_reject_the_next_value() {
    let mut config = Config::default();
    config.accounts.starting_plex = i64::MAX;
    config.validate().expect("starting PLEX at i64::MAX is accepted");
    config.accounts.starting_plex = -1;
    let error = config.validate().expect_err("negative starting PLEX is rejected").to_string();
    assert!(error.contains("accounts.starting_plex") && error.contains("-1"), "{error}");

    config = Config::default();
    config.characters.starting_isk = 9_999_999_999_999.99;
    config.validate().expect("starting ISK at the ceiling is accepted");
    let next = 10_000_000_000_000.00_f64;
    config.characters.starting_isk = next;
    let error = config.validate().expect_err("starting ISK above the ceiling is rejected").to_string();
    assert!(error.contains("characters.starting_isk") && error.contains(&next.to_string()), "{error}");
}

#[test]
fn deployment_environment_wins_but_game_environment_is_ignored() {
    let fixture = Fixture::new();
    fixture.write("[server]\nbind = 'file-address'\n[market]\nsales_tax = 0.25\n");
    let config = Config::load_with(&fixture.path(), |key| match key {
        "BIND" => Some("environment-address".into()),
        "SALES_TAX" => Some("0.75".into()),
        _ => None,
    }).expect("loading overrides");
    assert_eq!(config.server.bind, "environment-address");
    assert_eq!(config.market.sales_tax, 0.25);
    assert_eq!(config.source(), fixture.path().to_str().unwrap());
}

#[test]
fn logging_defaults_and_rotation_validation() {
    let fixture = Fixture::new();
    let defaults = Config::load_with(&fixture.path(), |_| None).expect("loading defaults");
    assert_eq!(defaults.logging.dir, "");
    assert_eq!(defaults.logging.rotation, "daily");
    assert_eq!(defaults.logging.keep_files, 14);

    for rotation in ["minutely", "hourly", "daily", "never"] {
        fixture.write(&format!("[logging]\nrotation = '{rotation}'\nkeep_files = 1\n"));
        Config::load_with(&fixture.path(), |_| None).expect("supported rotation");
    }
    let error = load_error("[logging]\nrotation = 'weekly'");
    assert!(error.contains("logging.rotation") && error.contains("weekly"), "{error}");
    for rotation in ["daily", "never"] {
        let error = load_error(&format!("[logging]\nrotation = '{rotation}'\nkeep_files = 0"));
        assert!(error.contains("logging.keep_files") && error.contains('0'), "{error}");
    }
}

#[test]
fn logging_directory_environment_overrides_file_including_empty() {
    let fixture = Fixture::new();
    fixture.write("[logging]\ndir = 'file-logs'\n");
    let from_file = Config::load_with(&fixture.path(), |_| None).expect("loading file directory");
    assert_eq!(from_file.logging.dir, "file-logs");
    for dir in ["environment-logs", ""] {
        let config = Config::load_with(&fixture.path(), |key| {
            (key == "LOG_DIR").then(|| dir.to_owned())
        }).expect("loading directory override");
        assert_eq!(config.logging.dir, dir);
    }
}

#[test]
fn missing_file_matches_complete_example_and_defaults() {
    let fixture = Fixture::new();
    let missing = Config::load_with(&fixture.path(), |_| None).expect("missing file uses defaults");
    let example: Config = toml::from_str(include_str!("../../../../config.example.toml"))
        .expect("complete example parses");
    example.validate().expect("complete example validates");
    assert_eq!(missing.source(), "defaults");
    assert!(missing == Config::default());
    assert!(missing.server == example.server && missing.database == example.database);
    assert!(missing.development == example.development && missing.logging == example.logging);
    assert!(missing.chat == example.chat && missing.images == example.images);
    assert!(missing.gateway == example.gateway && missing.cdn == example.cdn);
    assert!(missing.game_config() == example.game_config());
}

#[test]
fn malformed_values_do_not_leak_database_credentials() {
    let fixture = Fixture::new();
    fixture.write("[database]\npassword = 'file-secret'\npool_size = 'bad'\n");
    let error = match Config::load_with(&fixture.path(), |_| None) {
        Ok(_) => panic!("bad TOML accepted"),
        Err(error) => format!("{error:#}"),
    };
    assert!(!error.contains("file-secret"), "{error}");
    fixture.write("[database]\npassword = 'file-secret'\n");
    let error = match Config::load_with(&fixture.path(), |key| match key {
        "DATABASE_POOL_SIZE" => Some("not-a-number".into()),
        "DATABASE_URL" => Some("postgres://user:url-secret@localhost/db".into()),
        "DATABASE_PASSWORD" => Some("env-secret".into()),
        _ => None,
    }) {
        Ok(_) => panic!("bad environment value accepted"),
        Err(error) => format!("{error:#}"),
    };
    assert!(error.contains("DATABASE_POOL_SIZE") && error.contains("not-a-number"), "{error}");
    for secret in ["file-secret", "url-secret", "env-secret"] {
        assert!(!error.contains(secret), "credential leaked: {error}");
    }
}

#[test]
fn database_parse_errors_report_paths_and_safe_values() {
    let fixture = Fixture::new();
    fixture.write("[database]\npool_size = -1\n");
    let error = match Config::load_with(&fixture.path(), |_| None) {
        Ok(_) => panic!("negative pool size accepted"),
        Err(error) => format!("{error:#}"),
    };
    assert!(error.contains("database.pool_size") && error.contains("-1"), "{error}");
    assert!(error.contains(fixture.path().to_str().unwrap()), "{error}");

    let error = load_error("[database]\npassword = 42");
    assert!(error.contains("database.password") && error.contains("42"), "{error}");
    let error = load_error("[database]\npassword = ['secret']");
    assert!(error.contains("database.password") && !error.contains("secret"), "{error}");
    let error = load_error("database = { unknown_inline = 1 }");
    assert!(error.contains("unknown_inline"), "{error}");
}

#[test]
fn database_individual_fields_are_required_only_without_url() {
    for field in ["host", "user", "name"] {
        let error = load_error(&format!("[database]\nurl = ''\n{field} = ''"));
        assert!(error.contains(&format!("database.{field}")), "{error}");
    }
    let fixture = Fixture::new();
    fixture.write("[database]\nurl = 'postgres://user@host/db'\nhost = ''\nuser = ''\nname = ''");
    Config::load_with(&fixture.path(), |_| None).expect("URL takes precedence over empty individual fields");
}

#[test]
fn postgres_conversion_uses_url_or_individual_fields_and_applies_common_options() {
    let mut database = Config::default().database;
    database.host = "individual-host".into();
    database.port = 5433;
    database.user = "individual-user".into();
    database.password = "individual-password".into();
    database.name = "individual-db".into();
    database.application_name = "configured-app".into();
    database.connect_timeout_secs = 17;
    let pg = database.to_pg_config().expect("individual fields");
    assert!(matches!(&pg.get_hosts()[0], tokio_postgres::config::Host::Tcp(host) if host == "individual-host"));
    assert_eq!(pg.get_ports(), &[5433]);
    assert_eq!(pg.get_user(), Some("individual-user"));
    assert_eq!(pg.get_password(), Some(&b"individual-password"[..]));
    assert_eq!(pg.get_dbname(), Some("individual-db"));
    assert_eq!(pg.get_application_name(), Some("configured-app"));
    assert_eq!(pg.get_connect_timeout(), Some(&Duration::from_secs(17)));

    database.url = "postgres://url-user:url-password@url-host:5434/url-db?application_name=url-app&connect_timeout=3".into();
    let pg = database.to_pg_config().expect("connection URL");
    assert!(matches!(&pg.get_hosts()[0], tokio_postgres::config::Host::Tcp(host) if host == "url-host"));
    assert_eq!(pg.get_ports(), &[5434]);
    assert_eq!(pg.get_user(), Some("url-user"));
    assert_eq!(pg.get_password(), Some(&b"url-password"[..]));
    assert_eq!(pg.get_dbname(), Some("url-db"));
    assert_eq!(pg.get_application_name(), Some("configured-app"));
    assert_eq!(pg.get_connect_timeout(), Some(&Duration::from_secs(17)));

    database.url = "postgres://user:secret@host/db?invalid_option=1".into();
    let error = match database.to_pg_config() {
        Ok(_) => panic!("invalid URL accepted"),
        Err(error) => format!("{error:#}"),
    };
    assert!(!error.contains("secret"), "{error}");
}

pub(crate) fn test_database() -> deployment::DatabaseConfig {
    crate::tests::init_config();
    deployment::DatabaseConfig {
        url: std::env::var("INFINITY_TEST_DATABASE_URL").expect("INFINITY_TEST_DATABASE_URL must name a dedicated test database"),
        ..Default::default()
    }
}

#[test]
fn scheduler_validation_rejects_invalid_limits() {
    let mut config = Config::default();
    assert!(config.validate().is_ok());
    config.scheduler.lease_secs = config.scheduler.poll_secs;
    assert!(config.validate().is_err());
    config.scheduler = scheduler::SchedulerConfig::default();
    config.scheduler.max_concurrent = 0;
    assert!(config.validate().is_err());
    config.scheduler = scheduler::SchedulerConfig::default();
    config.contracts.maintenance_secs = 0;
    assert!(config.validate().is_err());
}
