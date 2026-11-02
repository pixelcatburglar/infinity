//! Server tracing always writes to stdout and optionally to rotating files, using
//! deployment configuration. File rotation is time-based in UTC, with retention
//! limited by file count per log; `never` rotation does not prune files. Files
//! omit ANSI escapes. A second direct `client-errors` stream records uploads and
//! details independently of the tracing filter; upload traces retain a UTF-8-safe
//! tail of at most 1 MiB, with truncation marked in the header. Both streams may
//! contain sensitive player information and must be kept private and out of source
//! control. Keep both writer guards until shutdown to flush queued records.
//! The database is authoritative; a direct-log write failure only emits a warning.
//! This server-only facility mirrors no client module; size-based rotation is not
//! supported.

use anyhow::{Context, Result};
use std::sync::OnceLock;
use tracing_appender::non_blocking::NonBlocking;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::config::deployment::LoggingConfig;

static CLIENT_ERRORS: OnceLock<NonBlocking> = OnceLock::new();

pub fn client_errors() -> Option<NonBlocking> { CLIENT_ERRORS.get().cloned() }

pub struct Guards {
    _file: Option<tracing_appender::non_blocking::WorkerGuard>,
    _client_errors: Option<tracing_appender::non_blocking::WorkerGuard>,
}

pub fn init(config: &LoggingConfig) -> Result<Guards> {
    config.validate()?;
    let filter = EnvFilter::try_new(&config.filter).context("invalid logging.filter")?;
    let (file_layer, guard, client_guard) = if config.dir.is_empty() {
        (None, None, None)
    } else {
        let rotation = match config.rotation.as_str() {
            "minutely" => Rotation::MINUTELY,
            "hourly" => Rotation::HOURLY,
            "daily" => Rotation::DAILY,
            "never" => Rotation::NEVER,
            other => anyhow::bail!("invalid logging.rotation: {other:?}"),
        };
        std::fs::create_dir_all(&config.dir)
            .with_context(|| format!("creating log directory {}", config.dir))?;
        let appender = |prefix: &str| -> Result<RollingFileAppender> {
            let mut builder = RollingFileAppender::builder()
                .filename_prefix(prefix)
                .filename_suffix("log");
            if rotation != Rotation::NEVER {
                builder = builder.max_log_files(config.keep_files);
            }
            builder.rotation(rotation.clone()).build(&config.dir)
                .with_context(|| format!("initializing {prefix} log file in {}", config.dir))
        };
        let (writer, guard) = tracing_appender::non_blocking(appender("server")?);
        let (client_writer, client_guard) = tracing_appender::non_blocking(appender("client-errors")?);
        CLIENT_ERRORS.set(client_writer).map_err(|_| anyhow::anyhow!("client error logging already initialized"))?;
        (Some(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(writer)), Some(guard), Some(client_guard))
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(file_layer)
        .try_init()
        .map_err(|error| anyhow::anyhow!("initializing tracing subscriber: {error}"))?;
    Ok(Guards { _file: guard, _client_errors: client_guard })
}
