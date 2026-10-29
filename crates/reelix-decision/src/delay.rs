//! Delay profiles: prefer one protocol and wait a while for a better release.

use reelix_core::Protocol;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelayProfile {
    #[serde(default)]
    pub id: i64,
    pub enable_usenet: bool,
    pub enable_torrent: bool,
    pub preferred_protocol: Protocol,
    /// Minutes to wait after a usenet release is first seen.
    #[serde(default)]
    pub usenet_delay: u32,
    #[serde(default)]
    pub torrent_delay: u32,
    /// Skip the delay when the release is the profile's best allowed quality.
    #[serde(default)]
    pub bypass_if_highest_quality: bool,
    /// Skip the delay when the custom-format score reaches `minimum_custom_format_score`.
    #[serde(default)]
    pub bypass_if_above_custom_format_score: bool,
    #[serde(default)]
    pub minimum_custom_format_score: i32,
    /// Tags this profile applies to; empty = the default profile (applies to everything).
    #[serde(default)]
    pub tags: Vec<i64>,
    /// Lower runs first when several tagged profiles match.
    #[serde(default)]
    pub order: i32,
}

impl Default for DelayProfile {
    fn default() -> Self {
        Self {
            id: 0,
            enable_usenet: true,
            enable_torrent: true,
            preferred_protocol: Protocol::Usenet,
            usenet_delay: 0,
            torrent_delay: 0,
            bypass_if_highest_quality: true,
            bypass_if_above_custom_format_score: false,
            minimum_custom_format_score: 0,
            tags: Vec::new(),
            order: i32::MAX,
        }
    }
}

impl DelayProfile {
    pub fn delay_for(&self, protocol: Protocol) -> u32 {
        match protocol {
            Protocol::Usenet => self.usenet_delay,
            Protocol::Torrent => self.torrent_delay,
        }
    }

    pub fn allows(&self, protocol: Protocol) -> bool {
        match protocol {
            Protocol::Usenet => self.enable_usenet,
            Protocol::Torrent => self.enable_torrent,
        }
    }

    /// The profile that applies to media with `tags`: the first (by `order`) tagged profile
    /// sharing a tag, else the untagged default.
    pub fn select<'a>(profiles: &'a [DelayProfile], tags: &[i64]) -> Option<&'a DelayProfile> {
        let mut tagged: Vec<&DelayProfile> = profiles
            .iter()
            .filter(|p| !p.tags.is_empty() && p.tags.iter().any(|t| tags.contains(t)))
            .collect();
        tagged.sort_by_key(|p| p.order);
        tagged
            .first()
            .copied()
            .or_else(|| profiles.iter().find(|p| p.tags.is_empty()))
    }
}
