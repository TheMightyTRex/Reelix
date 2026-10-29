//! Custom formats: named sets of conditions on a release, each scored by quality profiles.
//!
//! Evaluation follows the Radarr/Sonarr semantics: conditions are grouped by kind; within a
//! group every `required` condition must match and, if the group has non-required conditions,
//! at least one of them must match. `negate` inverts a single condition. A format matches
//! when every group passes.

use std::sync::OnceLock;

use reelix_parser::{Language, ParsedRelease, Quality, ReleaseKind, Resolution, Source};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::DecisionError;
use crate::profile::QualityProfile;

/// A case-insensitive regular expression, compiled once on first use.
#[derive(Clone, Default)]
pub struct Pattern {
    pattern: String,
    compiled: OnceLock<Option<Regex>>,
}

impl Pattern {
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            compiled: OnceLock::new(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Compiles the pattern, reporting a syntax error.
    pub fn validate(&self) -> Result<(), DecisionError> {
        compile(&self.pattern).map(|_| ())
    }

    /// True if the pattern matches; an invalid pattern never matches.
    pub fn is_match(&self, haystack: &str) -> bool {
        self.compiled
            .get_or_init(|| compile(&self.pattern).ok())
            .as_ref()
            .is_some_and(|re| re.is_match(haystack))
    }
}

fn compile(pattern: &str) -> Result<Regex, DecisionError> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| DecisionError::InvalidRegex {
            pattern: pattern.to_string(),
            message: e.to_string(),
        })
}

impl std::fmt::Debug for Pattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "/{}/i", self.pattern)
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.pattern == other.pattern
    }
}

impl Serialize for Pattern {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.pattern)
    }
}

impl<'de> Deserialize<'de> for Pattern {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Pattern::new)
    }
}

/// Quality modifier as used by custom formats (derived from the quality).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityModifier {
    None,
    Regional,
    Screener,
    RawHd,
    BrDisk,
    Remux,
}

impl QualityModifier {
    pub fn of(q: Quality) -> Self {
        match q {
            Quality::Remux1080p | Quality::Remux2160p => Self::Remux,
            Quality::BrDisk => Self::BrDisk,
            Quality::RawHd => Self::RawHd,
            Quality::Regional => Self::Regional,
            Quality::Dvdscr => Self::Screener,
            _ => Self::None,
        }
    }
}

/// Single episode, multi-episode or season pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseType {
    SingleEpisode,
    MultiEpisode,
    SeasonPack,
}

impl ReleaseType {
    pub fn of(p: &ParsedRelease) -> Option<Self> {
        let ReleaseKind::Episode(e) = &p.kind else {
            return None;
        };
        Some(if e.full_season {
            Self::SeasonPack
        } else if e.episodes.len() > 1 || e.absolute_episodes.len() > 1 {
            Self::MultiEpisode
        } else {
            Self::SingleEpisode
        })
    }
}

/// Indexer-supplied release flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexerFlag {
    Freeleech,
    Halfleech,
    DoubleUpload,
    Freeleech75,
    Freeleech25,
    Internal,
    Scene,
    Golden,
    Approved,
    Nuked,
}

/// What a single condition tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Condition {
    ReleaseTitle {
        value: Pattern,
    },
    ReleaseGroup {
        value: Pattern,
    },
    Edition {
        value: Pattern,
    },
    Source {
        value: Source,
    },
    Resolution {
        value: Resolution,
    },
    QualityModifier {
        value: QualityModifier,
    },
    /// `value: None` = any language. With `except`, matches releases having any *other* language.
    Language {
        value: Option<Language>,
        #[serde(default)]
        except: bool,
    },
    IndexerFlag {
        value: IndexerFlag,
    },
    /// Size in GB: `min < size ≤ max`.
    Size {
        min_gb: f64,
        max_gb: f64,
    },
    ReleaseType {
        value: ReleaseType,
    },
    Year {
        min: u16,
        max: u16,
    },
}

