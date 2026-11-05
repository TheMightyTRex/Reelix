//! Soulseek releases as found through slskd: one peer's folder of files.
//!
//! Remote paths are the peer's own (`@@abcde\Music\Artist\Album\01 - Track.flac` from Windows
//! peers, `/` from others); both separators are accepted everywhere.

use serde::{Deserialize, Serialize};

/// One shared file of a peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoulseekFile {
    /// Full remote path, exactly as slskd reports it (sent back unchanged to enqueue).
    pub filename: String,
    pub size: u64,
    /// kbps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
    /// Hz.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// Seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_variable_bit_rate: Option<bool>,
}

impl SoulseekFile {
    /// File name without the remote directory.
    pub fn name(&self) -> &str {
        split_remote(&self.filename).1
    }

    /// Lower-case extension without the dot (empty when there is none).
    pub fn extension(&self) -> String {
        match self.name().rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
            _ => String::new(),
        }
    }
}

/// A folder of one peer, grabbed as a unit (the slskd client enqueues every file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoulseekRelease {
    pub username: String,
    /// Remote directory holding `files`.
    pub directory: String,
    pub files: Vec<SoulseekFile>,
    #[serde(default)]
    pub has_free_upload_slot: bool,
    /// Uploads the peer has queued ahead of us.
    #[serde(default)]
    pub queue_length: u32,
    /// Bytes per second the peer reports.
    #[serde(default)]
    pub upload_speed: u64,
}

impl SoulseekRelease {
    /// Download id of the folder in the slskd client: `{username}/{directory}`.
    pub fn download_id(&self) -> String {
        download_id(&self.username, &self.directory)
    }

    /// Stable identity across searches (guid, blocklist key): `slsk:{username}/{directory}`.
    pub fn key(&self) -> String {
        format!("slsk:{}", self.download_id())
    }

    /// Last component of the remote directory (the folder slskd creates locally).
    pub fn folder_name(&self) -> &str {
        last_component(&self.directory)
    }

    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// `{username}/{directory}`; see [`parse_download_id`].
pub fn download_id(username: &str, directory: &str) -> String {
    format!("{username}/{directory}")
}

/// Splits a [`download_id`] at the first `/` (slskd routes put the username in a path
/// segment, so it never contains one).
pub fn parse_download_id(id: &str) -> Option<(&str, &str)> {
    id.split_once('/').filter(|(u, d)| !u.is_empty() && !d.is_empty())
}

/// `(directory, file name)` of a remote path, split at the last `\` or `/`.
pub fn split_remote(path: &str) -> (&str, &str) {
    match path.rfind(['\\', '/']) {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// Last non-empty component of a remote directory.
pub fn last_component(dir: &str) -> &str {
    dir.trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths_and_ids() {
        assert_eq!(
            split_remote(r"@@abcde\Music\Artist\Album\01 - Track.flac"),
            (r"@@abcde\Music\Artist\Album", "01 - Track.flac")
        );
        assert_eq!(split_remote("music/Album/a.mp3"), ("music/Album", "a.mp3"));
        assert_eq!(split_remote("a.mp3"), ("", "a.mp3"));
        assert_eq!(last_component(r"@@abcde\Music\Album\"), "Album");
        let r = SoulseekRelease {
            username: "peer".into(),
            directory: r"Music\Album".into(),
            files: vec![SoulseekFile {
                filename: r"Music\Album\01.FLAC".into(),
                size: 10,
                bit_rate: None,
                bit_depth: None,
                sample_rate: None,
                length: None,
                is_variable_bit_rate: None,
            }],
            has_free_upload_slot: true,
            queue_length: 0,
            upload_speed: 0,
        };
        assert_eq!(r.key(), r"slsk:peer/Music\Album");
        assert_eq!(
            parse_download_id(&r.download_id()),
            Some(("peer", r"Music\Album"))
        );
        assert_eq!(parse_download_id("nouser"), None);
        assert_eq!((r.folder_name(), r.total_size()), ("Album", 10));
        assert_eq!(r.files[0].extension(), "flac");
    }
}
