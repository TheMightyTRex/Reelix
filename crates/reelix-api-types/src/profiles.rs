//! Quality profiles, quality definitions, custom formats, delay/release profiles and the
//! release-name tester (`POST /api/v1/parse`).
//!
//! The configuration types themselves come from `reelix-decision` (shared with the UI);
//! in the OpenAPI document they appear as free-form objects.

use serde::{Deserialize, Serialize};

pub use reelix_decision::format::Pattern;
pub use reelix_decision::{
    Condition, CustomFormat, DelayProfile, FormatItem, FormatSpecification, IndexerFlag, ProfileItem,
    QualityDefinition, QualityModifier, QualityProfile, RejectionKind, ReleaseProfile, ReleaseType,
    SizeBasis,
};
pub use reelix_parser::{
    AudioCodec, EpisodeInfo, HdrFormat, Language, ParsedRelease, Quality, QualityFamily, ReleaseKind,
    Resolution, Source, VideoCodec,
};

/// `POST /api/v1/parse` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ParseRequest {
    /// Release title or file name.
    pub title: String,
    /// Evaluate against this quality profile (accept/reject + scores).
    #[serde(default)]
    pub profile_id: Option<i64>,
    /// Release size in bytes, for size-based conditions.
    #[serde(default)]
    pub size: Option<u64>,
}

/// A custom format that matched, with the score the profile gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MatchedFormatDto {
    pub id: i64,
    pub name: String,
    pub score: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RejectionDto {
    pub reason: String,
    /// `permanent` or `temporary`.
    pub kind: String,
    /// Specification that rejected, e.g. `quality`, `custom_format_score`.
    pub spec: String,
}

/// Result of evaluating a parsed release against a quality profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProfileDecisionDto {
    pub profile_id: i64,
    pub profile_name: String,
    pub accepted: bool,
    pub rejections: Vec<RejectionDto>,
    /// Row index in the profile (higher is better); null when the profile doesn't list the quality.
    pub quality_rank: Option<usize>,
}

/// `POST /api/v1/parse` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ParseResponse {
    /// Everything the parser extracted.
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub parsed: ParsedRelease,
    /// Display name of the parsed quality, e.g. `WEBDL-1080p`.
    pub quality: String,
    /// All custom formats that match (scores are 0 without a profile).
    pub matched_formats: Vec<MatchedFormatDto>,
    pub format_score: i32,
    /// Present when `profile_id` was given.
    pub decision: Option<ProfileDecisionDto>,
}

/// `GET /api/v1/customformats/builtin`: one built-in custom format by its stable key, and
/// the format as it is now (`null` once an admin has deleted it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BuiltinFormatDto {
    /// Stable key (`audio_description`, `bsl`); the name can be changed, the key cannot.
    pub key: String,
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub format: Option<CustomFormat>,
}

/// A format created by `POST /api/v1/customformats/import`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImportedFormatDto {
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub format: CustomFormat,
    pub trash_id: Option<String>,
    /// TRaSH's suggested score (`trash_scores.default`).
    pub default_score: Option<i32>,
}
