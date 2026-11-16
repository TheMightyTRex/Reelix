//! Links between episodes and library files: by hand (`PUT`/`DELETE /radio/episodes/{id}/file`)
//! and automatically after each library sync ([`RadioService::match_library`]).
//!
//! A hand-set link (`manual_match`) is kept by automatic matching, and its file is not offered
//! to other episodes. Clearing a link by hand ("Not this file") also sets `manual_match`, with
//! no file, so automatic matching leaves the episode alone; clearing with `auto` hands the
//! episode back to automatic matching.
//!
//! Automatic matching gives each episode at most one file and each file at most one episode,
//! trying in order:
//!
//! 1. **The source's id**: the episode's guid or enclosure address equals the file's, or one
//!    of the episode's [`source_ids`] (for the BBC, the programme pid) appears in the file's
//!    guid, enclosure address or file name.
//! 2. **Series and episode number** (`S20E03` in the file's title or file name, else
//!    Audiobookshelf's season and episode; no season counts as series 1), among the files of
//!    the show's Audiobookshelf item (`abs_item_id`) or, when it has none, of the items whose
//!    title matches the show's ([`clean_title`](crate::clean_title)). A number shared by two
//!    files or two episodes matches nothing.
//! 3. **Episode title**, only when exactly one file and one episode in the whole library share
//!    it.
//!
//! A show with no `abs_item_id` gets one when its matched files and title-matched items are
//! all one Audiobookshelf item that no other show uses.

use std::collections::{HashMap, HashSet};

use reelix_api_types::radio::{RadioEpisodeDto, RadioEpisodeFileDto, RadioShowDto, RadioShowEdit};

use crate::convert::clean_title;
use crate::error::{RadioError, Result};
use crate::status::filing_series_number;
use crate::{RadioService, entities, repo};

/// Ids of an episode that its library file may carry in its guid, enclosure address or file
/// name, per provider (`provider` is `radio_shows.provider`, `external_id` the episode's).
/// Providers whose episode ids are found verbatim in files add an arm here; ids are compared
/// lower case as whole words (between non-alphanumeric characters).
pub fn source_ids(provider: &str, external_id: &str) -> Vec<String> {
    match provider {
        // Programme pids: `b0abc123`, `p0abc123`, `m001abcd`, `w3ct1234`.
        "bbc" => external_id
            .split(|c: char| !c.is_ascii_alphanumeric())
            .map(str::to_ascii_lowercase)
            .filter(|t| is_bbc_pid(t))
            .collect(),
        // Internet Archive: the external id is the audio file's name in the item; its stem
        // (long and distinctive enough not to match by chance) in a file name.
        "archive" => {
            let name = external_id.rsplit('/').next().unwrap_or(external_id);
            let stem = name.rsplit_once('.').map_or(name, |(s, _)| s).to_lowercase();
            if stem.chars().count() >= 8 && stem.chars().any(char::is_alphabetic) {
                vec![stem]
            } else {
                Vec::new()
            }
        }
        // Feeds, Apple Podcasts (saved as feeds) and Podcast Index: the external id is the
        // guid or enclosure address, compared exactly.
        _ => Vec::new(),
    }
}

