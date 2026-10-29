//! Quality profiles: ordered (and optionally grouped) qualities, cutoff, upgrade rules,
//! custom-format scores and the preferred language.

use reelix_parser::{Language, Quality, QualityFamily};
use serde::{Deserialize, Serialize};

use crate::DecisionError;

/// First id used for quality groups, so a cutoff id is unambiguous (qualities use their own ids).
pub const GROUP_ID_BASE: u32 = 1000;

/// One row of a profile: a single quality or a named group of equally-ranked qualities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileItem {
    /// Group id (≥ [`GROUP_ID_BASE`]); `None` for single-quality rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u32>,
    /// Group name; `None` for single-quality rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The quality of a single-quality row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<Quality>,
    /// Members of a group row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<Quality>,
    pub allowed: bool,
}

impl ProfileItem {
    pub fn quality(q: Quality, allowed: bool) -> Self {
        Self {
            id: None,
            name: None,
            quality: Some(q),
            items: Vec::new(),
            allowed,
        }
    }

    pub fn group(id: u32, name: &str, items: Vec<Quality>, allowed: bool) -> Self {
        Self {
            id: Some(id),
            name: Some(name.to_string()),
            quality: None,
            items,
            allowed,
        }
    }

    /// The id a cutoff refers to: group id or quality id.
    pub fn cutoff_id(&self) -> u32 {
        self.id
            .unwrap_or_else(|| self.quality.map_or(0, |q| q.id() as u32))
    }

    pub fn contains(&self, q: Quality) -> bool {
        self.quality == Some(q) || self.items.contains(&q)
    }

    /// Display name: group name or quality name.
    pub fn display_name(&self) -> String {
        match (&self.name, self.quality) {
            (Some(n), _) => n.clone(),
            (None, Some(q)) => q.name().to_string(),
            _ => String::new(),
        }
    }
}

/// Score a profile gives a custom format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatItem {
    pub format_id: i64,
    pub score: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityProfile {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    /// Ordered worst → best; a release's rank is the index of the row containing its quality.
    pub items: Vec<ProfileItem>,
    /// Quality id or group id at which upgrades stop.
    pub cutoff: u32,
    pub upgrade_allowed: bool,
    #[serde(default)]
    pub min_format_score: i32,
    #[serde(default)]
    pub cutoff_format_score: i32,
    /// A same-quality upgrade must improve the format score by at least this much.
    #[serde(default = "one")]
    pub min_upgrade_format_score: i32,
    #[serde(default)]
    pub format_items: Vec<FormatItem>,
    /// Required audio language; `None` = any, [`Language::Original`] = the media's original language.
    #[serde(default)]
    pub language: Option<Language>,
    /// Which quality family the profile ranks. Existing rows/JSON default to `Video`
    /// (omitted from JSON for video profiles, so their wire shape is unchanged).
    #[serde(default, skip_serializing_if = "is_video")]
    pub family: QualityFamily,
}

fn one() -> i32 {
    1
}

fn is_video(f: &QualityFamily) -> bool {
    *f == QualityFamily::Video
}

impl QualityProfile {
    /// Rank (row index, higher is better) of `q`, if the profile lists it at all.
    pub fn rank(&self, q: Quality) -> Option<usize> {
        self.items.iter().position(|i| i.contains(q))
    }

    pub fn is_allowed(&self, q: Quality) -> bool {
        self.items.iter().any(|i| i.allowed && i.contains(q))
    }

    /// Rank of the cutoff row.
    pub fn cutoff_rank(&self) -> Option<usize> {
        self.items.iter().position(|i| i.cutoff_id() == self.cutoff)
    }

    /// The best allowed quality row's rank.
    pub fn best_allowed_rank(&self) -> Option<usize> {
        self.items.iter().rposition(|i| i.allowed)
    }

    /// Score assigned to a custom format id (0 when unscored).
    pub fn format_score(&self, format_id: i64) -> i32 {
        self.format_items
            .iter()
            .find(|f| f.format_id == format_id)
            .map_or(0, |f| f.score)
    }

    /// True when `q` with `score` satisfies both the quality and the format-score cutoff.
    pub fn cutoff_met(&self, q: Quality, score: i32) -> bool {
        match (self.rank(q), self.cutoff_rank()) {
            (Some(r), Some(c)) => r >= c && score >= self.cutoff_format_score,
            _ => false,
        }
    }

