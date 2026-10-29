//! Provider metadata (Open Library) → library rows.

use chrono::{Datelike, NaiveDate, Utc};
use reelix_api_types::books::{AuthorDto, AuthorStatisticsDto, BookDto, NewBookMonitor};
use reelix_api_types::library::{ImagesDto, RatingsDto};
use reelix_metadata::{AuthorMetadata, EditionMetadata, SeriesRef, WorkMetadata};

use crate::repo::series::SeriesLink;

/// A new author row (user settings filled in by the caller).
pub(crate) fn author_from(meta: &AuthorMetadata) -> AuthorDto {
    let mut a = AuthorDto {
        id: 0,
        openlibrary_id: meta.id.clone(),
        name: String::new(),
        sort_name: String::new(),
        overview: None,
        birth_date: None,
        death_date: None,
        images: ImagesDto::default(),
        links: Vec::new(),
        monitored: true,
        monitor_new_items: NewBookMonitor::All,
        quality_profile_id: None,
        root_folder_id: None,
        path: None,
        audio_quality_profile_id: None,
        audio_root_folder_id: None,
        audio_path: None,
        tags: Vec::new(),
        statistics: AuthorStatisticsDto::default(),
        added: Utc::now(),
        last_refreshed: None,
    };
    apply_author(&mut a, meta);
    a
}

/// Copies provider metadata onto an author (everything except user settings).
pub(crate) fn apply_author(a: &mut AuthorDto, meta: &AuthorMetadata) {
    a.name = meta.name.trim().to_string();
    a.sort_name = if meta.sort_name.trim().is_empty() {
        sort_name(&a.name)
    } else {
        meta.sort_name.trim().to_string()
    };
    a.overview = meta.bio.clone().filter(|b| !b.trim().is_empty());
    a.birth_date = meta.birth_date.clone();
    a.death_date = meta.death_date.clone();
    a.images.poster = meta.photo_url.clone();
    a.links = meta.links.clone();
    a.last_refreshed = Some(Utc::now());
}

/// `"Brandon Sanderson"` → `"Sanderson, Brandon"`.
pub(crate) fn sort_name(name: &str) -> String {
    let name = name.trim();
    match name.rsplit_once(' ') {
        Some((first, last)) if !first.is_empty() && !name.contains(',') => format!("{last}, {first}"),
        _ => name.to_string(),
    }
}

pub(crate) fn blank_book(author: &AuthorDto) -> BookDto {
    BookDto {
        id: 0,
        author_id: author.id,
        author_name: author.name.clone(),
        openlibrary_work_id: String::new(),
        title: String::new(),
        sort_title: String::new(),
        subtitle: None,
        overview: None,
        release_date: None,
        year: None,
        genres: Vec::new(),
        images: ImagesDto::default(),
        ratings: RatingsDto::default(),
        isbn13: None,
        asin: None,
        page_count: None,
        language: None,
        series: Vec::new(),
        credits: Vec::new(),
        monitored_ebook: false,
        monitored_audiobook: false,
        ebook_file: None,
        audiobook_file: None,
        added_by_request_id: None,
        added: Utc::now(),
    }
}

/// Copies work metadata onto a book (everything except monitoring).
pub(crate) fn apply_work(b: &mut BookDto, w: &WorkMetadata) {
    b.openlibrary_work_id = w.id.clone();
    b.title = w.title.trim().to_string();
    b.sort_title = reelix_metadata::sort_title(&b.title);
    b.subtitle = w.subtitle.clone().filter(|s| !s.trim().is_empty());
    b.overview = w.description.clone().filter(|s| !s.trim().is_empty());
    b.release_date = w.first_publish_date;
    b.year = w
        .first_publish_year
        .or_else(|| w.first_publish_date.map(|d| d.year()));
    b.genres = w.subjects.iter().take(10).cloned().collect();
    if w.cover_url.is_some() {
        b.images.poster = w.cover_url.clone();
    }
    b.ratings = RatingsDto {
        // Open Library rates 1–5.
        vote_average: w.rating.map_or(0.0, |r| (r * 2.0).clamp(0.0, 10.0)),
        vote_count: w.rating_count.unwrap_or(0),
        popularity: 0.0,
    };
}

/// Fills identifiers/page count/language from the editions (preferring English ebooks for
/// ISBN, audiobooks for ASIN) when the book has none yet.
pub(crate) fn apply_editions(b: &mut BookDto, editions: &[EditionMetadata]) {
    let text = editions.iter().filter(|e| !e.is_audiobook);
    if b.isbn13.is_none() {
        b.isbn13 = text.clone().find_map(|e| e.isbn13.clone());
    }
    if b.asin.is_none() {
        b.asin = editions
            .iter()
            .filter(|e| e.is_audiobook)
            .chain(editions.iter())
            .find_map(|e| e.asin.clone());
    }
    if b.page_count.is_none() {
        b.page_count = text.clone().find_map(|e| e.page_count);
    }
    if b.language.is_none() {
        b.language = editions.iter().find_map(|e| e.language.clone());
    }
    if b.images.poster.is_none() {
        b.images.poster = editions.iter().find_map(|e| e.cover_url.clone());
    }
}

