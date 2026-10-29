//! Core domain types shared by every Reelix crate, including the wasm frontend.

#[cfg(feature = "bus")]
pub mod bus;
pub mod events;
pub mod jobs;
pub mod permissions;
pub mod provider;
pub mod user;

pub use events::{Audience, DomainEvent};
pub use permissions::Permissions;

/// Download protocol of a release, indexer or download client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Torrent,
    Usenet,
}

/// Movie or series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Movie,
    Series,
}

/// Every kind of content Reelix manages. Superset of [`MediaKind`] for the places books and
/// music share with movies/TV (root folders, download categories, media servers, notifications).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentKind {
    Movie,
    Series,
    Ebook,
    Audiobook,
    Music,
}

impl ContentKind {
    pub const ALL: [ContentKind; 5] = [
        Self::Movie,
        Self::Series,
        Self::Ebook,
        Self::Audiobook,
        Self::Music,
    ];

    /// DB / wire string: `movie`, `series`, `ebook`, `audiobook`, `music`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Series => "series",
            Self::Ebook => "ebook",
            Self::Audiobook => "audiobook",
            Self::Music => "music",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// `Some` for Movie/Series only.
    pub fn media_kind(self) -> Option<MediaKind> {
        match self {
            Self::Movie => Some(MediaKind::Movie),
            Self::Series => Some(MediaKind::Series),
            Self::Ebook | Self::Audiobook | Self::Music => None,
        }
    }

    /// Ebook or audiobook.
    pub fn is_book(self) -> bool {
        self.book_format().is_some()
    }

    pub fn book_format(self) -> Option<BookFormat> {
        match self {
            Self::Ebook => Some(BookFormat::Ebook),
            Self::Audiobook => Some(BookFormat::Audiobook),
            Self::Movie | Self::Series | Self::Music => None,
        }
    }
}

impl From<MediaKind> for ContentKind {
    fn from(kind: MediaKind) -> Self {
        match kind {
            MediaKind::Movie => Self::Movie,
            MediaKind::Series => Self::Series,
        }
    }
}

/// The two formats of a book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BookFormat {
    Ebook,
    Audiobook,
}

impl BookFormat {
    pub const ALL: [BookFormat; 2] = [Self::Ebook, Self::Audiobook];

    /// `ebook` | `audiobook`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ebook => "ebook",
            Self::Audiobook => "audiobook",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == s)
    }

    pub fn content_kind(self) -> ContentKind {
        match self {
            Self::Ebook => ContentKind::Ebook,
            Self::Audiobook => ContentKind::Audiobook,
        }
    }
}
pub use user::{AuthSource, User};

/// Application version, embedded at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_kind_round_trips() {
        for k in ContentKind::ALL {
            assert_eq!(ContentKind::parse(k.as_str()), Some(k));
            assert_eq!(serde_json::to_string(&k).unwrap(), format!("\"{}\"", k.as_str()));
        }
        assert_eq!(ContentKind::parse("book"), None);
        assert_eq!(ContentKind::from(MediaKind::Series), ContentKind::Series);
        assert_eq!(ContentKind::Movie.media_kind(), Some(MediaKind::Movie));
        assert_eq!(ContentKind::Music.media_kind(), None);
        assert!(ContentKind::Audiobook.is_book() && !ContentKind::Music.is_book());
    }

    #[test]
    fn book_format_round_trips() {
        for f in BookFormat::ALL {
            assert_eq!(BookFormat::parse(f.as_str()), Some(f));
            assert_eq!(f.content_kind().book_format(), Some(f));
        }
    }
}