    /// Checks structural invariants: every quality at most once and of the profile's
    /// [`family`](Self::family), unique group ids, non-empty groups, an allowed cutoff row.
    pub fn validate(&self) -> Result<(), DecisionError> {
        let err = |m: String| Err(DecisionError::InvalidProfile(m));
        if self.name.trim().is_empty() {
            return err("name is required".into());
        }
        let mut seen = Vec::new();
        let mut groups = Vec::new();
        for item in &self.items {
            let members: Vec<Quality> = match (item.quality, item.id) {
                (Some(q), None) if item.items.is_empty() => vec![q],
                (None, Some(id)) => {
                    if id < GROUP_ID_BASE {
                        return err(format!("group id {id} must be ≥ {GROUP_ID_BASE}"));
                    }
                    if groups.contains(&id) {
                        return err(format!("duplicate group id {id}"));
                    }
                    groups.push(id);
                    if item.items.is_empty() {
                        return err(format!("group {:?} is empty", item.display_name()));
                    }
                    item.items.clone()
                }
                _ => return err("each item is either a quality or a group".into()),
            };
            for q in members {
                if q.family() != self.family {
                    return err(format!(
                        "{} is a {} quality and can't be used in a {} profile",
                        q.name(),
                        q.family().as_str(),
                        self.family.as_str()
                    ));
                }
                if seen.contains(&q) {
                    return err(format!("{} appears more than once", q.name()));
                }
                seen.push(q);
            }
        }
        match self.items.iter().find(|i| i.cutoff_id() == self.cutoff) {
            Some(i) if i.allowed => Ok(()),
            Some(_) => err("the cutoff quality must be allowed".into()),
            None => err(format!("cutoff {} is not in the profile", self.cutoff)),
        }
    }

    /// A profile listing every quality (default order) with only `allowed` enabled.
    pub fn with_allowed(name: &str, allowed: &[Quality], cutoff: Quality) -> Self {
        let mut items: Vec<ProfileItem> = Vec::new();
        let mut next_group = GROUP_ID_BASE;
        let mut push_group = |items: &mut Vec<ProfileItem>, gname: &str, qs: &[Quality]| {
            let on = qs.iter().any(|q| allowed.contains(q));
            items.push(ProfileItem::group(next_group, gname, qs.to_vec(), on));
            next_group += 1;
        };
        use Quality as Q;
        for q in [
            Q::Unknown,
            Q::Workprint,
            Q::Cam,
            Q::Telesync,
            Q::Telecine,
            Q::Regional,
            Q::Dvdscr,
            Q::Sdtv,
            Q::Dvd,
            Q::DvdR,
        ] {
            items.push(ProfileItem::quality(q, allowed.contains(&q)));
        }
        push_group(&mut items, "WEB 480p", &[Q::WebDl480p, Q::WebRip480p]);
        for q in [Q::Bluray480p, Q::Bluray576p, Q::Hdtv720p] {
            items.push(ProfileItem::quality(q, allowed.contains(&q)));
        }
        push_group(&mut items, "WEB 720p", &[Q::WebDl720p, Q::WebRip720p]);
        for q in [Q::Bluray720p, Q::Hdtv1080p] {
            items.push(ProfileItem::quality(q, allowed.contains(&q)));
        }
        push_group(&mut items, "WEB 1080p", &[Q::WebDl1080p, Q::WebRip1080p]);
        for q in [Q::Bluray1080p, Q::Remux1080p, Q::Hdtv2160p] {
            items.push(ProfileItem::quality(q, allowed.contains(&q)));
        }
        push_group(&mut items, "WEB 2160p", &[Q::WebDl2160p, Q::WebRip2160p]);
        for q in [Q::Bluray2160p, Q::Remux2160p, Q::BrDisk, Q::RawHd] {
            items.push(ProfileItem::quality(q, allowed.contains(&q)));
        }
        let cutoff_id = items
            .iter()
            .find(|i| i.contains(cutoff))
            .map_or(cutoff.id() as u32, ProfileItem::cutoff_id);
        Self {
            id: 0,
            name: name.to_string(),
            items,
            cutoff: cutoff_id,
            upgrade_allowed: true,
            min_format_score: 0,
            cutoff_format_score: 0,
            min_upgrade_format_score: 1,
            format_items: Vec::new(),
            language: Some(Language::English),
            family: QualityFamily::Video,
        }
    }
    /// A profile of `family` listing every quality of the family (default order, worst → best,
    /// no groups) with only `allowed` enabled. `Video` is [`with_allowed`](Self::with_allowed)
    /// (which also groups the WEB qualities). Non-video profiles have no language requirement.
    pub fn for_family(name: &str, family: QualityFamily, allowed: &[Quality], cutoff: Quality) -> Self {
        if family == QualityFamily::Video {
            return Self::with_allowed(name, allowed, cutoff);
        }
        Self::ordered(name, family, family.qualities(), allowed, cutoff)
    }

