//! Tracing setup: stdout plus a daily rolling file in the log directory.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::LogFormat;

/// Number of daily log files kept on disk.
const MAX_LOG_FILES: usize = 14;

/// Builds the filter: `RUST_LOG` wins, otherwise the configured level.
pub fn env_filter(level: &str) -> EnvFilter {
    EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level))
        .unwrap_or_else(|_| EnvFilter::new("info"))
}

/// Installs the global subscriber. Keep the returned guard alive to flush file logs on exit.
pub fn init(level: &str, format: LogFormat, log_dir: &Path) -> anyhow::Result<WorkerGuard> {
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("reelix")
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(log_dir)?;
    let (file_writer, guard) = tracing_appender::non_blocking(appender);

    let registry = tracing_subscriber::registry().with(env_filter(level));
    match format {
        LogFormat::Pretty => registry
            .with(tracing_subscriber::fmt::layer())
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(file_writer),
            )
            .try_init()?,
        LogFormat::Json => registry
            .with(tracing_subscriber::fmt::layer().json())
            .with(tracing_subscriber::fmt::layer().json().with_writer(file_writer))
            .try_init()?,
    }
    Ok(guard)
}
