//! Import/export of custom formats in the TRaSH-guides / Recyclarr JSON shape
//! (which is also what Radarr/Sonarr's "export" produces):
//!
//! ```json
//! { "trash_id": "…", "trash_scores": { "default": 1750 }, "name": "DV",
//!   "includeCustomFormatWhenRenaming": false,
//!   "specifications": [ { "name": "DV", "implementation": "ReleaseTitleSpecification",
//!                         "negate": false, "required": true, "fields": { "value": "\\bDV\\b" } } ] }
//! ```
//!
//! `fields` may be an object (TRaSH) or an array of `{name, value}` (Radarr/Sonarr API).
//! Enum-valued fields use the Radarr numbering (source, resolution, modifier, language, flags).

use reelix_parser::{Language, Resolution, Source};
use serde_json::{Map, Value, json};

use crate::DecisionError;
use crate::format::{
    Condition, CustomFormat, FormatSpecification, IndexerFlag, Pattern, QualityModifier, ReleaseType,
};

/// A custom format read from TRaSH/Radarr JSON, with its TRaSH metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedFormat {
    pub format: CustomFormat,
    pub trash_id: Option<String>,
    /// `trash_scores.default`, a suggested profile score.
    pub default_score: Option<i32>,
}

/// Parses one format object or an array of them.
pub fn import_json(text: &str) -> Result<Vec<ImportedFormat>, DecisionError> {
    let v: Value = serde_json::from_str(text).map_err(|e| DecisionError::InvalidJson(e.to_string()))?;
    import_value(&v)
}

/// Like [`import_json`] for an already-parsed value.
pub fn import_value(v: &Value) -> Result<Vec<ImportedFormat>, DecisionError> {
    match v {
        Value::Array(items) => items.iter().map(import_one).collect(),
        Value::Object(_) => Ok(vec![import_one(v)?]),
        _ => Err(DecisionError::InvalidJson(
            "expected an object or an array".into(),
        )),
    }
}

fn bad(msg: impl Into<String>) -> DecisionError {
    DecisionError::InvalidJson(msg.into())
}

fn import_one(v: &Value) -> Result<ImportedFormat, DecisionError> {
    let obj = v.as_object().ok_or_else(|| bad("format must be an object"))?;
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| bad("format \"name\" is required"))?;
    let specs = obj
        .get("specifications")
        .and_then(Value::as_array)
        .ok_or_else(|| bad(format!("{name}: \"specifications\" must be an array")))?;
    let specifications = specs.iter().map(import_spec).collect::<Result<Vec<_>, _>>()?;
    let format = CustomFormat {
        id: 0,
        name: name.to_string(),
        include_in_rename: obj
            .get("includeCustomFormatWhenRenaming")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        specifications,
    };
    format.validate()?;
    Ok(ImportedFormat {
        format,
        trash_id: obj.get("trash_id").and_then(Value::as_str).map(str::to_string),
        default_score: obj
            .get("trash_scores")
            .and_then(|s| s.get("default"))
            .and_then(Value::as_i64)
            .map(|s| s as i32),
    })
}

/// Normalises `fields` (object or `[{name, value}]`) into a map.
fn fields(spec: &Map<String, Value>) -> Map<String, Value> {
    match spec.get("fields") {
        Some(Value::Object(m)) => m.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|f| Some((f.get("name")?.as_str()?.to_string(), f.get("value")?.clone())))
            .collect(),
        _ => Map::new(),
    }
}

