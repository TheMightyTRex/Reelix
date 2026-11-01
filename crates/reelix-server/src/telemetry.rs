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

/// Flushes buffered console and file logs when dropped.
pub struct LogGuards {
    _stdout: WorkerGuard,
    _file: WorkerGuard,
}

/// Installs the global subscriber. Keep the returned guards alive to flush logs on exit.
///
/// Both outputs go through a background writer that drops lines rather than blocking when it
/// falls behind. Writing to stdout directly would freeze every thread that logs whenever the
/// console stops reading, such as a Windows console with text selected (QuickEdit mode), a
/// paused terminal or a stalled log pipe.
pub fn init(level: &str, format: LogFormat, log_dir: &Path) -> anyhow::Result<LogGuards> {
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("reelix")
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(log_dir)?;
    let (file_writer, file_guard) = tracing_appender::non_blocking(appender);
    let (stdout_writer, stdout_guard) = tracing_appender::non_blocking(std::io::stdout());

    let registry = tracing_subscriber::registry().with(env_filter(level));
    match format {
        LogFormat::Pretty => registry
            .with(tracing_subscriber::fmt::layer().with_writer(stdout_writer))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(file_writer),
            )
            .try_init()?,
        LogFormat::Json => registry
            .with(tracing_subscriber::fmt::layer().json().with_writer(stdout_writer))
            .with(tracing_subscriber::fmt::layer().json().with_writer(file_writer))
            .try_init()?,
    }
    Ok(LogGuards {
        _stdout: stdout_guard,
        _file: file_guard,
    })
}