/// A BBC programme id: a letter, a digit and six letters or digits.
fn is_bbc_pid(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() == 8
        && matches!(b[0], b'b' | b'p' | b'm' | b'w')
        && b[1].is_ascii_digit()
        && b[2..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// Whether `needle` (lower case) occurs in `haystack` as a whole word.
fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hay = haystack.to_lowercase();
    let mut from = 0;
    while let Some(at) = hay[from..].find(needle) {
        let start = from + at;
        let end = start + needle.len();
        let before = hay[..start].chars().next_back();
        let after = hay[end..].chars().next();
        if !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric) {
            return true;
        }
        from = start + needle.chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// The last component of a path (either separator).
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// `S20E03` (also `s20e03`, `S20 E03`, `S20.E03`) as `(20, 3)`.
pub(crate) fn parse_sxxeyy(s: &str) -> Option<(u32, u32)> {
    let b = s.as_bytes();
    let digits = |from: usize| -> (usize, Option<u32>) {
        let mut end = from;
        while end < b.len() && b[end].is_ascii_digit() && end - from < 4 {
            end += 1;
        }
        (end, s[from..end].parse().ok())
    };
    for i in 0..b.len() {
        if !b[i].eq_ignore_ascii_case(&b's') || (i > 0 && b[i - 1].is_ascii_alphanumeric()) {
            continue;
        }
        let (mut j, Some(season)) = digits(i + 1) else {
            continue;
        };
        if j < b.len() && matches!(b[j], b' ' | b'.' | b'_' | b'-') {
            j += 1;
        }
        if j >= b.len() || !b[j].eq_ignore_ascii_case(&b'e') {
            continue;
        }
        let (k, Some(episode)) = digits(j + 1) else {
            continue;
        };
        if k < b.len() && b[k].is_ascii_digit() {
            continue;
        }
        return Some((season, episode));
    }
    None
}

/// A file's `(series, episode)` for rule 2.
fn file_number(f: &RadioEpisodeFileDto) -> Option<(u32, u32)> {
    f.title
        .as_deref()
        .and_then(parse_sxxeyy)
        .or_else(|| f.path.as_deref().map(file_name).and_then(parse_sxxeyy))
        .or_else(|| f.episode.map(|e| (f.season.unwrap_or(1), e)))
}

/// Whether one of `ids` ([`source_ids`]) appears in the file's guid, enclosure address or
/// file name.
fn carries_id(f: &RadioEpisodeFileDto, ids: &[String]) -> bool {
    let fields = [
        f.guid.as_deref(),
        f.enclosure_url.as_deref(),
        f.path.as_deref().map(file_name),
    ];
    ids.iter()
        .any(|id| fields.iter().flatten().any(|h| contains_word(h, id)))
}

/// Keys that occur exactly once.
fn unique<K: std::hash::Hash + Eq + Clone, V: Copy>(pairs: impl Iterator<Item = (K, V)>) -> HashMap<K, V> {
    let mut seen: HashMap<K, Option<V>> = HashMap::new();
    for (k, v) in pairs {
        seen.entry(k).and_modify(|x| *x = None).or_insert(Some(v));
    }
    seen.into_iter().filter_map(|(k, v)| Some((k, v?))).collect()
}

/// The pure part of matching: which file each episode should have.
pub(crate) struct Matcher<'a> {
    pub shows: &'a [(RadioShowDto, String)],
    pub episodes: &'a [RadioEpisodeDto],
    pub files: &'a [RadioEpisodeFileDto],
}

impl Matcher<'_> {
    /// `episode id → file id` for episodes automatic matching may set (none set by hand),
    /// with files not linked by hand.
    pub fn assign(&self) -> HashMap<i64, i64> {
        let manual_files: HashSet<i64> = self
            .episodes
            .iter()
            .filter(|e| e.manual_match)
            .filter_map(|e| e.file_id)
            .collect();
        let files: Vec<&RadioEpisodeFileDto> = self
            .files
            .iter()
            .filter(|f| !manual_files.contains(&f.id))
            .collect();
        let eps: Vec<&RadioEpisodeDto> = self.episodes.iter().filter(|e| !e.manual_match).collect();
        let show_of: HashMap<i64, &(RadioShowDto, String)> = self.shows.iter().map(|s| (s.0.id, s)).collect();
        let mut out: HashMap<i64, i64> = HashMap::new();
        let mut used: HashSet<i64> = HashSet::new();

        // 1. The source's id.
        let index = |field: fn(&RadioEpisodeFileDto) -> Option<&str>| {
            let mut m: HashMap<&str, Vec<i64>> = HashMap::new();
            for f in &files {
                if let Some(v) = field(f).map(str::trim).filter(|v| !v.is_empty()) {
                    m.entry(v).or_default().push(f.id);
                }
            }
            m
        };
        let by_guid = index(|f| f.guid.as_deref());
        let by_enclosure = index(|f| f.enclosure_url.as_deref());
        for e in &eps {
            let Some((show, _)) = show_of.get(&e.show_id) else {
                continue;
            };
            let exact = |m: &HashMap<&str, Vec<i64>>, v: Option<&str>| {
                v.map(str::trim)
                    .and_then(|v| m.get(v))
                    .and_then(|ids| ids.iter().copied().find(|f| !used.contains(f)))
            };
            let found = exact(&by_guid, e.guid.as_deref())
                .or_else(|| exact(&by_enclosure, e.enclosure_url.as_deref()))
                .or_else(|| {
                    let ids = source_ids(&show.provider, &e.external_id);
                    if ids.is_empty() {
                        return None;
                    }
                    files
                        .iter()
                        .find(|f| !used.contains(&f.id) && carries_id(f, &ids))
                        .map(|f| f.id)
                });
            if let Some(f) = found {
                out.insert(e.id, f);
                used.insert(f);
            }
        }

        // 2. Series and episode number within the show's library item(s).
        for (show, key) in self.shows {
            let items: HashSet<&str> = match show.abs_item_id.as_deref() {
                Some(item) => HashSet::from([item]),
                None => files
                    .iter()
                    .filter(|f| {
                        !key.is_empty() && f.item_title.as_deref().map(clean_title).as_ref() == Some(key)
                    })
                    .map(|f| f.abs_item_id.as_str())
                    .collect(),
            };
            if items.is_empty() {
                continue;
            }
            let by_number = unique(
                files
                    .iter()
                    .filter(|f| !used.contains(&f.id) && items.contains(f.abs_item_id.as_str()))
                    .filter_map(|f| Some((file_number(f)?, f.id))),
            );
            let wanted = unique(
                eps.iter()
                    .filter(|e| e.show_id == show.id && !out.contains_key(&e.id))
                    .filter_map(|e| {
                        let n = e.number?;
                        Some(((filing_series_number(e.series_kind, e.series_number), n), e.id))
                    }),
            );
            let mut pairs: Vec<(i64, i64)> = wanted
                .iter()
                .filter_map(|(k, e)| Some((*e, *by_number.get(k)?)))
                .collect();
            pairs.sort_unstable();
            for (e, f) in pairs {
                out.insert(e, f);
                used.insert(f);
            }
        }

        // 3. A title exactly one file and one episode share (counted over the whole library).
        let title_key = |t: Option<&str>| t.map(clean_title).filter(|k| !k.is_empty());
        let file_titles = unique(
            self.files
                .iter()
                .filter_map(|f| Some((title_key(f.title.as_deref())?, f.id))),
        );
        let ep_titles = unique(
            self.episodes
                .iter()
                .filter_map(|e| Some((title_key(Some(&e.title))?, e.id))),
        );
        let open_eps: HashSet<i64> = eps.iter().map(|e| e.id).collect();
        let mut pairs: Vec<(i64, i64)> = ep_titles
            .iter()
            .filter_map(|(k, e)| Some((*e, *file_titles.get(k)?)))
            .filter(|(e, f)| {
                open_eps.contains(e) && !out.contains_key(e) && !used.contains(f) && !manual_files.contains(f)
            })
            .collect();
        pairs.sort_unstable();
        for (e, f) in pairs {
            out.insert(e, f);
            used.insert(f);
        }
        out
    }
}