    /// A profile of `family` whose rows are `order` (worst → best; must be qualities of `family`).
    fn ordered(
        name: &str,
        family: QualityFamily,
        order: &[Quality],
        allowed: &[Quality],
        cutoff: Quality,
    ) -> Self {
        Self {
            id: 0,
            name: name.to_string(),
            items: order
                .iter()
                .map(|q| ProfileItem::quality(*q, allowed.contains(q)))
                .collect(),
            cutoff: cutoff.id() as u32,
            upgrade_allowed: true,
            min_format_score: 0,
            cutoff_format_score: 0,
            min_upgrade_format_score: 1,
            format_items: Vec::new(),
            language: None,
            family,
        }
    }

    /// Merges the single-quality rows of `members` into one equally-ranked group at the position
    /// of the first of them. The group is allowed when any member was, and becomes the cutoff
    /// when the cutoff pointed at a member. Members that aren't single rows are ignored.
    pub fn with_group(mut self, name: &str, members: &[Quality]) -> Self {
        let is_member = |i: &ProfileItem| i.quality.is_some_and(|q| members.contains(&q));
        let Some(pos) = self.items.iter().position(is_member) else {
            return self;
        };
        let rows: Vec<ProfileItem> = self.items.iter().filter(|i| is_member(i)).cloned().collect();
        let id = self
            .items
            .iter()
            .filter_map(|i| i.id)
            .max()
            .map_or(GROUP_ID_BASE, |m| m + 1)
            .max(GROUP_ID_BASE);
        let allowed = rows.iter().any(|i| i.allowed);
        if rows.iter().any(|i| i.cutoff_id() == self.cutoff) {
            self.cutoff = id;
        }
        self.items.retain(|i| !is_member(i));
        let qualities = rows.iter().filter_map(|i| i.quality).collect();
        self.items
            .insert(pos, ProfileItem::group(id, name, qualities, allowed));
        self
    }
}

/// Built-in profiles: Any, SD, HD-720p, HD-1080p, Ultra-HD, HD-720p/1080p.
pub fn default_profiles() -> Vec<QualityProfile> {
    use Quality as Q;
    let sd = [
        Q::Sdtv,
        Q::Dvd,
        Q::WebDl480p,
        Q::WebRip480p,
        Q::Bluray480p,
        Q::Bluray576p,
    ];
    let hd720 = [Q::Hdtv720p, Q::WebDl720p, Q::WebRip720p, Q::Bluray720p];
    let hd1080 = [
        Q::Hdtv1080p,
        Q::WebDl1080p,
        Q::WebRip1080p,
        Q::Bluray1080p,
        Q::Remux1080p,
    ];
    let uhd = [
        Q::Hdtv2160p,
        Q::WebDl2160p,
        Q::WebRip2160p,
        Q::Bluray2160p,
        Q::Remux2160p,
    ];
    let any: Vec<Quality> = sd
        .iter()
        .chain(&hd720)
        .chain(&hd1080)
        .chain(&uhd)
        .copied()
        .collect();
    let both: Vec<Quality> = hd720.iter().chain(&hd1080).copied().collect();
    vec![
        QualityProfile::with_allowed("Any", &any, Q::Sdtv),
        QualityProfile::with_allowed("SD", &sd, Q::Sdtv),
        QualityProfile::with_allowed("HD-720p", &hd720, Q::Bluray720p),
        QualityProfile::with_allowed("HD-1080p", &hd1080, Q::Bluray1080p),
        QualityProfile::with_allowed("Ultra-HD", &uhd, Q::Remux2160p),
        QualityProfile::with_allowed("HD-720p/1080p", &both, Q::Bluray1080p),
    ]
}

