//! Book naming tokens rendered with [`reelix_import::naming::render_custom`].
//!
//! Tokens: `{Author Name}`, `{Author SortName}`, `{Book Title}`, `{Book CleanTitle}`,
//! `{Book SeriesTitle}`, `{Book SeriesPosition}`, `{Release Year}`, `{Edition Language}`,
//! `{Quality Title}`, `{Part:00}`, `{PartCount}`, `{Narrator}`.

use std::path::Path;

use reelix_api_types::books::{AuthorDto, BookDto, BookNamingDto};
use reelix_core::BookFormat;
use reelix_import::naming::{NamingConfig, pad_number, render_custom, unknown_custom_tokens};
use reelix_parser::Quality;

/// Every token key (lowercased, as the template parser reports them).
pub(crate) const TOKENS: &[&str] = &[
    "author name",
    "author sortname",
    "book title",
    "book cleantitle",
    "book seriestitle",
    "book seriesposition",
    "release year",
    "edition language",
    "quality title",
    "part",
    "partcount",
    "narrator",
];

/// Facts the tokens are rendered from.
#[derive(Debug, Clone, Default)]
pub(crate) struct BookNaming {
    pub author_name: String,
    pub author_sort_name: String,
    pub title: String,
    pub series_title: Option<String>,
    pub series_position: Option<String>,
    pub year: Option<i32>,
    pub language: Option<String>,
    pub quality: Option<Quality>,
    pub narrator: Option<String>,
    /// 1-based part number and part count (multi-file audiobooks).
    pub part: Option<(u32, u32)>,
}

impl BookNaming {
    pub fn new(author: &AuthorDto, book: &BookDto) -> Self {
        let series = book.series.first();
        Self {
            author_name: author.name.clone(),
            author_sort_name: author.sort_name.clone(),
            title: book.title.clone(),
            series_title: series.map(|s| s.title.clone()),
            series_position: series.and_then(|s| s.position.clone()),
            year: book.year,
            language: book.language.clone(),
            quality: None,
            narrator: None,
            part: None,
        }
    }

    fn resolve(&self, key: &str, spec: Option<&str>) -> Option<String> {
        Some(match key {
            "author name" => self.author_name.clone(),
            "author sortname" => self.author_sort_name.clone(),
            "book title" => self.title.clone(),
            "book cleantitle" => reelix_import::naming::clean_title(&self.title),
            "book seriestitle" => self.series_title.clone().unwrap_or_default(),
            "book seriesposition" => self.series_position.clone().unwrap_or_default(),
            "release year" => self.year.map(|y| y.to_string()).unwrap_or_default(),
            "edition language" => self.language.clone().unwrap_or_default(),
            "quality title" => self.quality.map(|q| q.name().to_string()).unwrap_or_default(),
            "part" => self.part.map(|(n, _)| pad_number(n, spec)).unwrap_or_default(),
            "partcount" => self.part.map(|(_, c)| c.to_string()).unwrap_or_default(),
            "narrator" => self.narrator.clone().unwrap_or_default(),
            _ => return None,
        })
    }

    pub fn render(&self, template: &str, config: &NamingConfig) -> String {
        render_custom(template, &|k, s| self.resolve(k, s), config)
    }
}

/// Unknown tokens of a book template.
pub(crate) fn unknown_tokens(template: &str) -> Vec<String> {
    unknown_custom_tokens(template, &|k| TOKENS.contains(&k))
}

/// Folder name of an author (falls back to the author's name when the template renders empty).
pub(crate) fn author_folder(n: &BookNamingDto, author: &AuthorDto, config: &NamingConfig) -> String {
    let naming = BookNaming {
        author_name: author.name.clone(),
        author_sort_name: author.sort_name.clone(),
        ..Default::default()
    };
    let f = naming.render(&n.author_folder, config);
    if f.is_empty() {
        reelix_import::naming::sanitize(&author.name, config)
    } else {
        f
    }
}

fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn with_ext(stem: &str, ext: &str) -> String {
    if ext.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}.{ext}")
    }
}

