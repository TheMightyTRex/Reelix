//! Serde model of the supported Cardigann-compatible YAML subset.

use std::fmt;
use std::marker::PhantomData;

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};

/// A YAML scalar (string, number or bool) read as a string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scalar(pub String);

impl<'de> Deserialize<'de> for Scalar {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match yaml_serde::Value::deserialize(d)? {
            yaml_serde::Value::String(s) => Ok(Self(s)),
            yaml_serde::Value::Number(n) => Ok(Self(n.to_string())),
            yaml_serde::Value::Bool(b) => Ok(Self(b.to_string())),
            yaml_serde::Value::Null => Ok(Self(String::new())),
            other => Err(de::Error::custom(format!("expected a scalar, got {other:?}"))),
        }
    }
}

/// A mapping that keeps the YAML key order (field evaluation order matters).
#[derive(Debug, Clone, PartialEq)]
pub struct OrderedMap<V>(pub Vec<(String, V)>);

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<V> OrderedMap<V> {
    pub fn get(&self, k: &str) -> Option<&V> {
        self.0.iter().find(|e| e.0 == k).map(|e| &e.1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for OrderedMap<V> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V2<V>(PhantomData<V>);
        impl<'de, V: Deserialize<'de>> Visitor<'de> for V2<V> {
            type Value = OrderedMap<V>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a mapping")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(OrderedMap::default())
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some((k, v)) = m.next_entry::<Scalar, V>()? {
                    out.push((k.0, v));
                }
                Ok(OrderedMap(out))
            }
        }
        d.deserialize_any(V2(PhantomData))
    }
}

/// Root of a definition file.
#[derive(Debug, Clone, Deserialize)]
pub struct Definition {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub language: String,
    /// `public`, `private` or `semi-private`.
    #[serde(default, rename = "type")]
    pub site_type: String,
    #[serde(default = "utf8")]
    pub encoding: String,
    /// Extension: `torrent` (default) or `usenet`.
    #[serde(default)]
    pub protocol: Option<String>,
    pub links: Vec<String>,
    #[serde(default)]
    pub caps: CapsDef,
    #[serde(default)]
    pub settings: Vec<SettingDef>,
    #[serde(default)]
    pub login: Option<LoginDef>,
    pub search: SearchDef,
    #[serde(default)]
    pub download: Option<DownloadDef>,
    /// Seconds to wait between two requests to the site.
    #[serde(default, rename = "requestDelay")]
    pub request_delay: Option<f64>,
    /// Follow redirects for login requests (redirects are always followed by this engine).
    #[serde(default)]
    pub followredirect: bool,
}

/// A scalar or a sequence of scalars (header values are written both ways).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StrList(pub Vec<String>);

impl<'de> Deserialize<'de> for StrList {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match yaml_serde::Value::deserialize(d)? {
            yaml_serde::Value::Sequence(v) => v
                .into_iter()
                .map(|x| Scalar::deserialize(x).map(|s| s.0).map_err(de::Error::custom))
                .collect::<Result<Vec<_>, _>>()
                .map(Self),
            yaml_serde::Value::Null => Ok(Self::default()),
            other => Scalar::deserialize(other)
                .map(|s| Self(vec![s.0]))
                .map_err(de::Error::custom),
        }
    }
}