impl Condition {
    /// Stable kind name (used for grouping and in the UI).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ReleaseTitle { .. } => "release_title",
            Self::ReleaseGroup { .. } => "release_group",
            Self::Edition { .. } => "edition",
            Self::Source { .. } => "source",
            Self::Resolution { .. } => "resolution",
            Self::QualityModifier { .. } => "quality_modifier",
            Self::Language { .. } => "language",
            Self::IndexerFlag { .. } => "indexer_flag",
            Self::Size { .. } => "size",
            Self::ReleaseType { .. } => "release_type",
            Self::Year { .. } => "year",
        }
    }

    /// Raw match, before `negate`.
    pub fn matches(&self, input: &FormatInput<'_>) -> bool {
        let p = input.parsed;
        let q = p.quality.quality;
        match self {
            Self::ReleaseTitle { value } => value.is_match(input.title),
            Self::ReleaseGroup { value } => p.release_group.as_deref().is_some_and(|g| value.is_match(g)),
            Self::Edition { value } => p.edition.as_deref().is_some_and(|e| value.is_match(e)),
            Self::Source { value } => {
                let s = q.source();
                s == *value || (*value == Source::Bluray && matches!(s, Source::Remux | Source::BrDisk))
            }
            Self::Resolution { value } => q.resolution() == *value,
            Self::QualityModifier { value } => QualityModifier::of(q) == *value,
            Self::Language { value, except } => {
                let target = match value {
                    None => return !*except,
                    Some(Language::Original) => match input.original_language {
                        Some(l) => l,
                        None => return false,
                    },
                    Some(l) => *l,
                };
                if *except {
                    p.languages.iter().any(|l| *l != target)
                } else {
                    p.languages.contains(&target)
                }
            }
            Self::IndexerFlag { value } => input.indexer_flags.contains(value),
            Self::Size { min_gb, max_gb } => input.size.is_some_and(|s| {
                let gb = s as f64 / (1024.0 * 1024.0 * 1024.0);
                gb > *min_gb && gb <= *max_gb
            }),
            Self::ReleaseType { value } => ReleaseType::of(p) == Some(*value),
            Self::Year { min, max } => p.year.is_some_and(|y| y >= *min && y <= *max),
        }
    }

    fn validate(&self) -> Result<(), DecisionError> {
        match self {
            Self::ReleaseTitle { value } | Self::ReleaseGroup { value } | Self::Edition { value } => {
                value.validate()
            }
            _ => Ok(()),
        }
    }
}

/// One condition of a custom format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormatSpecification {
    pub name: String,
    #[serde(default)]
    pub negate: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(flatten)]
    pub condition: Condition,
}

