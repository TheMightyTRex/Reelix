//! Crate-wide error type.

use std::path::PathBuf;
use std::time::Duration;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The path resolves outside every configured root (incl. via symlinks).
    #[error("path {0} is outside the allowed root folders")]
    OutsideRoots(PathBuf),
    /// Relative components (`..`) or an otherwise unusable path.
    #[error("invalid path {path}: {reason}")]
    InvalidPath { path: PathBuf, reason: &'static str },
    #[error("destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error("hardlink from {src} to {dst} failed: {source}")]
    Hardlink {
        src: PathBuf,
        dst: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("ffprobe could not be started ({path}): {source}")]
    FfprobeSpawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("ffprobe failed (exit {code:?}): {stderr}")]
    FfprobeFailed { code: Option<i32>, stderr: String },
    #[error("ffprobe timed out after {0:?}")]
    FfprobeTimeout(Duration),
    #[error("invalid ffprobe output: {0}")]
    ProbeJson(#[from] serde_json::Error),
    #[error("ffprobe found no usable streams")]
    NoStreams,
    #[error("naming template error: {0}")]
    Template(String),
    #[error("{what} is {len} bytes, longer than the {max}-byte limit")]
    PathTooLong {
        what: &'static str,
        len: usize,
        max: usize,
    },
}

impl Error {
    /// Wrap an `io::Error` with the path it concerns.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