fn non_empty(s: String, fallback: &str) -> String {
    if s.trim().is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

/// Relative destination paths (relative to the author's folder of `format`) of the files of one
/// book. `sources` are the source files in part order. Returns `(relative_folder_or_file,
/// relative part paths)`; the part list is empty for single-file books.
pub(crate) fn destination(
    n: &BookNamingDto,
    naming: &BookNaming,
    format: BookFormat,
    sources: &[&Path],
    config: &NamingConfig,
) -> (String, Vec<String>) {
    let original_stem = |p: &Path| {
        p.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    match format {
        BookFormat::Ebook => {
            let src = sources.first().copied().unwrap_or(Path::new(""));
            let stem = if n.rename {
                non_empty(naming.render(&n.ebook_file, config), &naming.title)
            } else {
                original_stem(src)
            };
            (with_ext(&stem, &ext_of(src)), Vec::new())
        }
        BookFormat::Audiobook => {
            let folder = if n.rename || sources.len() > 1 {
                non_empty(
                    naming.render(&n.audiobook_folder, config),
                    &reelix_import::naming::sanitize(&naming.title, config),
                )
            } else {
                non_empty(
                    reelix_import::naming::sanitize(&naming.title, config),
                    "Audiobook",
                )
            };
            if sources.len() <= 1 {
                let src = sources.first().copied().unwrap_or(Path::new(""));
                let stem = if n.rename {
                    non_empty(naming.render(&n.audiobook_file, config), &naming.title)
                } else {
                    original_stem(src)
                };
                return (format!("{folder}/{}", with_ext(&stem, &ext_of(src))), Vec::new());
            }
            let count = sources.len() as u32;
            let parts = sources
                .iter()
                .enumerate()
                .map(|(i, src)| {
                    let stem = if n.rename {
                        let mut p = naming.clone();
                        p.part = Some((i as u32 + 1, count));
                        non_empty(
                            p.render(&n.audiobook_part_file, config),
                            &format!("Part {:02}", i + 1),
                        )
                    } else {
                        original_stem(src)
                    };
                    format!("{folder}/{}", with_ext(&stem, &ext_of(src)))
                })
                .collect();
            (folder, parts)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naming() -> BookNaming {
        BookNaming {
            author_name: "Brandon Sanderson".into(),
            author_sort_name: "Sanderson, Brandon".into(),
            title: "The Final Empire: Book One".into(),
            series_title: Some("Mistborn".into()),
            series_position: Some("1".into()),
            year: Some(2006),
            quality: Some(Quality::Epub),
            ..Default::default()
        }
    }

    #[test]
    fn renders_book_tokens() {
        let cfg = NamingConfig::default();
        let n = BookNamingDto::default();
        let b = naming();
        assert_eq!(
            b.render(&n.ebook_file, &cfg),
            "The Final Empire - Book One - Mistborn 1"
        );
        assert_eq!(
            b.render(&n.audiobook_folder, &cfg),
            "The Final Empire - Book One (2006)"
        );
        assert_eq!(
            b.render("{Author SortName} [{Quality Title}]", &cfg),
            "Sanderson, Brandon [EPUB]"
        );
        let mut single = b.clone();
        single.series_title = None;
        single.series_position = None;
        assert_eq!(single.render(&n.ebook_file, &cfg), "The Final Empire - Book One");
        assert!(unknown_tokens(&n.audiobook_part_file).is_empty());
        assert_eq!(unknown_tokens("{Book Title} {Movie Title}"), ["{Movie Title}"]);
    }

    #[test]
    fn destinations() {
        let cfg = NamingConfig::default();
        let n = BookNamingDto::default();
        let b = naming();
        let (e, parts) = destination(&n, &b, BookFormat::Ebook, &[Path::new("/dl/x.EPUB")], &cfg);
        assert_eq!(e, "The Final Empire - Book One - Mistborn 1.epub");
        assert!(parts.is_empty());
        let (single, _) = destination(&n, &b, BookFormat::Audiobook, &[Path::new("/dl/a.m4b")], &cfg);
        assert_eq!(
            single,
            "The Final Empire - Book One (2006)/The Final Empire - Book One.m4b"
        );
        let srcs = [Path::new("/dl/01.mp3"), Path::new("/dl/02.mp3")];
        let (folder, parts) = destination(&n, &b, BookFormat::Audiobook, &srcs, &cfg);
        assert_eq!(folder, "The Final Empire - Book One (2006)");
        assert_eq!(
            parts,
            [
                "The Final Empire - Book One (2006)/The Final Empire - Book One - Part 01.mp3",
                "The Final Empire - Book One (2006)/The Final Empire - Book One - Part 02.mp3"
            ]
        );
        let keep = BookNamingDto {
            rename: false,
            ..Default::default()
        };
        let (e, _) = destination(
            &keep,
            &b,
            BookFormat::Ebook,
            &[Path::new("/dl/orig name.epub")],
            &cfg,
        );
        assert_eq!(e, "orig name.epub");
    }
}