fn import_spec(v: &Value) -> Result<FormatSpecification, DecisionError> {
    let spec = v
        .as_object()
        .ok_or_else(|| bad("specification must be an object"))?;
    let implementation = spec
        .get("implementation")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("specification \"implementation\" is required"))?;
    let f = fields(spec);
    let int = |key: &str| -> Result<i64, DecisionError> {
        f.get(key)
            .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
            .ok_or_else(|| bad(format!("{implementation}: field {key:?} must be a number")))
    };
    let num = |key: &str| -> Result<f64, DecisionError> {
        f.get(key)
            .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
            .ok_or_else(|| bad(format!("{implementation}: field {key:?} must be a number")))
    };
    let text = |key: &str| -> Result<Pattern, DecisionError> {
        f.get(key)
            .and_then(Value::as_str)
            .map(Pattern::new)
            .ok_or_else(|| bad(format!("{implementation}: field {key:?} must be a string")))
    };
    let unknown = |what: &str, n: i64| bad(format!("{implementation}: unknown {what} value {n}"));
    let condition = match implementation {
        "ReleaseTitleSpecification" => Condition::ReleaseTitle {
            value: text("value")?,
        },
        "ReleaseGroupSpecification" => Condition::ReleaseGroup {
            value: text("value")?,
        },
        "EditionSpecification" => Condition::Edition {
            value: text("value")?,
        },
        "SourceSpecification" => {
            let n = int("value")?;
            Condition::Source {
                value: source_from(n).ok_or_else(|| unknown("source", n))?,
            }
        }
        "ResolutionSpecification" => {
            let n = int("value")?;
            Condition::Resolution {
                value: resolution_from(n).ok_or_else(|| unknown("resolution", n))?,
            }
        }
        "QualityModifierSpecification" => {
            let n = int("value")?;
            Condition::QualityModifier {
                value: modifier_from(n).ok_or_else(|| unknown("modifier", n))?,
            }
        }
        "LanguageSpecification" => {
            let n = int("value")?;
            let value = match n {
                -1 => None,
                -2 => Some(Language::Original),
                n => Some(
                    u16::try_from(n)
                        .ok()
                        .and_then(Language::from_id)
                        .ok_or_else(|| unknown("language", n))?,
                ),
            };
            let except = f.get("exceptLanguage").and_then(Value::as_bool).unwrap_or(false);
            Condition::Language { value, except }
        }
        "IndexerFlagSpecification" => {
            let n = int("value")?;
            Condition::IndexerFlag {
                value: flag_from(n).ok_or_else(|| unknown("indexer flag", n))?,
            }
        }
        "SizeSpecification" => Condition::Size {
            min_gb: num("min")?,
            max_gb: num("max")?,
        },
        "ReleaseTypeSpecification" => {
            let n = int("value")?;
            Condition::ReleaseType {
                value: match n {
                    1 => ReleaseType::SingleEpisode,
                    2 => ReleaseType::MultiEpisode,
                    3 => ReleaseType::SeasonPack,
                    n => return Err(unknown("release type", n)),
                },
            }
        }
        "YearSpecification" => Condition::Year {
            min: int("min")?.clamp(0, u16::MAX as i64) as u16,
            max: int("max")?.clamp(0, u16::MAX as i64) as u16,
        },
        other => return Err(DecisionError::UnsupportedImplementation(other.to_string())),
    };
    Ok(FormatSpecification {
        name: spec
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(implementation)
            .to_string(),
        negate: spec.get("negate").and_then(Value::as_bool).unwrap_or(false),
        required: spec.get("required").and_then(Value::as_bool).unwrap_or(false),
        condition,
    })
}

const SOURCES: &[(i64, Source)] = &[
    (0, Source::Unknown),
    (1, Source::Cam),
    (2, Source::Telesync),
    (3, Source::Telecine),
    (4, Source::Workprint),
    (5, Source::Dvd),
    (6, Source::Tv),
    (7, Source::WebDl),
    (8, Source::WebRip),
    (9, Source::Bluray),
];
const MODIFIERS: &[(i64, QualityModifier)] = &[
    (0, QualityModifier::None),
    (1, QualityModifier::Regional),
    (2, QualityModifier::Screener),
    (3, QualityModifier::RawHd),
    (4, QualityModifier::BrDisk),
    (5, QualityModifier::Remux),
];
const FLAGS: &[(i64, IndexerFlag)] = &[
    (1, IndexerFlag::Freeleech),
    (2, IndexerFlag::Halfleech),
    (4, IndexerFlag::DoubleUpload),
    (8, IndexerFlag::Golden),
    (16, IndexerFlag::Approved),
    (32, IndexerFlag::Internal),
    (128, IndexerFlag::Scene),
    (256, IndexerFlag::Freeleech75),
    (512, IndexerFlag::Freeleech25),
    (2048, IndexerFlag::Nuked),
];

fn code<T: PartialEq + Copy>(table: &[(i64, T)], v: T) -> i64 {
    table.iter().find(|(_, t)| *t == v).map_or(0, |(k, _)| *k)
}
fn source_from(n: i64) -> Option<Source> {
    SOURCES.iter().find(|(k, _)| *k == n).map(|(_, s)| *s)
}
fn modifier_from(n: i64) -> Option<QualityModifier> {
    MODIFIERS.iter().find(|(k, _)| *k == n).map(|(_, s)| *s)
}
fn flag_from(n: i64) -> Option<IndexerFlag> {
    FLAGS.iter().find(|(k, _)| *k == n).map(|(_, s)| *s)
}
fn resolution_from(n: i64) -> Option<Resolution> {
    Some(match n {
        0 => Resolution::Unknown,
        360 => Resolution::R360p,
        480 => Resolution::R480p,
        540 => Resolution::R540p,
        576 => Resolution::R576p,
        720 => Resolution::R720p,
        1080 => Resolution::R1080p,
        2160 => Resolution::R2160p,
        _ => return None,
    })
}

/// Exports a format in the TRaSH JSON shape (`fields` as an object).
/// `default_score` is written as `trash_scores.default` when given.
pub fn export_json(format: &CustomFormat, default_score: Option<i32>) -> Value {
    let specs: Vec<Value> = format.specifications.iter().map(export_spec).collect();
    let mut out = json!({
        "name": format.name,
        "includeCustomFormatWhenRenaming": format.include_in_rename,
        "specifications": specs,
    });
    if let (Some(score), Some(obj)) = (default_score, out.as_object_mut()) {
        obj.insert("trash_scores".into(), json!({ "default": score }));
    }
    out
}