impl RadioService {
    /// Links `file_id` to the episode by hand. A file linked automatically to another episode
    /// moves here; one linked by hand elsewhere is refused.
    pub async fn set_episode_file(&self, episode_id: i64, file_id: i64) -> Result<RadioEpisodeDto> {
        let ep = repo::episodes::require(&self.db, episode_id).await?;
        repo::files::get(&self.db, file_id)
            .await?
            .ok_or_else(|| RadioError::Validation(format!("library file {file_id} does not exist")))?;
        let mut shows: HashSet<i64> = HashSet::from([ep.show_id]);
        if let Some(other) = repo::episodes::for_file(&self.db, file_id).await?
            && other.id != episode_id
        {
            if other.manual_match {
                return Err(RadioError::Conflict(format!(
                    "that file is matched by hand to “{}”; clear that match first",
                    other.title
                )));
            }
            repo::episodes::set_file(&self.db, other.id, None, false).await?;
            shows.insert(other.show_id);
            self.emit(entities::EPISODE, "updated", other.id);
        }
        repo::episodes::set_file(&self.db, episode_id, Some(file_id), true).await?;
        for s in shows {
            self.recompute_statuses(s).await?;
        }
        self.emit(entities::EPISODE, "updated", episode_id);
        self.get_episode(episode_id).await
    }

