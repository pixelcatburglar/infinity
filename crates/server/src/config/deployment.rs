//! Deployment settings and their defaults, including rotating logs, the client
//! socket write bound and client telemetry mode. Connection credentials are never
//! formatted in errors. These settings do not mirror client modules or use
//! tables; logging does not support size-based rotation.

use std::time::Duration;

use anyhow::{bail, Result};
use serde::Deserialize;

/// Bytes in a mebibyte, used to bound image uploads.
const BYTES_PER_MIB: usize = 1024 * 1024;

use infinity_wire::conn::DEFAULT_CLIENT_WRITE_TIMEOUT_SECS;
/// Server policy: admission time bounds never exceed one day.
const MAX_ADMISSION_DURATION_SECS: u64 = 86_400;
/// Server policy: counted credential failures before a cooldown are few.
const MAX_LOGIN_FAILURE_THRESHOLD: usize = 100;

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Server listener address.
    pub bind: String,
    /// Address clients use to reach chat, images and the CDN.
    pub public_host: String,
    /// Node ID the client sees as its proxy.
    pub node_id: i64,
    /// Pilots per solar system before gate jumps queue; zero is unlimited.
    pub system_capacity: usize,
    /// Server policy: a new login replaces the existing session.
    pub login_takeover: bool,
    /// Server policy: bound the position flush at shutdown.
    pub shutdown_flush_timeout_secs: u64,
    /// Server policy: bound on one client or chat socket write.
    pub client_write_timeout_secs: u64,
    /// Server policy: one game handshake's total bound from accept to reply.
    pub handshake_timeout_secs: u64,
    /// Server policy: simultaneous handshakes in progress.
    pub max_pending_handshakes: usize,
    /// Server policy: simultaneous handshakes from one address.
    pub max_pending_handshakes_per_address: usize,
    /// Server policy: counted credential failures inside the window before a cooldown.
    pub login_failure_threshold: usize,
    /// Server policy: sliding window for counted credential failures.
    pub login_failure_window_secs: u64,
    /// Server policy: refusal period once a source saturates its window.
    pub login_failure_cooldown_secs: u64,
    /// Server policy: logins are refused this long before a planned shutdown.
    pub shutdown_reject_secs: u64,
    /// Infinity build 3653293 handshake macho version.
    pub macho_version: i64,
    /// Infinity build 3653293 handshake boot version.
    pub boot_version: f64,
    /// Infinity build 3653293 handshake build number.
    pub boot_build: i64,
    /// Infinity build 3653293 handshake codename.
    pub boot_codename: String,
    /// Infinity build 3653293 handshake region.
    pub boot_region: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:26000".into(), public_host: "127.0.0.1".into(), node_id: 888444,
            system_capacity: 0, login_takeover: true, shutdown_flush_timeout_secs: 10,
            client_write_timeout_secs: DEFAULT_CLIENT_WRITE_TIMEOUT_SECS,
            handshake_timeout_secs: 30, max_pending_handshakes: 256,
            max_pending_handshakes_per_address: 16, login_failure_threshold: 10,
            login_failure_window_secs: 60, login_failure_cooldown_secs: 60,
            shutdown_reject_secs: 120,
            macho_version: 496, boot_version: 24.01, boot_build: 3653293,
            boot_codename: "EVE-RELEASE".into(), boot_region: "optic".into(),
        }
    }
}

