//! Persisted provider rows and related records.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use reelix_api_types::providers::ProviderKind;
use serde_json::{Map, Value};

/// A provider instance as stored (settings decrypted, never masked).
#[derive(Clone, PartialEq)]
pub struct ProviderRow {
    pub id: i64,
    pub kind: ProviderKind,
    pub implementation: String,
    pub name: String,
    pub enabled: bool,
    pub priority: i32,
    pub tags: Vec<i64>,
    /// Plain-text settings, including secrets. Never log or return these directly.
    pub settings: Map<String, Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for ProviderRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRow")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("implementation", &self.implementation)
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("priority", &self.priority)
            .field("tags", &self.tags)
            .finish_non_exhaustive()
    }
}

impl ProviderRow {
    pub fn setting_str(&self, key: &str) -> Option<&str> {
        self.settings.get(key).and_then(Value::as_str)
    }

    pub fn setting_bool(&self, key: &str, default: bool) -> bool {
        self.settings.get(key).and_then(Value::as_bool).unwrap_or(default)
    }

    pub fn setting_u64(&self, key: &str) -> Option<u64> {
        match self.settings.get(key)? {
            Value::Number(n) => n
                .as_u64()
                .or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub id: i64,
    pub label: String,
}

/// A download client's path → local path mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePathMapping {
    pub id: i64,
    pub host: String,
    pub remote_path: String,
    pub local_path: String,
}

fn norm(p: &str) -> String {
    let s = p.replace('\\', "/");
    let s = s.trim_end_matches('/');
    format!("{s}/")
}

/// Applies the longest matching mapping for `host`; `None` when nothing matches.
pub fn apply_mappings(mappings: &[RemotePathMapping], host: &str, remote: &str) -> Option<PathBuf> {
    let target = norm(remote);
    let windows = remote.contains('\\') || remote.as_bytes().get(1) == Some(&b':');
    mappings
        .iter()
        .filter(|m| m.host.eq_ignore_ascii_case(host.trim()))
        .filter_map(|m| {
            let prefix = norm(&m.remote_path);
            let matches = if windows {
                target
                    .to_ascii_lowercase()
                    .starts_with(&prefix.to_ascii_lowercase())
            } else {
                target.starts_with(&prefix)
            };
            matches.then_some((prefix.len(), m))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(len, m)| {
            let rest = &target[len.min(target.len())..];
            let mut out = PathBuf::from(&m.local_path);
            for part in rest.split('/').filter(|p| !p.is_empty()) {
                out.push(part);
            }
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: i64, host: &str, remote: &str, local: &str) -> RemotePathMapping {
        RemotePathMapping {
            id,
            host: host.into(),
            remote_path: remote.into(),
            local_path: local.into(),
        }
    }

    #[test]
    fn longest_prefix_wins_and_host_must_match() {
        let maps = vec![
            m(1, "nas", "/downloads/", "/mnt/dl"),
            m(2, "nas", "/downloads/complete", "/mnt/complete"),
            m(3, "other", "/", "/x"),
        ];
        assert_eq!(
            apply_mappings(&maps, "NAS", "/downloads/complete/movie/a.mkv"),
            Some(PathBuf::from("/mnt/complete/movie/a.mkv"))
        );
        assert_eq!(
            apply_mappings(&maps, "nas", "/downloads/tv"),
            Some(PathBuf::from("/mnt/dl/tv"))
        );
        assert_eq!(apply_mappings(&maps, "nas", "/downloadsX/tv"), None);
        assert_eq!(apply_mappings(&maps, "unknown", "/downloads"), None);
        assert_eq!(
            apply_mappings(&maps, "nas", "/downloads"),
            Some(PathBuf::from("/mnt/dl"))
        );
    }

    #[test]
    fn windows_remote_paths() {
        let maps = vec![m(1, "win", "D:\\Downloads\\", "/data/dl")];
        assert_eq!(
            apply_mappings(&maps, "win", "d:\\downloads\\Movie\\x.mkv"),
            Some(PathBuf::from("/data/dl/Movie/x.mkv"))
        );
    }
}
