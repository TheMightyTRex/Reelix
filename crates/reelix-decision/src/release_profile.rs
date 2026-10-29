//! Release profiles: terms a release title must / must not contain.

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::DecisionError;

/// A term: plain text (case-insensitive substring) or `/regex/` with optional `i` flag.
#[derive(Debug, Clone)]
pub enum Term {
    Text(String),
    Regex(Regex),
}

impl Term {
    pub fn parse(raw: &str) -> Result<Term, DecisionError> {
        let t = raw.trim();
        if t.len() >= 2
            && t.starts_with('/')
            && let Some(end) = t.rfind('/')
            && end > 0
        {
            let (body, flags) = (&t[1..end], &t[end + 1..]);
            if flags.chars().all(|c| c == 'i') {
                let re = regex::RegexBuilder::new(body)
                    .case_insensitive(flags.contains('i'))
                    .size_limit(1 << 20)
                    .build()
                    .map_err(|e| DecisionError::InvalidRegex {
                        pattern: raw.to_string(),
                        message: e.to_string(),
                    })?;
                return Ok(Term::Regex(re));
            }
        }
        Ok(Term::Text(t.to_lowercase()))
    }

    pub fn is_match(&self, title: &str) -> bool {
        match self {
            Term::Text(t) => !t.is_empty() && title.to_lowercase().contains(t.as_str()),
            Term::Regex(re) => re.is_match(title),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseProfile {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// At least one must match (when non-empty).
    #[serde(default)]
    pub required: Vec<String>,
    /// None may match.
    #[serde(default)]
    pub ignored: Vec<String>,
    /// Restrict to one indexer; `None` = all indexers.
    #[serde(default)]
    pub indexer_id: Option<i64>,
    /// Empty = applies to all media.
    #[serde(default)]
    pub tags: Vec<i64>,
}

fn yes() -> bool {
    true
}

impl ReleaseProfile {
    /// Validates every term (regex syntax).
    pub fn validate(&self) -> Result<(), DecisionError> {
        self.required
            .iter()
            .chain(&self.ignored)
            .try_for_each(|t| Term::parse(t).map(|_| ()))
    }

    /// Whether this profile applies to a release from `indexer_id` for media tagged `tags`.
    pub fn applies(&self, indexer_id: Option<i64>, tags: &[i64]) -> bool {
        self.enabled
            && self.indexer_id.is_none_or(|id| Some(id) == indexer_id)
            && (self.tags.is_empty() || self.tags.iter().any(|t| tags.contains(t)))
    }

    /// Reasons `title` violates this profile (empty = OK). Invalid terms are skipped.
    pub fn violations(&self, title: &str) -> Vec<String> {
        let mut out = Vec::new();
        let required: Vec<Term> = self.required.iter().filter_map(|t| Term::parse(t).ok()).collect();
        if !required.is_empty() && !required.iter().any(|t| t.is_match(title)) {
            out.push(format!(
                "Does not contain any of the required terms: {}",
                self.required.join(", ")
            ));
        }
        for (raw, term) in self
            .ignored
            .iter()
            .filter_map(|t| Some((t, Term::parse(t).ok()?)))
        {
            if term.is_match(title) {
                out.push(format!("Contains ignored term {raw:?}"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms() {
        assert!(Term::parse("x265").unwrap().is_match("Movie.2020.X265-GRP"));
        assert!(Term::parse("/\\bHDR\\b/i").unwrap().is_match("movie.hdr.x265"));
        assert!(!Term::parse("/\\bHDR\\b/").unwrap().is_match("movie.hdr.x265"));
        assert!(Term::parse("/(bad/i").is_err());
        let p = ReleaseProfile {
            id: 1,
            name: "p".into(),
            enabled: true,
            required: vec!["1080p".into(), "2160p".into()],
            ignored: vec!["/\\bCAM\\b/i".into()],
            indexer_id: Some(3),
            tags: vec![],
        };
        assert!(p.violations("Movie.2020.1080p.WEB-DL").is_empty());
        assert_eq!(p.violations("Movie.2020.720p.CAM").len(), 2);
        assert!(p.applies(Some(3), &[]));
        assert!(!p.applies(Some(4), &[]));
    }
}