    /// Clears the episode's file. `auto = false` ("Not this file") keeps it unmatched until
    /// changed by hand; `auto = true` ("Match automatically again") lets the next library sync
    /// match it. The status returns to Wanted or Not requested from the follow choice.
    pub async fn clear_episode_file(&self, episode_id: i64, auto: bool) -> Result<RadioEpisodeDto> {
        let ep = repo::episodes::require(&self.db, episode_id).await?;
        repo::episodes::set_file(&self.db, episode_id, None, !auto).await?;
        self.recompute_statuses(ep.show_id).await?;
        self.emit(entities::EPISODE, "updated", episode_id);
        self.get_episode(episode_id).await
    }

    /// Matches library files to episodes (rules in the [module docs](self)) and fills in
    /// missing `abs_item_id`s. Hand-set links and "Not this file" are left alone; an automatic
    /// link that no rule gives to another episode is kept. Returns the number of new links.
    pub async fn match_library(&self) -> Result<u32> {
        // Custom shows make their episodes from their files first (RFX-90).
        self.sync_custom_shows().await?;
        let keys: HashMap<i64, String> = repo::shows::match_keys(&self.db)
            .await?
            .into_iter()
            .map(|(id, key, _)| (id, key))
            .collect();
        let shows: Vec<(RadioShowDto, String)> = repo::shows::load(&self.db, None)
            .await?
            .into_iter()
            .map(|s| {
                let key = keys.get(&s.id).cloned().unwrap_or_default();
                (s, key)
            })
            .collect();
        let mut episodes = Vec::new();
        for (s, _) in &shows {
            episodes.extend(repo::episodes::for_show(&self.db, s.id).await?);
        }
        let files = repo::files::all(&self.db).await?;
        let wanted = Matcher {
            shows: &shows,
            episodes: &episodes,
            files: &files,
        }
        .assign();

        // Apply: current file → episode, so a moved file is first taken off its old episode.
        let mut holder: HashMap<i64, i64> =
            episodes.iter().filter_map(|e| Some((e.file_id?, e.id))).collect();
        let current: HashMap<i64, Option<i64>> = episodes.iter().map(|e| (e.id, e.file_id)).collect();
        let show_of: HashMap<i64, i64> = episodes.iter().map(|e| (e.id, e.show_id)).collect();
        let mut pairs: Vec<(i64, i64)> = wanted.into_iter().collect();
        pairs.sort_unstable();
        let mut touched: HashSet<i64> = HashSet::new();
        let mut linked = 0u32;
        for (e, f) in pairs {
            if current.get(&e).copied().flatten() == Some(f) && holder.get(&f) == Some(&e) {
                continue;
            }
            if let Some(other) = holder.remove(&f)
                && other != e
            {
                repo::episodes::set_file(&self.db, other, None, false).await?;
                self.emit(entities::EPISODE, "updated", other);
                touched.extend(show_of.get(&other));
            }
            holder.retain(|_, ep| *ep != e);
            repo::episodes::set_file(&self.db, e, Some(f), false).await?;
            holder.insert(f, e);
            self.emit(entities::EPISODE, "updated", e);
            touched.extend(show_of.get(&e));
            linked += 1;
        }
        let mut touched: Vec<i64> = touched.into_iter().collect();
        touched.sort_unstable();
        for s in touched {
            self.recompute_statuses(s).await?;
        }

        // Shows matched to exactly one library item get it as their `abs_item_id`.
        let item_of: HashMap<i64, &str> = files.iter().map(|f| (f.id, f.abs_item_id.as_str())).collect();
        let mut taken: HashSet<String> = shows.iter().filter_map(|(s, _)| s.abs_item_id.clone()).collect();
        for (show, key) in &shows {
            if show.abs_item_id.is_some() {
                continue;
            }
            let mut items: HashSet<&str> = holder
                .iter()
                .filter(|(_, e)| show_of.get(e) == Some(&show.id))
                .filter_map(|(f, _)| item_of.get(f).copied())
                .collect();
            if !key.is_empty() {
                items.extend(
                    files
                        .iter()
                        .filter(|f| f.item_title.as_deref().map(clean_title).as_ref() == Some(key))
                        .map(|f| f.abs_item_id.as_str()),
                );
            }
            if items.len() == 1
                && let Some(item) = items.into_iter().next()
                && taken.insert(item.to_owned())
            {
                self.update_show(
                    show.id,
                    RadioShowEdit {
                        abs_item_id: Some(Some(item.to_owned())),
                        ..RadioShowEdit::default()
                    },
                )
                .await?;
            }
        }
        Ok(linked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_episode_numbers() {
        assert_eq!(parse_sxxeyy("The Archers S20E03 Ambridge"), Some((20, 3)));
        assert_eq!(parse_sxxeyy("show - s1e12.mp3"), Some((1, 12)));
        assert_eq!(parse_sxxeyy("S02 E07"), Some((2, 7)));
        assert_eq!(parse_sxxeyy("Seasons S3"), None);
        assert_eq!(parse_sxxeyy("XS1E2"), None);
        assert_eq!(parse_sxxeyy("S1E23456"), None);
        assert_eq!(parse_sxxeyy("no numbers"), None);
    }

    #[test]
    fn bbc_pids_and_words() {
        assert_eq!(source_ids("bbc", "b0abc123"), ["b0abc123"]);
        assert_eq!(source_ids("bbc", "P0ABC123"), ["p0abc123"]);
        assert_eq!(source_ids("bbc", "episode:m001abcd"), ["m001abcd"]);
        assert!(source_ids("bbc", "archers").is_empty());
        assert!(source_ids("rss", "b0abc123").is_empty());
        assert_eq!(
            source_ids("archive", "Jeff_Regan_49-10-15_The_Lute.mp3"),
            ["jeff_regan_49-10-15_the_lute"]
        );
        assert!(source_ids("archive", "01.mp3").is_empty());
        assert!(contains_word(
            "/podcasts/Jeff Regan/Jeff_Regan_49-10-15_The_Lute.mp3",
            "jeff_regan_49-10-15_the_lute"
        ));
        assert!(contains_word("https://x/b0abc123.mp3", "b0abc123"));
        assert!(contains_word("Show_B0ABC123_x", "b0abc123"));
        assert!(contains_word("Show - b0abc123 - x", "b0abc123"));
        assert!(!contains_word("xb0abc123", "b0abc123"));
        assert!(!contains_word("b0abc1234", "b0abc123"));
        assert_eq!(file_name("/a/b/c.mp3"), "c.mp3");
        assert_eq!(file_name("C:\\a\\c.mp3"), "c.mp3");
    }
}
