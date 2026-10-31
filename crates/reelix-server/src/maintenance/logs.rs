//! Reads the rolling log files written by [`crate::telemetry`] (pretty or JSON lines).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use reelix_api_types::system::{LogEntryDto, LogFileDto, LogLevel, LogsDto};

use super::backup::is_safe_name;

/// How much of the end of a log file the viewer reads.
const TAIL_BYTES: u64 = 4 * 1024 * 1024;
const DEFAULT_LIMIT: usize = 500;
const MAX_LIMIT: usize = 5000;

fn is_log_name(name: &str) -> bool {
    is_safe_name(name) && name.starts_with("reelix") && name.ends_with(".log")
}

/// Log files, newest first.
pub fn files(dir: &Path) -> std::io::Result<Vec<LogFileDto>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_log_name(&name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        out.push(LogFileDto {
            name,
            size_bytes: meta.len(),
            modified: meta.modified().map(DateTime::<Utc>::from).unwrap_or_default(),
        });
    }
    // Daily files are named `reelix.YYYY-MM-DD.log`, so the name breaks modification-time ties.
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| b.name.cmp(&a.name)));
    Ok(out)
}

/// Path of a log file; `None` for unknown or unsafe names.
pub fn path(dir: &Path, name: &str) -> Option<PathBuf> {
    if !is_log_name(name) {
        return None;
    }
    let p = dir.join(name);
    p.is_file().then_some(p)
}

pub struct Filter {
    pub min_level: LogLevel,
    pub search: Option<String>,
    pub limit: usize,
}

impl Filter {
    pub fn new(level: Option<&str>, search: Option<&str>, limit: Option<usize>) -> Self {
        Self {
            min_level: level.and_then(LogLevel::parse).unwrap_or(LogLevel::Info),
            search: search
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_lowercase),
            limit: limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        }
    }

    fn matches(&self, e: &LogEntryDto) -> bool {
        // Lines without a level (e.g. panics printed by the runtime) are always shown.
        e.level.is_none_or(|l| l >= self.min_level)
            && self.search.as_ref().is_none_or(|s| {
                e.message.to_lowercase().contains(s)
                    || e.target.as_deref().is_some_and(|t| t.to_lowercase().contains(s))
            })
    }
}

/// Reads the end of `file` and returns matching entries, newest first.
pub fn read(file: &Path, filter: &Filter) -> std::io::Result<(Vec<LogEntryDto>, bool)> {
    let mut f = std::fs::File::open(file)?;
    let len = f.metadata()?.len();
    let truncated = len > TAIL_BYTES;
    if truncated {
        f.seek(SeekFrom::Start(len - TAIL_BYTES))?;
    }
    let mut buf = Vec::with_capacity(len.min(TAIL_BYTES) as usize);
    f.read_to_end(&mut buf)?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        // Drop the partial first line.
        text = text
            .split_once('\n')
            .map(|(_, rest)| rest.to_owned())
            .unwrap_or_default();
    }
    let entries = parse(&text);
    let mut out: Vec<_> = entries
        .into_iter()
        .rev()
        .filter(|e| filter.matches(e))
        .take(filter.limit)
        .collect();
    out.shrink_to_fit();
    Ok((out, truncated))
}

pub fn logs(dir: &Path, name: Option<&str>, filter: &Filter) -> Result<LogsDto, ReadError> {
    let file = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => Some((n.to_owned(), path(dir, n).ok_or(ReadError::NotFound)?)),
        None => files(dir)?
            .into_iter()
            .next()
            .map(|f| (f.name.clone(), dir.join(f.name))),
    };
    let Some((name, path)) = file else {
        return Ok(LogsDto {
            file: None,
            entries: Vec::new(),
            truncated: false,
        });
    };
    let (entries, truncated) = read(&path, filter)?;
    Ok(LogsDto {
        file: Some(name),
        entries,
        truncated,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("unknown log file")]
    NotFound,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Parses pretty or JSON log text into entries (oldest first). Continuation lines (multi-line
/// messages, backtraces) are appended to the entry before them.
pub fn parse(text: &str) -> Vec<LogEntryDto> {
    let mut out: Vec<LogEntryDto> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(entry) = parse_json(line).or_else(|| parse_pretty(line)) {
            out.push(entry);
        } else if let Some(last) = out.last_mut() {
            last.message.push('\n');
            last.message.push_str(line);
        } else {
            out.push(LogEntryDto {
                time: None,
                level: None,
                target: None,
                message: line.to_owned(),
            });
        }
    }
    out
}

/// `2026-10-31T02:42:00.697280Z  INFO reelix_server::server: listening on http://…`
fn parse_pretty(line: &str) -> Option<LogEntryDto> {
    let (ts, rest) = line.split_once(' ')?;
    let time = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc);
    let rest = rest.trim_start();
    let (level, rest) = rest.split_once(' ').unwrap_or((rest, ""));
    let level = LogLevel::parse(level)?;
    let rest = rest.trim_start();
    // The target is the first token ending in `:` (spans with fields come before it and contain `{`).
    let (target, message) = match rest.split_once(": ") {
        Some((head, msg)) if !head.contains(' ') && !head.contains('{') => {
            (Some(head.to_owned()), msg.to_owned())
        }
        _ => (None, rest.to_owned()),
    };
    Some(LogEntryDto {
        time: Some(time),
        level: Some(level),
        target,
        message,
    })
}