impl ServerConfig {
    pub fn validate(&self) -> Result<()> {
        if self.node_id <= 0 { bail!("server.node_id must be positive: {}", self.node_id); }
        if self.shutdown_flush_timeout_secs == 0 { bail!("server.shutdown_flush_timeout_secs must be positive: {}", self.shutdown_flush_timeout_secs); }
        if self.client_write_timeout_secs == 0 { bail!("server.client_write_timeout_secs must be positive: {}", self.client_write_timeout_secs); }
        if !(1..=MAX_ADMISSION_DURATION_SECS).contains(&self.handshake_timeout_secs) || std::time::Instant::now().checked_add(Duration::from_secs(self.handshake_timeout_secs)).is_none() { bail!("server.handshake_timeout_secs must fit 1..={MAX_ADMISSION_DURATION_SECS} and a deadline: {}", self.handshake_timeout_secs); }
        if self.max_pending_handshakes == 0 { bail!("server.max_pending_handshakes must be positive: {}", self.max_pending_handshakes); }
        if self.max_pending_handshakes_per_address == 0 { bail!("server.max_pending_handshakes_per_address must be positive: {}", self.max_pending_handshakes_per_address); }
        if !(1..=MAX_LOGIN_FAILURE_THRESHOLD).contains(&self.login_failure_threshold) { bail!("server.login_failure_threshold must be in 1..={MAX_LOGIN_FAILURE_THRESHOLD}: {}", self.login_failure_threshold); }
        for (key, value) in [("server.login_failure_window_secs", self.login_failure_window_secs), ("server.login_failure_cooldown_secs", self.login_failure_cooldown_secs), ("server.shutdown_reject_secs", self.shutdown_reject_secs)] {
            if !(1..=MAX_ADMISSION_DURATION_SECS).contains(&value) { bail!("{key} must be in 1..={MAX_ADMISSION_DURATION_SECS}: {value}"); }
        }
        if self.macho_version <= 0 { bail!("server.macho_version must be positive: {}", self.macho_version); }
        if !self.boot_version.is_finite() || self.boot_version <= 0.0 { bail!("server.boot_version must be finite and positive: {}", self.boot_version); }
        if self.boot_build <= 0 { bail!("server.boot_build must be positive: {}", self.boot_build); }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Libpq connection string; when nonempty it takes precedence over individual fields.
    pub url: String,
    /// PostgreSQL host, used when url is empty.
    pub host: String,
    /// PostgreSQL port, used when url is empty.
    pub port: u16,
    /// PostgreSQL user, used when url is empty.
    pub user: String,
    /// PostgreSQL password, used when url is empty; prefer the environment.
    pub password: String,
    /// PostgreSQL database name, used when url is empty.
    pub name: String,
    /// Application name shown in pg_stat_activity.
    pub application_name: String,
    /// Server policy: leave capacity for administrators under PostgreSQL's default limit.
    pub pool_size: usize,
    /// Per-connection timeout in seconds.
    pub connect_timeout_secs: u64,
    /// Pool checkout timeout in seconds; zero waits forever.
    pub checkout_timeout_secs: u64,
    /// Initial and maximum startup retry delays in seconds.
    pub startup_retry_secs: [u64; 2],
    /// Apply the idempotent game schema at startup.
    pub schema_on_start: bool,
    /// Refuse to apply a game schema whose hash was recorded and later superseded.
    pub schema_downgrade_guard: bool,
    /// Permit loss-tolerant transactions to commit without a WAL flush wait.
    pub loss_tolerant_async_commit: bool,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: String::new(), host: "127.0.0.1".into(), port: 5432, user: "infinity".into(),
            password: String::new(), name: "infinity".into(), application_name: "infinity-server".into(),
            pool_size: 16, connect_timeout_secs: 5, checkout_timeout_secs: 30,
            startup_retry_secs: [1, 16], schema_on_start: true, schema_downgrade_guard: true,
            loss_tolerant_async_commit: true,
        }
    }
}

impl DatabaseConfig {
    pub fn validate(&self) -> Result<()> {
        if self.url.is_empty() {
            if self.host.is_empty() { bail!("database.host must not be empty: {:?}", self.host); }
            if self.user.is_empty() { bail!("database.user must not be empty: {:?}", self.user); }
            if self.name.is_empty() { bail!("database.name must not be empty: {:?}", self.name); }
        }
        if self.port == 0 { bail!("database.port must be positive: {}", self.port); }
        if self.pool_size == 0 { bail!("database.pool_size must be positive: {}", self.pool_size); }
        if self.connect_timeout_secs == 0 { bail!("database.connect_timeout_secs must be positive: {}", self.connect_timeout_secs); }
        let [first, max] = self.startup_retry_secs;
        if first == 0 || max == 0 || first > max {
            bail!("database.startup_retry_secs must be positive and ordered: [{first}, {max}]");
        }
        self.to_pg_config()?;
        Ok(())
    }

    pub fn to_pg_config(&self) -> Result<tokio_postgres::Config> {
        let mut config = if self.url.is_empty() {
            let mut config = tokio_postgres::Config::new();
            config.host(&self.host).port(self.port).user(&self.user).password(&self.password).dbname(&self.name);
            config
        } else {
            self.url.parse::<tokio_postgres::Config>()
                .map_err(|_| anyhow::anyhow!("invalid database.url connection string"))?
        };
        config.application_name(&self.application_name).connect_timeout(Duration::from_secs(self.connect_timeout_secs));
        Ok(config)
    }
}

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DevelopmentConfig {
    /// Server policy: register unknown names at first login.
    pub auto_register_accounts: bool,
    /// Role for auto-registered accounts: player or gm.
    pub auto_register_role: String,
}

impl Default for DevelopmentConfig {
    fn default() -> Self { Self { auto_register_accounts: true, auto_register_role: "player".into() } }
}