/// Built-in profiles of one family (Video = [`default_profiles`]), seeded on first access of
/// the family:
///
/// - Ebook: "Ebook: Any" (every format, cutoff EPUB) and "Ebook: EPUB" (EPUB + AZW3, cutoff
///   EPUB). Ranking EPUB > AZW3 > MOBI > PDF > unknown.
/// - Audiobook: "Audiobook: Any" (cutoff M4B) and "Audiobook: M4B" (M4B only). Ranking
///   M4B > MP3 > FLAC > unknown: a chaptered single-file M4B beats MP3, and lossless audiobooks
///   are huge for no audible gain.
/// - Music: "Music: Any" (cutoff MP3-320), "Music: Lossless" (WAV, ALAC/FLAC grouped as
///   "Lossless", 24-bit ALAC/FLAC grouped as "Hi-Res"; cutoff Lossless), "Music: High Quality
///   Lossy" (AAC-256, MP3-VBR-V0, MP3-320, AAC-320; cutoff MP3-320) and "Music: Standard"
///   (every lossy format of about 192 kbps or better; cutoff MP3-320).
///
/// "Any" profiles also allow the family's unknown quality, since book and music release names
/// often carry no format token at all; the cutoff makes such grabs upgradeable.
pub fn default_profiles_for(family: QualityFamily) -> Vec<QualityProfile> {
    use Quality as Q;
    use QualityFamily as F;
    let p = QualityProfile::for_family;
    match family {
        F::Video => default_profiles(),
        F::Ebook => vec![
            p("Ebook: Any", family, Quality::EBOOK, Q::Epub),
            p("Ebook: EPUB", family, &[Q::Azw3, Q::Epub], Q::Epub),
        ],
        F::Audiobook => {
            let order = [
                Q::UnknownAudiobook,
                Q::AudiobookFlac,
                Q::AudiobookMp3,
                Q::AudiobookM4b,
            ];
            vec![
                QualityProfile::ordered("Audiobook: Any", family, &order, &order, Q::AudiobookM4b),
                QualityProfile::ordered(
                    "Audiobook: M4B",
                    family,
                    &order,
                    &[Q::AudiobookM4b],
                    Q::AudiobookM4b,
                ),
            ]
        }
        F::Music => {
            let lossless = [Q::Wav, Q::Alac, Q::Flac, Q::Alac24, Q::Flac24];
            let hq_lossy = [Q::Aac256, Q::Mp3VbrV0, Q::Mp3_320, Q::Aac320];
            let standard = [
                Q::Mp3_192,
                Q::AacVbr,
                Q::Vorbis,
                Q::Opus,
                Q::Mp3VbrV2,
                Q::Mp3_256,
                Q::Aac256,
                Q::Mp3VbrV0,
                Q::Mp3_320,
                Q::Aac320,
            ];
            vec![
                p("Music: Any", family, Quality::MUSIC, Q::Mp3_320),
                p("Music: Lossless", family, &lossless, Q::Flac)
                    .with_group("Lossless", &[Q::Alac, Q::Flac])
                    .with_group("Hi-Res", &[Q::Alac24, Q::Flac24]),
                p("Music: High Quality Lossy", family, &hq_lossy, Q::Mp3_320),
                p("Music: Standard", family, &standard, Q::Mp3_320),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let profiles = default_profiles();
        assert_eq!(profiles.len(), 6);
        for p in &profiles {
            p.validate().unwrap_or_else(|e| panic!("{}: {e}", p.name));
        }
    }

    #[test]
    fn ranking_and_groups() {
        let p = &default_profiles()[3]; // HD-1080p
        assert!(p.is_allowed(Quality::WebDl1080p));
        assert!(p.is_allowed(Quality::WebRip1080p));
        assert!(!p.is_allowed(Quality::Hdtv720p));
        assert_eq!(p.rank(Quality::WebDl1080p), p.rank(Quality::WebRip1080p));
        assert!(p.rank(Quality::Bluray1080p) > p.rank(Quality::WebDl1080p));
        assert!(p.cutoff_met(Quality::Bluray1080p, 0));
        assert!(!p.cutoff_met(Quality::WebDl1080p, 0));
    }

    #[test]
    fn validation_errors() {
        let mut p = default_profiles()[0].clone();
        p.cutoff = 9999;
        assert!(p.validate().is_err());
        let mut p = default_profiles()[0].clone();
        p.items.push(ProfileItem::quality(Quality::Sdtv, true));
        assert!(p.validate().is_err());
        let mut p = default_profiles()[2].clone();
        p.cutoff = Quality::Sdtv.id() as u32;
        assert!(matches!(p.validate(), Err(DecisionError::InvalidProfile(m)) if m.contains("allowed")));
    }

    #[test]
    fn family_defaults_are_valid() {
        for f in QualityFamily::ALL {
            let profiles = default_profiles_for(f);
            assert!(!profiles.is_empty(), "{f:?}");
            for p in &profiles {
                assert_eq!(p.family, f);
                p.validate().unwrap_or_else(|e| panic!("{}: {e}", p.name));
                // Every quality of the family is listed exactly once.
                let listed: usize = p.items.iter().map(|i| i.items.len().max(1)).sum();
                assert_eq!(listed, f.qualities().len(), "{}", p.name);
                if f != QualityFamily::Video {
                    assert_eq!(p.language, None, "{}", p.name);
                }
            }
        }
        assert_eq!(default_profiles_for(QualityFamily::Video), default_profiles());
    }

    #[test]
    fn ebook_ranking() {
        let any = &default_profiles_for(QualityFamily::Ebook)[0];
        let r = |q| any.rank(q).unwrap();
        assert!(r(Quality::Epub) > r(Quality::Azw3));
        assert!(r(Quality::Azw3) > r(Quality::Mobi));
        assert!(r(Quality::Mobi) > r(Quality::Pdf));
        assert!(any.is_allowed(Quality::UnknownText));
        assert!(any.cutoff_met(Quality::Epub, 0));
        assert!(!any.cutoff_met(Quality::Pdf, 0));
        let epub = &default_profiles_for(QualityFamily::Ebook)[1];
        assert!(epub.is_allowed(Quality::Azw3) && !epub.is_allowed(Quality::Mobi));
    }

    #[test]
    fn audiobook_prefers_m4b() {
        let any = &default_profiles_for(QualityFamily::Audiobook)[0];
        assert_eq!(any.best_allowed_rank(), any.rank(Quality::AudiobookM4b));
        assert!(any.rank(Quality::AudiobookMp3) > any.rank(Quality::AudiobookFlac));
        let m4b = &default_profiles_for(QualityFamily::Audiobook)[1];
        assert!(m4b.is_allowed(Quality::AudiobookM4b) && !m4b.is_allowed(Quality::AudiobookMp3));
    }

    #[test]
    fn music_lossless_groups() {
        let profiles = default_profiles_for(QualityFamily::Music);
        let lossless = profiles.iter().find(|p| p.name == "Music: Lossless").unwrap();
        assert_eq!(lossless.rank(Quality::Alac), lossless.rank(Quality::Flac));
        assert_eq!(lossless.rank(Quality::Alac24), lossless.rank(Quality::Flac24));
        assert!(lossless.rank(Quality::Flac24) > lossless.rank(Quality::Flac));
        assert!(lossless.rank(Quality::Flac) > lossless.rank(Quality::Wav));
        assert!(lossless.cutoff >= GROUP_ID_BASE);
        assert!(lossless.cutoff_met(Quality::Alac, 0));
        assert!(lossless.cutoff_met(Quality::Flac24, 0));
        assert!(!lossless.cutoff_met(Quality::Wav, 0));
        assert!(!lossless.is_allowed(Quality::Mp3_320));
        let standard = profiles.iter().find(|p| p.name == "Music: Standard").unwrap();
        assert!(standard.is_allowed(Quality::Mp3_192) && !standard.is_allowed(Quality::Mp3_128));
        assert!(!standard.is_allowed(Quality::Flac));
        let hq = profiles
            .iter()
            .find(|p| p.name == "Music: High Quality Lossy")
            .unwrap();
        assert!(hq.rank(Quality::Mp3_320) > hq.rank(Quality::Aac256));
        assert!(hq.rank(Quality::Mp3VbrV0) > hq.rank(Quality::Aac256));
    }

    #[test]
    fn profiles_only_hold_their_family() {
        let mut p = default_profiles_for(QualityFamily::Ebook)[0].clone();
        p.items.push(ProfileItem::quality(Quality::Flac, true));
        assert!(matches!(p.validate(), Err(DecisionError::InvalidProfile(m)) if m.contains("FLAC")));
        let mut p = default_profiles()[0].clone();
        p.items.push(ProfileItem::quality(Quality::Epub, false));
        assert!(p.validate().is_err());
        // A book profile mislabelled as video is rejected too.
        let mut p = default_profiles_for(QualityFamily::Music)[0].clone();
        p.family = QualityFamily::Video;
        assert!(p.validate().is_err());
        // Video JSON keeps its shape; other families carry `family`.
        assert!(
            !serde_json::to_string(&default_profiles()[0])
                .unwrap()
                .contains("family")
        );
        let json = serde_json::to_string(&default_profiles_for(QualityFamily::Music)[1]).unwrap();
        assert!(json.contains(r#""family":"music""#));
        let back: QualityProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, default_profiles_for(QualityFamily::Music)[1]);
    }
}
