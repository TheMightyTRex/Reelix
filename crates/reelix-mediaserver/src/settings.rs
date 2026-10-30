//! Typed access to validated settings.

use reelix_core::provider::ProviderSchema;
use serde_json::{Map, Value};

use crate::MsError;

pub(crate) struct Settings(Map<String, Value>);

impl Settings {
    pub fn new(schema: &ProviderSchema, raw: &Value) -> Result<Self, MsError> {
        schema.validate(raw).map(Self).map_err(MsError::InvalidSettings)
    }

    pub fn opt_str(&self, key: &str) -> Option<String> {
        match self.0.get(key)? {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            _ => None,
        }
    }

    pub fn str(&self, key: &str) -> Result<String, MsError> {
        self.opt_str(key)
            .ok_or_else(|| MsError::InvalidSettings(format!("{key} is required")))
    }

    /// A boolean flag (`true`/`false`, or `"true"`/`"1"` strings); `default` when unset.
    pub fn bool(&self, key: &str, default: bool) -> bool {
        match self.0.get(key) {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => matches!(s.trim(), "true" | "1" | "yes"),
            Some(Value::Number(n)) => n.as_i64().is_some_and(|n| n != 0),
            _ => default,
        }
    }

    /// An absolute http(s) URL with any trailing slash removed.
    pub fn url(&self, key: &str) -> Result<String, MsError> {
        let raw = self.str(key)?;
        let u = url::Url::parse(&raw)
            .map_err(|e| MsError::InvalidSettings(format!("{key}: invalid URL ({e})")))?;
        if !matches!(u.scheme(), "http" | "https") {
            return Err(MsError::InvalidSettings(format!("{key}: must be http or https")));
        }
        Ok(raw.trim_end_matches('/').to_string())
    }

    /// Path mappings `server_prefix=local_prefix` (one per entry).
    pub fn list(&self, key: &str) -> Vec<String> {
        match self.0.get(key) {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            Some(Value::String(s)) => s
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// Maps Reelix-side paths to the server's view (Docker setups often mount differently).
#[derive(Debug, Clone, Default)]
pub(crate) struct PathMap(Vec<(String, String)>);

impl PathMap {
    /// Parses `reelix_prefix=server_prefix` entries.
    pub fn parse(entries: &[String]) -> Result<Self, MsError> {
        entries
            .iter()
            .map(|e| {
                e.split_once('=')
                    .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
                    .filter(|(a, b)| !a.is_empty() && !b.is_empty())
                    .ok_or_else(|| {
                        MsError::InvalidSettings(format!(
                            "path mapping {e:?} must be `reelix_path=server_path`"
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    /// Rewrites a Reelix path into the server's namespace (first matching prefix wins).
    pub fn to_server(&self, path: &str) -> String {
        for (local, remote) in &self.0 {
            let l = local.trim_end_matches(['/', '\\']);
            if let Some(rest) = path.strip_prefix(l)
                && (rest.is_empty() || rest.starts_with(['/', '\\']))
            {
                return format!("{}{rest}", remote.trim_end_matches(['/', '\\']));
            }
        }
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_map_rewrites_prefixes_on_boundaries() {
        let m = PathMap::parse(&["/data/movies=/media/movies".into()]).unwrap();
        assert_eq!(
            m.to_server("/data/movies/Dune (2021)"),
            "/media/movies/Dune (2021)"
        );
        assert_eq!(m.to_server("/data/movies"), "/media/movies");
        assert_eq!(m.to_server("/data/movies2/x"), "/data/movies2/x");
        assert!(PathMap::parse(&["nope".into()]).is_err());
    }
}
