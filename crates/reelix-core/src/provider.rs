//! Self-describing settings schemas for pluggable providers
//! (indexers, download clients, notifiers, media servers, import lists).
//!
//! Each adapter declares a [`ProviderSchema`]; the UI renders forms from it and the
//! persistence layer encrypts every field whose kind is [`FieldKind::Secret`].

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    /// Stored encrypted at rest, never returned in plain text by the API.
    Secret,
    Number,
    Bool,
    Url,
    Path,
    Select {
        options: Vec<SelectOption>,
    },
    MultiSelect {
        options: Vec<SelectOption>,
    },
    /// Free-form list of strings (e.g. categories, required terms).
    List,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: Value,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldSchema {
    pub name: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Hidden behind "show advanced" in the UI.
    #[serde(default)]
    pub advanced: bool,
}

impl FieldSchema {
    pub fn new(name: &str, label: &str, kind: FieldKind) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            kind,
            required: false,
            default: None,
            help: None,
            placeholder: None,
            advanced: false,
        }
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn default_value(mut self, v: impl Into<Value>) -> Self {
        self.default = Some(v.into());
        self
    }

    pub fn help(mut self, h: &str) -> Self {
        self.help = Some(h.into());
        self
    }

    pub fn placeholder(mut self, p: &str) -> Self {
        self.placeholder = Some(p.into());
        self
    }

    pub fn advanced(mut self) -> Self {
        self.advanced = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSchema {
    /// Stable implementation id, e.g. `qbittorrent`, `torznab`, `discord`.
    pub implementation: String,
    /// Display name, e.g. `qBittorrent`.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_url: Option<String>,
    pub fields: Vec<FieldSchema>,
}

impl ProviderSchema {
    /// Fills in defaults and checks required fields; returns the normalised settings object.
    pub fn validate(&self, settings: &Value) -> Result<Map<String, Value>, String> {
        let mut obj = match settings {
            Value::Object(m) => m.clone(),
            Value::Null => Map::new(),
            _ => return Err("settings must be a JSON object".into()),
        };
        for f in &self.fields {
            let missing = obj
                .get(&f.name)
                .is_none_or(|v| v.is_null() || v.as_str().is_some_and(str::is_empty));
            if missing {
                if let Some(d) = &f.default {
                    obj.insert(f.name.clone(), d.clone());
                } else if f.required {
                    return Err(format!("{} is required", f.label));
                }
            }
        }
        Ok(obj)
    }

    /// Names of fields that must be encrypted at rest and masked in API responses.
    pub fn secret_fields(&self) -> impl Iterator<Item = &str> {
        self.fields
            .iter()
            .filter(|f| matches!(f.kind, FieldKind::Secret))
            .map(|f| f.name.as_str())
    }
}

/// Placeholder returned by the API in place of stored secrets; sending it back means "unchanged".
pub const SECRET_MASK: &str = "********";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> ProviderSchema {
        ProviderSchema {
            implementation: "x".into(),
            name: "X".into(),
            description: None,
            info_url: None,
            fields: vec![
                FieldSchema::new("host", "Host", FieldKind::Text).required(),
                FieldSchema::new("port", "Port", FieldKind::Number).default_value(8080),
                FieldSchema::new("api_key", "API key", FieldKind::Secret).required(),
            ],
        }
    }

    #[test]
    fn validate_fills_defaults_and_requires_fields() {
        let s = schema();
        let ok = s.validate(&json!({"host": "h", "api_key": "k"})).unwrap();
        assert_eq!(ok["port"], 8080);
        assert!(s.validate(&json!({"host": "h"})).unwrap_err().contains("API key"));
        assert_eq!(s.secret_fields().collect::<Vec<_>>(), ["api_key"]);
    }

    #[test]
    fn field_kind_serializes_flat() {
        let f = FieldSchema::new("a", "A", FieldKind::Bool);
        assert_eq!(serde_json::to_value(&f).unwrap()["type"], "bool");
    }
}