fn utf8() -> String {
    "UTF-8".into()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CapsDef {
    #[serde(default)]
    pub categorymappings: Vec<CategoryMapping>,
    /// Legacy form: `site id → Newznab name`.
    #[serde(default)]
    pub categories: OrderedMap<Scalar>,
    #[serde(default)]
    pub modes: OrderedMap<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CategoryMapping {
    pub id: Scalar,
    /// Newznab name such as `Movies/HD`.
    pub cat: String,
    #[serde(default)]
    pub desc: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SettingDef {
    pub name: String,
    /// `text`, `password`, `checkbox`, `select`, `multi-select`, `info…`.
    #[serde(rename = "type", default = "text")]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub default: Option<yaml_serde::Value>,
    #[serde(default)]
    pub options: OrderedMap<Scalar>,
}

fn text() -> String {
    "text".into()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LoginDef {
    #[serde(default)]
    pub path: Option<String>,
    /// Submit target for `form` logins when it differs from the form's action.
    #[serde(default)]
    pub submitpath: Option<String>,
    /// `post` (default), `form`, `get` or `cookie`.
    #[serde(default)]
    pub method: Option<String>,
    /// CSS selector of the form for `method: form` (default `form`).
    #[serde(default)]
    pub form: Option<String>,
    #[serde(default)]
    pub inputs: OrderedMap<Scalar>,
    /// `form` logins: input keys are selectors of the form's inputs rather than names.
    #[serde(default)]
    pub selectors: bool,
    /// `form` logins: extra inputs scraped from the login page.
    #[serde(default)]
    pub selectorinputs: OrderedMap<FieldDef>,
    /// `form` logins: extra query parameters scraped from the login page.
    #[serde(default)]
    pub getselectorinputs: OrderedMap<FieldDef>,
    /// Cookies (`name=value`) set before logging in.
    #[serde(default)]
    pub cookies: Vec<String>,
    #[serde(default)]
    pub headers: OrderedMap<StrList>,
    #[serde(default)]
    pub captcha: Option<CaptchaDef>,
    #[serde(default)]
    pub error: Vec<ErrorDef>,
    #[serde(default)]
    pub test: Option<TestDef>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CaptchaDef {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub selector: String,
    #[serde(default)]
    pub input: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErrorDef {
    pub selector: String,
    #[serde(default)]
    pub message: Option<FieldDef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestDef {
    pub path: String,
    #[serde(default)]
    pub selector: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchDef {
    #[serde(default)]
    pub paths: Vec<PathDef>,
    /// Shorthand for a single path.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub inputs: OrderedMap<Scalar>,
    #[serde(default)]
    pub keywordsfilters: Vec<FilterDef>,
    /// Send inputs whose rendered value is empty (dropped by default).
    #[serde(default, rename = "allowEmptyInputs")]
    pub allow_empty_inputs: bool,
    #[serde(default)]
    pub headers: OrderedMap<StrList>,
    /// Filters applied to the raw response body before HTML parsing.
    #[serde(default)]
    pub preprocessingfilters: Vec<FilterDef>,
    #[serde(default)]
    pub error: Vec<ErrorDef>,
    pub rows: RowsDef,
    pub fields: OrderedMap<FieldDef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PathDef {
    pub path: String,
    #[serde(default)]
    pub method: Option<String>,
    /// Only use this path when one of these site categories is searched.
    #[serde(default)]
    pub categories: Vec<Scalar>,
    #[serde(default)]
    pub inputs: OrderedMap<Scalar>,
    #[serde(default = "yes")]
    pub inheritinputs: bool,
    #[serde(default)]
    pub response: Option<ResponseDef>,
    #[serde(default)]
    pub followredirect: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseDef {
    /// `html` (default) or `json`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// A body equal to this (trimmed) means "no results".
    #[serde(default, rename = "noResultsMessage")]
    pub no_results_message: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RowsDef {
    pub selector: String,
    /// Merge the next `after` rows into each row (HTML).
    #[serde(default)]
    pub after: usize,
    /// JSON: each row's fields live under this key.
    #[serde(default)]
    pub attribute: Option<String>,
    /// JSON: the `attribute` value holds several releases sharing the row's (`..`) fields.
    #[serde(default)]
    pub multiple: bool,
    /// A count below 1 means no results.
    #[serde(default)]
    pub count: Option<FieldDef>,
    /// HTML: publish date taken from the nearest preceding header row matching this.
    #[serde(default)]
    pub dateheaders: Option<FieldDef>,
    /// Row filters (`andmatch`).
    #[serde(default)]
    pub filters: Vec<FilterDef>,
    #[serde(default, rename = "missingAttributeEqualsNoResults")]
    pub missing_attribute_equals_no_results: bool,
}

/// A field extraction rule (also used for error messages).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FieldDef {
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub attribute: Option<String>,
    /// Template evaluated instead of a selector.
    #[serde(default)]
    pub text: Option<Scalar>,
    #[serde(default)]
    pub remove: Option<String>,
    #[serde(default)]
    pub filters: Vec<FilterDef>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub default: Option<Scalar>,
    /// Ordered `selector → value` alternatives; `*` always matches.
    #[serde(default)]
    pub case: OrderedMap<Scalar>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FilterDef {
    pub name: String,
    #[serde(default)]
    pub args: Option<yaml_serde::Value>,
}

impl FilterDef {
    /// Arguments normalised to strings (scalar → one arg, sequence → many).
    pub fn args(&self) -> Vec<String> {
        fn s(v: &yaml_serde::Value) -> String {
            match v {
                yaml_serde::Value::String(s) => s.clone(),
                yaml_serde::Value::Number(n) => n.to_string(),
                yaml_serde::Value::Bool(b) => b.to_string(),
                _ => String::new(),
            }
        }
        match &self.args {
            None | Some(yaml_serde::Value::Null) => Vec::new(),
            Some(yaml_serde::Value::Sequence(v)) => v.iter().map(s).collect(),
            Some(v) => vec![s(v)],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DownloadDef {
    /// Tried in order on the details page; the first match yields the real download link.
    #[serde(default)]
    pub selectors: Vec<FieldDef>,
    /// Request sent before downloading (e.g. a "thanks" endpoint).
    #[serde(default)]
    pub before: Option<BeforeDef>,
    /// Build a magnet from a hash (and title) found on the details page.
    #[serde(default)]
    pub infohash: Option<InfohashDef>,
    /// `get` (default) or `post` for the final download request.
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers: OrderedMap<StrList>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BeforeDef {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub inputs: OrderedMap<Scalar>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InfohashDef {
    pub hash: FieldDef,
    #[serde(default)]
    pub title: Option<FieldDef>,
    /// Read the hash from the `before` response instead of fetching the details page.
    #[serde(default)]
    pub usebeforeresponse: bool,
}