/// `{"timestamp":"…","level":"INFO","fields":{"message":"…","k":"v"},"target":"…"}`
fn parse_json(line: &str) -> Option<LogEntryDto> {
    if !line.starts_with('{') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let obj = v.as_object()?;
    let time = obj
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc));
    let level = obj
        .get("level")
        .and_then(|l| l.as_str())
        .and_then(LogLevel::parse);
    let target = obj.get("target").and_then(|t| t.as_str()).map(str::to_owned);
    let mut message = String::new();
    if let Some(fields) = obj.get("fields").and_then(|f| f.as_object()) {
        if let Some(m) = fields.get("message") {
            message = m.as_str().map_or_else(|| m.to_string(), str::to_owned);
        }
        for (k, val) in fields.iter().filter(|(k, _)| *k != "message") {
            let val = val.as_str().map_or_else(|| val.to_string(), str::to_owned);
            if !message.is_empty() {
                message.push(' ');
            }
            message.push_str(&format!("{k}={val}"));
        }
    }
    Some(LogEntryDto {
        time,
        level,
        target,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRETTY: &str = "\
2026-10-31T02:42:00.697280Z  INFO reelix_server::server: listening on http://0.0.0.0:7700/
2026-10-31T02:42:01.000000Z DEBUG request{method=GET uri=/api/v1/auth/me}: tower_http::trace: started processing request
2026-10-31T02:42:02.000000Z ERROR reelix_api::error: internal API error cause=boom
caused by: line two
2026-10-31T02:42:03.000000Z  WARN reelix_jobs: job failed kind=\"rss_sync\" attempt=2
";

    #[test]
    fn parses_pretty_lines_with_continuations() {
        let e = parse(PRETTY);
        assert_eq!(e.len(), 4);
        assert_eq!(e[0].level, Some(LogLevel::Info));
        assert_eq!(e[0].target.as_deref(), Some("reelix_server::server"));
        assert_eq!(e[0].message, "listening on http://0.0.0.0:7700/");
        assert_eq!(e[1].level, Some(LogLevel::Debug));
        assert_eq!(e[1].target, None);
        assert!(e[1].message.starts_with("request{method=GET"));
        assert_eq!(e[2].message, "internal API error cause=boom\ncaused by: line two");
        assert_eq!(e[3].level, Some(LogLevel::Warn));
    }

    #[test]
    fn parses_json_lines() {
        let line = r#"{"timestamp":"2026-10-31T02:42:00.697280Z","level":"WARN","fields":{"message":"slow","ms":1500},"target":"reelix_pipeline"}"#;
        let e = parse(line);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].level, Some(LogLevel::Warn));
        assert_eq!(e[0].target.as_deref(), Some("reelix_pipeline"));
        assert_eq!(e[0].message, "slow ms=1500");
        assert!(e[0].time.is_some());
    }

    #[test]
    fn reads_files_newest_first_with_filters() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("reelix.2026-10-31.log"), PRETTY).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        let list = files(dir.path()).unwrap();
        assert_eq!(list.len(), 1);

        let all = logs(dir.path(), None, &Filter::new(Some("trace"), None, None)).unwrap();
        assert_eq!(all.file.as_deref(), Some("reelix.2026-10-31.log"));
        assert_eq!(all.entries.len(), 4);
        assert_eq!(all.entries[0].level, Some(LogLevel::Warn), "newest first");

        let warn = logs(dir.path(), None, &Filter::new(Some("warn"), None, None)).unwrap();
        assert_eq!(warn.entries.len(), 2);
        let found = logs(
            dir.path(),
            None,
            &Filter::new(Some("trace"), Some("RSS_SYNC"), None),
        )
        .unwrap();
        assert_eq!(found.entries.len(), 1);
        let one = logs(dir.path(), None, &Filter::new(Some("trace"), None, Some(1))).unwrap();
        assert_eq!(one.entries.len(), 1);

        assert!(matches!(
            logs(dir.path(), Some("../secret.key"), &Filter::new(None, None, None)),
            Err(ReadError::NotFound)
        ));
        assert!(path(dir.path(), "notes.txt").is_none());
    }
}
