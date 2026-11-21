//! Where a subtitle is saved: beside the video, named as Jellyfin reads it.
//!
//! `Name (Year).mkv` gets `Name (Year).en.srt`, `Name (Year).en.sdh.srt` and
//! `Name (Year).en.forced.srt`. Jellyfin takes the language from the part after the video's
//! name and the `sdh` and `forced` flags from the parts after that.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A kind of subtitle for one language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Variant {
    Normal,
    /// For people who are deaf or hard of hearing.
    Sdh,
    /// Only the foreign-language parts.
    Forced,
}

impl Variant {
    pub const ALL: [Variant; 3] = [Variant::Normal, Variant::Sdh, Variant::Forced];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Sdh => "sdh",
            Self::Forced => "forced",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// Normalises a language code from the settings: `EN` → `en`, `pt-br` → `pt-BR`.
/// `None` when it is not a two-letter code with an optional two-letter region.
pub fn normalise_language(code: &str) -> Option<String> {
    let code = code.trim().replace('_', "-");
    let mut parts = code.split('-');
    let lang = parts.next()?.to_ascii_lowercase();
    if lang.len() != 2 || !lang.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    match (parts.next(), parts.next()) {
        (None, _) => Some(lang),
        (Some(r), None) if r.len() == 2 && r.bytes().all(|b| b.is_ascii_alphabetic()) => {
            Some(format!("{lang}-{}", r.to_ascii_uppercase()))
        }
        _ => None,
    }
}

/// The sidecar path for `video`, `language` and `variant`.
pub fn subtitle_path(video: &Path, language: &str, variant: Variant) -> PathBuf {
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let flag = match variant {
        Variant::Normal => "",
        Variant::Sdh => ".sdh",
        Variant::Forced => ".forced",
    };
    video.with_file_name(format!("{stem}.{language}{flag}.srt"))
}

/// ISO 639-2 codes (bibliographic and terminology) for a two-letter code, as Stremio add-ons
/// usually label languages (`eng`). Common languages only; others match on the two-letter code.
pub fn three_letter_codes(code: &str) -> &'static [&'static str] {
    let lang = code.split('-').next().unwrap_or(code);
    match lang {
        "ar" => &["ara"],
        "bg" => &["bul"],
        "bn" => &["ben"],
        "ca" => &["cat"],
        "cs" => &["cze", "ces"],
        "cy" => &["wel", "cym"],
        "da" => &["dan"],
        "de" => &["ger", "deu"],
        "el" => &["gre", "ell"],
        "en" => &["eng"],
        "es" => &["spa", "spn", "spl"],
        "et" => &["est"],
        "eu" => &["baq", "eus"],
        "fa" => &["per", "fas"],
        "fi" => &["fin"],
        "fr" => &["fre", "fra"],
        "ga" => &["gle"],
        "gd" => &["gla"],
        "he" => &["heb"],
        "hi" => &["hin"],
        "hr" => &["hrv"],
        "hu" => &["hun"],
        "id" => &["ind"],
        "is" => &["ice", "isl"],
        "it" => &["ita"],
        "ja" => &["jpn"],
        "ko" => &["kor"],
        "lt" => &["lit"],
        "lv" => &["lav"],
        "ms" => &["may", "msa"],
        "nl" => &["dut", "nld"],
        "no" | "nb" => &["nor", "nob"],
        "pl" => &["pol"],
        "pt" if code.eq_ignore_ascii_case("pt-BR") => &["pob"],
        "pt" => &["por"],
        "ro" => &["rum", "ron"],
        "ru" => &["rus"],
        "sk" => &["slo", "slk"],
        "sl" => &["slv"],
        "sr" => &["srp", "scc"],
        "sv" => &["swe"],
        "th" => &["tha"],
        "tr" => &["tur"],
        "uk" => &["ukr"],
        "vi" => &["vie"],
        "zh" => &["chi", "zho"],
        _ => &[],
    }
}

/// Whether a source's language label (`en`, `eng`, `pt-BR`, `pob`) is the language from the
/// settings. A plain `pt` does not match `pt-BR` and the other way round.
pub fn language_matches(wanted: &str, label: &str) -> bool {
    let label = label.trim();
    if label.eq_ignore_ascii_case(wanted) {
        return true;
    }
    three_letter_codes(wanted)
        .iter()
        .any(|c| label.eq_ignore_ascii_case(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_jellyfin() {
        let v = Path::new("/media/films/Heat (1995)/Heat (1995).mkv");
        assert_eq!(
            subtitle_path(v, "en", Variant::Normal),
            Path::new("/media/films/Heat (1995)/Heat (1995).en.srt")
        );
        assert_eq!(
            subtitle_path(v, "en", Variant::Sdh),
            Path::new("/media/films/Heat (1995)/Heat (1995).en.sdh.srt")
        );
        assert_eq!(
            subtitle_path(v, "en", Variant::Forced),
            Path::new("/media/films/Heat (1995)/Heat (1995).en.forced.srt")
        );
    }

    #[test]
    fn names_keep_dots_in_the_video_name() {
        let v = Path::new("/tv/Show/Season 01/Show - S01E02 - Mr. Smith.mp4");
        assert_eq!(
            subtitle_path(v, "pt-BR", Variant::Normal),
            Path::new("/tv/Show/Season 01/Show - S01E02 - Mr. Smith.pt-BR.srt")
        );
    }

    #[test]
    fn language_codes_are_normalised() {
        assert_eq!(normalise_language(" EN ").as_deref(), Some("en"));
        assert_eq!(normalise_language("pt_br").as_deref(), Some("pt-BR"));
        assert_eq!(normalise_language("eng"), None);
        assert_eq!(normalise_language("e1"), None);
        assert_eq!(normalise_language("en-GB-x"), None);
    }

    #[test]
    fn source_labels_match_settings_codes() {
        assert!(language_matches("en", "eng"));
        assert!(language_matches("en", "EN"));
        assert!(language_matches("fr", "fre"));
        assert!(language_matches("pt-BR", "pob"));
        assert!(!language_matches("pt-BR", "por"));
        assert!(!language_matches("pt", "pob"));
        assert!(!language_matches("en", "spa"));
    }
}