fn export_spec(s: &FormatSpecification) -> Value {
    let (implementation, fields) = match &s.condition {
        Condition::ReleaseTitle { value } => {
            ("ReleaseTitleSpecification", json!({ "value": value.as_str() }))
        }
        Condition::ReleaseGroup { value } => {
            ("ReleaseGroupSpecification", json!({ "value": value.as_str() }))
        }
        Condition::Edition { value } => ("EditionSpecification", json!({ "value": value.as_str() })),
        Condition::Source { value } => {
            // Remux/BR-DISK are modifiers in the Radarr model; export them as Bluray.
            let v = match value {
                Source::Remux | Source::BrDisk => Source::Bluray,
                v => *v,
            };
            ("SourceSpecification", json!({ "value": code(SOURCES, v) }))
        }
        Condition::Resolution { value } => ("ResolutionSpecification", json!({ "value": value.height() })),
        Condition::QualityModifier { value } => (
            "QualityModifierSpecification",
            json!({ "value": code(MODIFIERS, *value) }),
        ),
        Condition::Language { value, except } => {
            let id: i64 = match value {
                None => -1,
                Some(Language::Original) => -2,
                Some(l) => l.id() as i64,
            };
            (
                "LanguageSpecification",
                json!({ "value": id, "exceptLanguage": except }),
            )
        }
        Condition::IndexerFlag { value } => (
            "IndexerFlagSpecification",
            json!({ "value": code(FLAGS, *value) }),
        ),
        Condition::Size { min_gb, max_gb } => ("SizeSpecification", json!({ "min": min_gb, "max": max_gb })),
        Condition::ReleaseType { value } => {
            let n = match value {
                ReleaseType::SingleEpisode => 1,
                ReleaseType::MultiEpisode => 2,
                ReleaseType::SeasonPack => 3,
            };
            ("ReleaseTypeSpecification", json!({ "value": n }))
        }
        Condition::Year { min, max } => ("YearSpecification", json!({ "min": min, "max": max })),
    };
    json!({
        "name": s.name,
        "implementation": implementation,
        "negate": s.negate,
        "required": s.required,
        "fields": fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "trash_id": "0123456789abcdef0123456789abcdef",
        "trash_scores": { "default": 1500 },
        "name": "HDR10+ Boost",
        "includeCustomFormatWhenRenaming": true,
        "specifications": [
            { "name": "HDR10+", "implementation": "ReleaseTitleSpecification", "negate": false, "required": true,
              "fields": { "value": "\\bHDR10(\\+|P(lus)?\\b)" } },
            { "name": "Not SDR", "implementation": "ReleaseTitleSpecification", "negate": true, "required": true,
              "fields": { "value": "\\bSDR\\b" } },
            { "name": "2160p", "implementation": "ResolutionSpecification", "negate": false, "required": false,
              "fields": [ { "name": "value", "value": 2160 } ] },
            { "name": "Not French", "implementation": "LanguageSpecification", "negate": true, "required": false,
              "fields": { "value": 2, "exceptLanguage": false } },
            { "name": "Size", "implementation": "SizeSpecification", "negate": false, "required": false,
              "fields": { "min": 1, "max": 100 } }
        ]
    }"#;

    #[test]
    fn import_export_roundtrip() {
        let imported = import_json(SAMPLE).unwrap();
        assert_eq!(imported.len(), 1);
        let f = &imported[0];
        assert_eq!(f.default_score, Some(1500));
        assert_eq!(f.trash_id.as_deref(), Some("0123456789abcdef0123456789abcdef"));
        assert!(f.format.include_in_rename);
        assert_eq!(f.format.specifications.len(), 5);
        assert_eq!(
            f.format.specifications[2].condition,
            Condition::Resolution {
                value: Resolution::R2160p
            }
        );

        let exported = export_json(&f.format, f.default_score);
        let back = import_value(&exported).unwrap();
        assert_eq!(back[0].format, f.format);
        assert_eq!(back[0].default_score, Some(1500));
    }

    #[test]
    fn import_errors() {
        assert!(matches!(import_json("nope"), Err(DecisionError::InvalidJson(_))));
        assert!(matches!(
            import_json(r#"{"name":"x"}"#),
            Err(DecisionError::InvalidJson(_))
        ));
        let unsupported =
            r#"{"name":"x","specifications":[{"implementation":"WhateverSpecification","fields":{}}]}"#;
        assert!(matches!(
            import_json(unsupported),
            Err(DecisionError::UnsupportedImplementation(_))
        ));
        let bad_regex = r#"{"name":"x","specifications":[{"implementation":"ReleaseTitleSpecification","fields":{"value":"("}}]}"#;
        assert!(matches!(
            import_json(bad_regex),
            Err(DecisionError::InvalidRegex { .. })
        ));
        assert_eq!(import_json("[]").unwrap(), vec![]);
    }
}
