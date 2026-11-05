//! The status rules, as pure functions shared by refresh, edits, matching and filing.
//!
//! - A show's follow choice decides whether an episode is monitored when it first appears
//!   ([`monitored_by_follow`]); changing the choice re-applies it to every episode.
//! - An episode's status comes from the matched file, monitoring and first broadcast
//!   ([`episode_status`]).

use chrono::{DateTime, Utc};
use reelix_api_types::radio::{RadioEpisodeStatus, RadioFollow, RadioSeriesKind};

/// Whether an episode is wanted under the show's follow choice.
///
/// `from_now_on` wants episodes first broadcast at or after `followed_at`; an episode without
/// a broadcast date is wanted only when it appears after the show was followed
/// (`discovered_after_follow`, false for the episodes listed when the show is added or the
/// choice changes).
pub fn monitored_by_follow(
    follow: RadioFollow,
    followed_at: Option<DateTime<Utc>>,
    first_broadcast: Option<DateTime<Utc>>,
    discovered_after_follow: bool,
) -> bool {
    match follow {
        RadioFollow::All => true,
        RadioFollow::None => false,
        RadioFollow::FromNowOn => match (first_broadcast, followed_at) {
            (Some(b), Some(f)) => b >= f,
            (None, _) => discovered_after_follow,
            (Some(_), None) => true,
        },
    }
}

/// In library (a file is matched) → Not requested (not monitored, or the show is not) → Not
/// released (broadcast in the future) → Wanted. An episode without a date counts as
/// broadcast.
pub fn episode_status(
    has_file: bool,
    show_monitored: bool,
    episode_monitored: bool,
    first_broadcast: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> RadioEpisodeStatus {
    if has_file {
        RadioEpisodeStatus::InLibrary
    } else if !(show_monitored && episode_monitored) {
        RadioEpisodeStatus::NotRequested
    } else if first_broadcast.is_some_and(|b| b > now) {
        RadioEpisodeStatus::NotReleased
    } else {
        RadioEpisodeStatus::Wanted
    }
}

/// The series number used for file names and tags: specials are series 0, an unnumbered
/// series is series 1.
pub fn filing_series_number(kind: RadioSeriesKind, number: Option<u32>) -> u32 {
    match kind {
        RadioSeriesKind::Specials => 0,
        RadioSeriesKind::Series => number.unwrap_or(1),
    }
}

/// Days after its last broadcast before a complete series is settled.
pub const SETTLE_AFTER_DAYS: i64 = 60;

/// A series is settled once it has all its expected episodes and the last one was broadcast
/// more than [`SETTLE_AFTER_DAYS`] ago; refresh then stops reading it.
pub fn is_settled(
    expected: Option<u32>,
    known: u32,
    last_broadcast: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    match (expected, last_broadcast) {
        (Some(n), Some(last)) => {
            n > 0 && known >= n && now - last > chrono::Duration::days(SETTLE_AFTER_DAYS)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn t(d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 1, d, 12, 0, 0).unwrap()
    }

    #[test]
    fn follow_choices() {
        assert!(monitored_by_follow(RadioFollow::All, None, None, false));
        assert!(!monitored_by_follow(
            RadioFollow::None,
            Some(t(1)),
            Some(t(5)),
            true
        ));
        let f = Some(t(10));
        assert!(!monitored_by_follow(RadioFollow::FromNowOn, f, Some(t(9)), true));
        assert!(monitored_by_follow(RadioFollow::FromNowOn, f, Some(t(10)), false));
        assert!(!monitored_by_follow(RadioFollow::FromNowOn, f, None, false));
        assert!(monitored_by_follow(RadioFollow::FromNowOn, f, None, true));
    }

    #[test]
    fn statuses() {
        use RadioEpisodeStatus::*;
        let now = t(10);
        assert_eq!(episode_status(true, false, false, None, now), InLibrary);
        assert_eq!(episode_status(false, true, false, Some(t(1)), now), NotRequested);
        assert_eq!(episode_status(false, false, true, Some(t(1)), now), NotRequested);
        assert_eq!(episode_status(false, true, true, Some(t(11)), now), NotReleased);
        assert_eq!(episode_status(false, true, true, Some(t(10)), now), Wanted);
        assert_eq!(episode_status(false, true, true, None, now), Wanted);
    }

    #[test]
    fn settling() {
        let now = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();
        assert!(is_settled(Some(6), 6, Some(t(1)), now));
        assert!(!is_settled(Some(6), 5, Some(t(1)), now));
        assert!(!is_settled(None, 6, Some(t(1)), now));
        assert!(!is_settled(
            Some(6),
            6,
            Some(now - chrono::Duration::days(30)),
            now
        ));
        assert_eq!(filing_series_number(RadioSeriesKind::Specials, Some(3)), 0);
        assert_eq!(filing_series_number(RadioSeriesKind::Series, None), 1);
    }
}