impl DevelopmentConfig {
    pub fn validate(&self) -> Result<()> {
        if !matches!(self.auto_register_role.as_str(), "player" | "gm") {
            bail!("development.auto_register_role must be player or gm: {:?}", self.auto_register_role);
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// Existing tracing filter fallback; RUST_LOG overrides it.
    pub filter: String,
    /// Empty disables file logging; LOG_DIR overrides it.
    pub dir: String,
    /// Time-based rotation interval.
    pub rotation: String,
    /// Maximum number of retained server log files.
    pub keep_files: usize,
}
impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            filter: "info,infinity_server=debug".into(),
            dir: String::new(),
            rotation: "daily".into(),
            keep_files: 14,
        }
    }
}
impl LoggingConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            matches!(self.rotation.as_str(), "minutely" | "hourly" | "daily" | "never"),
            "logging.rotation must be minutely, hourly, daily or never: {:?}", self.rotation
        );
        anyhow::ensure!(self.keep_files >= 1, "logging.keep_files must be at least 1: {}", self.keep_files);
        Ok(())
    }
}

/// Default logging mode assigned to newly observed client errors.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetryConfig {
    pub client_error_log_mode: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self { Self { client_error_log_mode: "B".into() } }
}

impl TelemetryConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(matches!(self.client_error_log_mode.as_str(), "B" | "D"),
            "telemetry.client_error_log_mode must be B or D");
        Ok(())
    }
}

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChatConfig {
    /// XMPP listener address.
    pub bind: String,
    /// XMPP domain: users are <charID>@domain, rooms <name>@conference.domain.
    pub domain: String,
    /// The client drops chat lines when its banned-words filter is off.
    pub banned_words: bool,
}
impl Default for ChatConfig {
    fn default() -> Self { Self { bind: "0.0.0.0:5222".into(), domain: "infinity".into(), banned_words: true } }
}
impl ChatConfig { pub fn validate(&self) -> Result<()> { Ok(()) } }

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImagesConfig {
    /// Image server listener address.
    pub bind: String,
    /// Public image server port.
    pub port: u16,
    /// Directory containing persisted images.
    pub dir: String,
    /// Source for missing images; empty disables fetching.
    pub upstream: String,
    /// Maximum uploaded image size in MiB.
    pub upload_limit_mib: usize,
    /// Timeout for fetching an upstream image.
    pub upstream_timeout_secs: u64,
}
impl Default for ImagesConfig {
    fn default() -> Self {
        Self { bind: "0.0.0.0:26001".into(), port: 26001, dir: "/data/images".into(),
            upstream: "https://images.evetech.net".into(), upload_limit_mib: 8, upstream_timeout_secs: 10 }
    }
}
impl ImagesConfig {
    pub fn validate(&self) -> Result<()> {
        if self.port == 0 { bail!("images.port must be positive: {}", self.port); }
        if self.upload_limit_mib == 0 || self.upload_limit_mib.checked_mul(BYTES_PER_MIB).is_none() {
            bail!("images.upload_limit_mib must be positive and fit in bytes: {}", self.upload_limit_mib);
        }
        if self.upstream_timeout_secs == 0 { bail!("images.upstream_timeout_secs must be positive: {}", self.upstream_timeout_secs); }
        Ok(())
    }
    /// Convert a validated upload limit to bytes.
    pub fn upload_limit_bytes(&self) -> usize {
        self.upload_limit_mib.checked_mul(BYTES_PER_MIB).expect("images.upload_limit_mib must be validated")
    }
}

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewayConfig {
    /// Gateway listener address.
    pub bind: String,
    /// Maximum queued gateway notices per stream.
    pub notice_queue_capacity: usize,
}
impl Default for GatewayConfig {
    fn default() -> Self { Self { bind: "0.0.0.0:26002".into(), notice_queue_capacity: 256 } }
}
impl GatewayConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.notice_queue_capacity > 0 && self.notice_queue_capacity <= tokio::sync::Semaphore::MAX_PERMITS, "gateway.notice_queue_capacity is out of range");
        Ok(())
    }
}

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CdnConfig {
    /// Content CDN listener; the client always fetches plain HTTP on port 80.
    pub bind: String,
}
impl Default for CdnConfig {
    fn default() -> Self { Self { bind: "0.0.0.0:80".into() } }
}
impl CdnConfig { pub fn validate(&self) -> Result<()> { Ok(()) } }

#[cfg(all(test, feature = "tests-runtime"))]
#[path = "deployment/tests.rs"]
mod tests;
