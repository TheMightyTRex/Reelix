//! Built-in custom formats: accessibility versions Reelix recognises by name without an
//! import. Each has a stable [`BuiltinFormat::key`], so clients (such as a dashboard's
//! audio description plug-in) find the built-in format instead of adding a second one.
//! They match by release title only and score nothing until a quality profile gives them
//! a score.

use crate::format::{Condition, CustomFormat, FormatSpecification, Pattern};

/// One built-in custom format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinFormat {
    /// Stable key for `GET /api/v1/customformats/builtin/{key}`; never changes, even if the
    /// format is renamed.
    pub key: &'static str,
    /// The name it is created with.
    pub name: &'static str,
    /// `(specification name, release title pattern)`; any one matching is enough.
    pub patterns: &'static [(&'static str, &'static str)],
}

/// Audio description: a spoken description of what is on screen, for blind and partially
/// sighted viewers.
pub const AUDIO_DESCRIPTION: BuiltinFormat = BuiltinFormat {
    key: "audio_description",
    name: "Audio description",
    patterns: &[
        ("Audio description", r"\baudio[ ._-]?descri(bed|ption)\b"),
        ("Described video", r"\bdescri(bed|ptive)[ ._-]?(video|audio)\b"),
        ("DVS", r"\bDVS\b"),
        (
            "AD beside the audio",
            r"[ ._-]AD[ ._-](AAC|E-?AC-?3|DDP?|DTS|Opus)|\b(AAC|E-?AC-?3|DDP?|DTS|Opus)[0-9. ]*[ ._-]AD\b",
        ),
    ],
};

/// British Sign Language: a version with a signer in vision.
pub const BSL: BuiltinFormat = BuiltinFormat {
    key: "bsl",
    name: "BSL",
    patterns: &[
        ("BSL", r"\b(with[ ._-]?)?BSL\b"),
        ("British Sign Language", r"\bbritish[ ._-]?sign[ ._-]?language\b"),
        ("SignZone", r"\bsign[ ._-]?zone\b"),
        ("Signed version", r"\bsigned[ ._-]?(version|edition)\b"),
    ],
};

/// Every built-in custom format.
pub const BUILTIN_FORMATS: &[BuiltinFormat] = &[AUDIO_DESCRIPTION, BSL];

impl BuiltinFormat {
    /// The built-in format with this key.
    pub fn by_key(key: &str) -> Option<&'static BuiltinFormat> {
        BUILTIN_FORMATS.iter().find(|f| f.key == key)
    }

    /// The custom format to create (id 0).
    pub fn format(&self) -> CustomFormat {
        CustomFormat {
            id: 0,
            name: self.name.to_string(),
            include_in_rename: false,
            specifications: self
                .patterns
                .iter()
                .map(|(name, re)| {
                    FormatSpecification::new(
                        name,
                        Condition::ReleaseTitle {
                            value: Pattern::new(*re),
                        },
                    )
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use reelix_parser::parse;

    use super::*;
    use crate::format::FormatInput;

    fn hits(f: &BuiltinFormat, title: &str) -> bool {
        let p = parse(title);
        f.format().matches(&FormatInput::from_parsed(&p))
    }

    #[test]
    fn builtins_are_valid_and_keys_unique() {
        for f in BUILTIN_FORMATS {
            f.format().validate().unwrap();
            assert_eq!(BuiltinFormat::by_key(f.key), Some(f));
        }
        assert_eq!(BuiltinFormat::by_key("nope"), None);
    }

    #[test]
    fn audio_description_releases() {
        for t in [
            "The.Movie.2021.1080p.WEB-DL.Audio.Description.AAC2.0.H.264-GRP",
            "The Movie (2021) Audio Described 720p",
            "Show.S01E01.1080p.iP.WEB-DL.AAC2.0.AD.H.264-GRP",
            "Show.S01E01.1080p.WEB-DL.AD.DDP5.1.H.264-GRP",
            "The.Movie.2021.Descriptive.Video.1080p.BluRay.x264-GRP",
            "The.Movie.2021.1080p.BluRay.DVS.x264-GRP",
        ] {
            assert!(hits(&AUDIO_DESCRIPTION, t), "{t}");
        }
        for t in [
            "Ad.Astra.2019.1080p.BluRay.x264-GRP",
            "The.Road.2009.1080p.BluRay.AAC2.0.x264-GRP",
            "Mad.Max.Fury.Road.2015.2160p.UHD.BluRay.DTS-HD.MA.7.1-GRP",
            "Show.S02E03.ADR.Session.1080p.WEB-DL.AAC2.0-GRP",
            "Show.S01E01.1080p.WEB-DL.DDP5.1.H.264-GRP",
        ] {
            assert!(!hits(&AUDIO_DESCRIPTION, t), "{t}");
        }
    }

    #[test]
    fn bsl_releases() {
        for t in [
            "Show.S01E01.WiTH.BSL.1080p.iP.WEB-DL.AAC2.0.H.264-GRP",
            "Show.S01E01.BSL.720p.HDTV.x264-GRP",
            "Show S01E01 British Sign Language 1080p",
            "Show.S01E01.SignZone.720p.WEB-DL-GRP",
            "Show.S01E01.Signed.Version.1080p.iP.WEB-DL-GRP",
        ] {
            assert!(hits(&BSL, t), "{t}");
        }
        for t in [
            "Bsltown.S01E01.1080p.WEB-DL-GRP",
            "Signed.Sealed.Delivered.S01E01.720p.HDTV-GRP",
            "Show.S01E01.1080p.WEB-DL.AAC2.0-GRP",
        ] {
            assert!(!hits(&BSL, t), "{t}");
        }
    }
}
