//! Quality definitions, quality profiles, custom formats, delay/release profiles and the
//! release decision engine (specifications → accept/reject with reasons → prioritisation).
//!
//! Everything here is pure and synchronous so the web UI can reuse it (wasm-compatible).

pub mod definition;
pub mod delay;
pub mod engine;
pub mod family;
pub mod format;
pub mod profile;
pub mod release_profile;
pub mod trash;

pub use definition::{QualityDefinition, SizeBasis, default_definitions, default_definitions_for};
pub use delay::DelayProfile;
pub use engine::{
    Decision, DecisionContext, ExistingFile, FormatRef, IndexerFlag, RankedRelease, Rejection, RejectionKind,
    ReleaseCandidate, Scored, SearchKind, Specification, default_specifications, evaluate, is_upgrade,
    prioritize,
};
pub use family::{family_for, size_runtime_minutes, unknown_quality};
pub use format::{
    Condition, CustomFormat, FormatInput, FormatSpecification, QualityModifier, ReleaseType,
    matching_formats, score_formats,
};
pub use profile::{FormatItem, ProfileItem, QualityProfile, default_profiles, default_profiles_for};
pub use release_profile::{ReleaseProfile, Term};

/// Errors from validating or importing decision configuration.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum DecisionError {
    #[error("invalid regular expression {pattern:?}: {message}")]
    InvalidRegex { pattern: String, message: String },
    #[error("invalid custom format JSON: {0}")]
    InvalidJson(String),
    #[error("unsupported specification implementation {0:?}")]
    UnsupportedImplementation(String),
    #[error("invalid profile: {0}")]
    InvalidProfile(String),
}
