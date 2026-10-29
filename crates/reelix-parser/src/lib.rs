//! Release-name parsing and the quality/language model shared across Reelix.
//!
//! The parser is pure and synchronous: no I/O, and it must never panic.
//!
//! ```
//! let p = reelix_parser::parse("Show.Name.S02E05.1080p.WEB-DL.DDP5.1.Atmos.H.264-GROUP");
//! assert_eq!(p.title, "Show Name");
//! assert_eq!(p.quality.quality, reelix_parser::Quality::WebDl1080p);
//! assert_eq!(p.release_group.as_deref(), Some("GROUP"));
//! ```

/// Declares a lazily compiled static regex. Patterns are compile-time constants.
macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new($pat).expect("static regex is valid"));
    };
}

pub mod book;
mod episode;
mod group;
pub mod model;
pub mod music;
mod parser;
mod path;
mod tags;
mod title;

pub use book::{ParsedBook, book_quality_from_extension, parse_book};
pub use model::*;
pub use music::{MusicSource, ParsedMusic, music_quality, parse_music};
pub use parser::parse;
pub use path::parse_path;
pub use title::clean_title;
