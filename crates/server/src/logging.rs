//! Server tracing always writes to stdout and optionally to rotating files, using
//! deployment configuration. File rotation is time-based in UTC, with retention
//! limited by file count per log; `never` rotation does not prune files. Files
//! omit ANSI escapes and flush on graceful shutdown.
//!
//! Logs may contain player information and should be kept private and out of
//! source control. This server-only facility mirrors no client module and uses
//! no database tables; size-based rotation and client-error logs are not supported.

use anyhow::{Context, Result};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::config::deployment::LoggingConfig;

pub struct Guards {
    _file: Option<tracing_appender::non_blocking::WorkerGuard>,
}

pub fn init(config: &LoggingConfig) -> Result<Guards> {
    config.validate()?;
    let filter = EnvFilter::try_new(&config.filter).context("invalid logging.filter")?;
    let (file_layer, guard) = if config.dir.is_empty() {
        (None, None)
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
        let mut builder = RollingFileAppender::builder()
            .filename_prefix("server")
            .filename_suffix("log");
        if rotation != Rotation::NEVER {
            builder = builder.max_log_files(config.keep_files);
        }
        let appender = builder
            .rotation(rotation)
            .build(&config.dir)
            .with_context(|| format!("initializing server log file in {}", config.dir))?;
        let (writer, guard) = tracing_appender::non_blocking(appender);
        (Some(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(writer)), Some(guard))
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(file_layer)
        .try_init()
        .map_err(|error| anyhow::anyhow!("initializing tracing subscriber: {error}"))?;
    Ok(Guards { _file: guard })
}