impl FormatSpecification {
    pub fn new(name: &str, condition: Condition) -> Self {
        Self {
            name: name.to_string(),
            negate: false,
            required: false,
            condition,
        }
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn negate(mut self) -> Self {
        self.negate = true;
        self
    }

    /// Match after applying `negate`.
    pub fn is_satisfied(&self, input: &FormatInput<'_>) -> bool {
        self.condition.matches(input) != self.negate
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomFormat {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub include_in_rename: bool,
    pub specifications: Vec<FormatSpecification>,
}

/// The facts custom formats are evaluated against.
#[derive(Debug, Clone, Copy)]
pub struct FormatInput<'a> {
    pub parsed: &'a ParsedRelease,
    /// Full release title (as reported by the indexer, or the file name).
    pub title: &'a str,
    pub size: Option<u64>,
    pub indexer_flags: &'a [IndexerFlag],
    /// The media's original language, for `Language: Original` conditions.
    pub original_language: Option<Language>,
}

impl<'a> FormatInput<'a> {
    /// Input built from a parsed release only (title = raw title).
    pub fn from_parsed(parsed: &'a ParsedRelease) -> Self {
        Self {
            parsed,
            title: &parsed.raw_title,
            size: None,
            indexer_flags: &[],
            original_language: None,
        }
    }
}

impl CustomFormat {
    /// True when the format matches (see the module docs for the semantics).
    /// A format without specifications never matches.
    pub fn matches(&self, input: &FormatInput<'_>) -> bool {
        if self.specifications.is_empty() {
            return false;
        }
        let mut kinds: Vec<&'static str> = self.specifications.iter().map(|s| s.condition.kind()).collect();
        kinds.dedup();
        kinds.sort_unstable();
        kinds.dedup();
        kinds.into_iter().all(|kind| {
            let group = self.specifications.iter().filter(|s| s.condition.kind() == kind);
            let mut any_optional = false;
            let mut optional_hit = false;
            for spec in group {
                let ok = spec.is_satisfied(input);
                if spec.required {
                    if !ok {
                        return false;
                    }
                } else {
                    any_optional = true;
                    optional_hit |= ok;
                }
            }
            !any_optional || optional_hit
        })
    }

    /// Validates the name and every regular expression.
    pub fn validate(&self) -> Result<(), DecisionError> {
        if self.name.trim().is_empty() {
            return Err(DecisionError::InvalidProfile(
                "custom format name is required".into(),
            ));
        }
        self.specifications
            .iter()
            .try_for_each(|s| s.condition.validate())
    }
}

/// Formats (from `formats`) matching `input`.
pub fn matching_formats<'f>(formats: &'f [CustomFormat], input: &FormatInput<'_>) -> Vec<&'f CustomFormat> {
    formats.iter().filter(|f| f.matches(input)).collect()
}

/// Sum of the profile's scores for the given formats.
pub fn score_formats(profile: &QualityProfile, formats: &[&CustomFormat]) -> i32 {
    formats.iter().map(|f| profile.format_score(f.id)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelix_parser::parse;

    fn cf(specs: Vec<FormatSpecification>) -> CustomFormat {
        CustomFormat {
            id: 1,
            name: "t".into(),
            include_in_rename: false,
            specifications: specs,
        }
    }

    fn title(re: &str) -> Condition {
        Condition::ReleaseTitle {
            value: Pattern::new(re),
        }
    }

    #[test]
    fn group_semantics() {
        let p = parse("Movie.2020.2160p.WEB-DL.DDP5.1.DV.HDR.H.265-FLUX");
        let input = FormatInput::from_parsed(&p);
        // One of the optional title conditions matches.
        assert!(
            cf(vec![
                FormatSpecification::new("a", title(r"\bDV\b")),
                FormatSpecification::new("b", title("nope"))
            ])
            .matches(&input)
        );
        // A required one fails.
        assert!(
            !cf(vec![
                FormatSpecification::new("a", title(r"\bDV\b")),
                FormatSpecification::new("b", title("nope")).required()
            ])
            .matches(&input)
        );
        // Negated required.
        assert!(
            cf(vec![
                FormatSpecification::new("x", title("x264")).required().negate()
            ])
            .matches(&input)
        );
        // Different kinds must each pass.
        let res = FormatSpecification::new(
            "r",
            Condition::Resolution {
                value: Resolution::R2160p,
            },
        );
        let src = FormatSpecification::new(
            "s",
            Condition::Source {
                value: Source::Bluray,
            },
        );
        assert!(!cf(vec![res.clone(), src]).matches(&input));
        let web = FormatSpecification::new("s", Condition::Source { value: Source::WebDl });
        assert!(cf(vec![res, web]).matches(&input));
        assert!(!cf(vec![]).matches(&input));
    }

    #[test]
    fn other_conditions() {
        let p = parse("Movie.2020.FRENCH.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-GRP");
        let mut input = FormatInput::from_parsed(&p);
        let ok = |c: Condition, i: &FormatInput<'_>| cf(vec![FormatSpecification::new("c", c)]).matches(i);
        assert!(ok(
            Condition::QualityModifier {
                value: QualityModifier::Remux
            },
            &input
        ));
        assert!(ok(
            Condition::Source {
                value: Source::Bluray
            },
            &input
        ));
        assert!(ok(
            Condition::Language {
                value: Some(Language::French),
                except: false
            },
            &input
        ));
        assert!(!ok(
            Condition::Language {
                value: Some(Language::English),
                except: false
            },
            &input
        ));
        assert!(ok(
            Condition::Language {
                value: Some(Language::English),
                except: true
            },
            &input
        ));
        assert!(!ok(
            Condition::Language {
                value: Some(Language::Original),
                except: false
            },
            &input
        ));
        input.original_language = Some(Language::French);
        assert!(ok(
            Condition::Language {
                value: Some(Language::Original),
                except: false
            },
            &input
        ));
        assert!(ok(
            Condition::ReleaseGroup {
                value: Pattern::new("^grp$")
            },
            &input
        ));
        assert!(ok(Condition::Year { min: 2019, max: 2021 }, &input));
        assert!(!ok(
            Condition::Size {
                min_gb: 1.0,
                max_gb: 10.0
            },
            &input
        ));
        input.size = Some(5 * 1024 * 1024 * 1024);
        assert!(ok(
            Condition::Size {
                min_gb: 1.0,
                max_gb: 10.0
            },
            &input
        ));
        input.indexer_flags = &[IndexerFlag::Freeleech];
        assert!(ok(
            Condition::IndexerFlag {
                value: IndexerFlag::Freeleech
            },
            &input
        ));
        let ep = parse("Show.S01.1080p.WEB-DL-GRP");
        assert!(ok(
            Condition::ReleaseType {
                value: ReleaseType::SeasonPack
            },
            &FormatInput::from_parsed(&ep)
        ));
    }

    #[test]
    fn invalid_regex_never_matches_and_fails_validation() {
        let f = cf(vec![FormatSpecification::new("bad", title("(unclosed"))]);
        assert!(f.validate().is_err());
        let p = parse("anything");
        assert!(!f.matches(&FormatInput::from_parsed(&p)));
    }

    #[test]
    fn serde_shape() {
        let s = FormatSpecification::new("x", title("abc")).required();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"name":"x","negate":false,"required":true,"type":"release_title","value":"abc"})
        );
        let back: FormatSpecification = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }
}