/// Series of a work: structured refs first, then edition series strings (`"Mistborn #1"`).
pub(crate) fn series_links(w: &WorkMetadata, editions: &[EditionMetadata]) -> Vec<SeriesLink> {
    let mut out: Vec<SeriesLink> = Vec::new();
    for s in w
        .series
        .iter()
        .chain(editions.iter().flat_map(|e| e.series.iter()))
    {
        let l = parse_series(s);
        if l.title.is_empty() {
            continue;
        }
        let key = crate::repo::clean(&l.title);
        match out.iter_mut().find(|x| crate::repo::clean(&x.title) == key) {
            Some(x) if x.position.is_none() => x.position = l.position,
            Some(_) => {}
            None => out.push(l),
        }
    }
    out
}

/// `"Mistborn #1"`, `"Mistborn, Book 2"`, `"Mistborn (3)"`, `"Mistborn; 2.5"` → title + position.
pub(crate) fn parse_series(s: &SeriesRef) -> SeriesLink {
    let raw = s.title.trim();
    if let Some(p) = s.position.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        return SeriesLink {
            title: raw.to_string(),
            position: Some(p.trim_start_matches('#').to_string()),
        };
    }
    let trimmed = raw.trim_end_matches(')').trim_end();
    let num_start = trimmed
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit() || *c == '.')
        .last()
        .map(|(i, _)| i);
    if let Some(i) = num_start {
        let number = trimmed[i..].trim_matches('.');
        let mut head = trimmed[..i].trim_end();
        let mut marked = false;
        for marker in ["#", "(", ",", ";"] {
            if let Some(h) = head.strip_suffix(marker) {
                head = h.trim_end();
                marked = true;
            }
        }
        for word in ["book", "vol.", "vol", "volume", "part", "no.", "no"] {
            let lower = head.to_ascii_lowercase();
            let boundary = lower.len() > word.len()
                && lower.ends_with(word)
                && lower[..lower.len() - word.len()].ends_with([' ', ',', ';', '-', '(']);
            if boundary {
                head = head[..head.len() - word.len()].trim_end();
                marked = true;
                for marker in [",", ";", "-", "("] {
                    if let Some(h) = head.strip_suffix(marker) {
                        head = h.trim_end();
                    }
                }
                break;
            }
        }
        if marked && !head.is_empty() && !number.is_empty() {
            return SeriesLink {
                title: head.to_string(),
                position: Some(number.to_string()),
            };
        }
    }
    SeriesLink {
        title: raw.to_string(),
        position: None,
    }
}

/// Whether a book counts as not yet released on `today`.
pub(crate) fn is_future(release: Option<NaiveDate>, year: Option<i32>, today: NaiveDate) -> bool {
    match (release, year) {
        (Some(d), _) => d > today,
        (None, Some(y)) => y > today.year(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: &str) -> SeriesLink {
        parse_series(&SeriesRef {
            title: t.into(),
            position: None,
        })
    }

    #[test]
    fn series_strings() {
        assert_eq!(s("Mistborn #1").title, "Mistborn");
        assert_eq!(s("Mistborn #1").position.as_deref(), Some("1"));
        assert_eq!(
            s("The Stormlight Archive, Book 2").title,
            "The Stormlight Archive"
        );
        assert_eq!(s("The Stormlight Archive, Book 2").position.as_deref(), Some("2"));
        assert_eq!(s("Discworld (12)").position.as_deref(), Some("12"));
        assert_eq!(s("Cosmere; 2.5").position.as_deref(), Some("2.5"));
        assert_eq!(s("Catch-22").title, "Catch-22");
        assert_eq!(s("Catch-22").position, None);
        assert_eq!(s("Dune").position, None);
        assert_eq!(s("Facebook 2").position, None);
        assert_eq!(s("Wheel of Time - Book 3").title, "Wheel of Time");
        let p = parse_series(&SeriesRef {
            title: "Dune".into(),
            position: Some("#2".into()),
        });
        assert_eq!(p.position.as_deref(), Some("2"));
    }

    #[test]
    fn sort_names() {
        assert_eq!(sort_name("Brandon Sanderson"), "Sanderson, Brandon");
        assert_eq!(sort_name("Plato"), "Plato");
        assert_eq!(sort_name("Tolkien, J.R.R."), "Tolkien, J.R.R.");
    }
}
