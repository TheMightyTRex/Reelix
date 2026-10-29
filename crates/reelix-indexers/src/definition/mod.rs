//! YAML definition engine for trackers that don't speak Torznab, compatible with the
//! Cardigann/Prowlarr (schema v11) definition format: HTML and XML pages are matched with a CSS
//! engine that understands jQuery extensions (`:contains`, `:has`, `:root` …), JSON APIs with
//! Cardigann's JSON selectors, and definition regexes fall back to `fancy-regex` for lookaround.
//! Features the engine cannot honour make a definition fail to load with `unsupported: …`
//! (or, for login captchas, fail at login with a clear message).

mod engine;
mod filters;
mod jsonsel;
pub mod model;
mod re;
mod selector;
pub mod template;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema, SelectOption};

pub use engine::DefinitionIndexer;
pub use model::Definition;

use crate::IndexerError;

/// Prefix of the provider implementation id for definition-backed indexers.
pub const IMPLEMENTATION_PREFIX: &str = "definition:";

/// Loaded definitions, keyed by id.
#[derive(Debug, Clone, Default)]
pub struct DefinitionStore {
    defs: HashMap<String, Arc<Definition>>,
}

/// A definition file that failed to load.
#[derive(Debug, Clone)]
pub struct LoadError {
    pub path: PathBuf,
    pub message: String,
}

impl DefinitionStore {
    /// Loads every `*.yml`/`*.yaml` in `dir`. Broken files are reported, not fatal.
    pub fn load_dir(dir: &Path) -> Result<(Self, Vec<LoadError>), IndexerError> {
        let mut store = Self::default();
        let mut errors = Vec::new();
        let rd =
            std::fs::read_dir(dir).map_err(|e| IndexerError::Config(format!("{}: {e}", dir.display())))?;
        let mut paths: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
            .collect();
        paths.sort();
        for path in paths {
            let res = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|s| parse(&s))
                .and_then(|d| store.insert(d));
            if let Err(message) = res {
                tracing::warn!(path = %path.display(), %message, "skipping indexer definition");
                errors.push(LoadError { path, message });
            }
        }
        Ok((store, errors))
    }

    /// Adds one definition from YAML source.
    pub fn add_yaml(&mut self, yaml: &str) -> Result<(), IndexerError> {
        parse(yaml)
            .and_then(|d| self.insert(d))
            .map_err(IndexerError::Config)
    }

    fn insert(&mut self, d: Definition) -> Result<(), String> {
        if self.defs.contains_key(&d.id) {
            return Err(format!("duplicate definition id `{}`", d.id));
        }
        self.defs.insert(d.id.clone(), Arc::new(d));
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<Arc<Definition>> {
        self.defs.get(id).cloned()
    }

    /// All definitions sorted by name.
    pub fn list(&self) -> Vec<Arc<Definition>> {
        let mut v: Vec<_> = self.defs.values().cloned().collect();
        v.sort_by_key(|a| a.name.to_lowercase());
        v
    }

    /// One schema per definition (`definition:<id>`).
    pub fn schemas(&self) -> Vec<ProviderSchema> {
        self.list().iter().map(|d| schema(d)).collect()
    }
}

/// Parses and statically validates a definition.
pub fn parse(yaml: &str) -> Result<Definition, String> {
    let yaml = yaml.trim_start_matches('\u{feff}');
    let d: Definition = yaml_serde::from_str(yaml).map_err(|e| e.to_string())?;
    if d.id.trim().is_empty() || d.id.contains(char::is_whitespace) {
        return Err("id must be a non-empty token".into());
    }
    if d.links.is_empty() {
        return Err("at least one link is required".into());
    }
    for l in &d.links {
        url::Url::parse(l).map_err(|e| format!("bad link `{l}`: {e}"))?;
    }
    if d.search.paths.is_empty() && d.search.path.is_none() {
        return Err("search needs `path` or `paths`".into());
    }
    if d.search.fields.get("title").is_none() {
        return Err("search.fields needs a `title`".into());
    }
    if !d
        .search
        .fields
        .iter()
        .any(|(k, _)| matches!(base_name(k), "download" | "magnet" | "infohash"))
    {
        return Err("search.fields needs `download`, `magnet` or `infohash`".into());
    }
    engine::validate(&d)?;
    Ok(d)
}

/// Field names may carry modifiers (`category|noappend`); this strips them.
pub(crate) fn base_name(k: &str) -> &str {
    k.split('|').next().unwrap_or(k)
}

/// Settings schema for a definition: `base_url` plus its declared settings.
pub fn schema(d: &Definition) -> ProviderSchema {
    let mut fields = vec![
        FieldSchema::new("base_url", "Site URL", FieldKind::Url)
            .default_value(d.links[0].clone())
            .help("Mirror to use; defaults to the first known link"),
    ];
    for s in &d.settings {
        let label = if s.label.is_empty() {
            s.name.as_str()
        } else {
            s.label.as_str()
        };
        let options = || {
            s.options
                .iter()
                .map(|(k, v)| SelectOption {
                    value: k.into(),
                    label: v.0.clone(),
                })
                .collect::<Vec<_>>()
        };
        let kind = match s.kind.as_str() {
            "password" => FieldKind::Secret,
            "checkbox" => FieldKind::Bool,
            "select" => FieldKind::Select { options: options() },
            "multi-select" => FieldKind::MultiSelect { options: options() },
            k if k.starts_with("info") => continue,
            _ => FieldKind::Text,
        };
        let mut f = FieldSchema::new(&s.name, label, kind);
        match s.default.as_ref().map(yaml_to_json) {
            Some(v) if !v.is_null() => f = f.default_value(v),
            _ if matches!(s.kind.as_str(), "text" | "password") && is_credential(&s.name) => f = f.required(),
            _ => {}
        }
        fields.push(f);
    }
    ProviderSchema {
        implementation: format!("{IMPLEMENTATION_PREFIX}{}", d.id),
        name: d.name.clone(),
        description: (!d.description.is_empty()).then(|| d.description.clone()),
        info_url: Some(d.links[0].clone()),
        fields,
    }
}

fn is_credential(name: &str) -> bool {
    matches!(name, "username" | "password" | "cookie" | "apikey" | "passkey")
}

pub(crate) fn yaml_to_json(v: &yaml_serde::Value) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}
