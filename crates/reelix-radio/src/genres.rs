//! Shared radio genres (RFX-93): one short list for every source. Each source names genres its own way
//! (BBC Sounds categories, Apple Podcasts genre ids, Podcast Index category names); this table maps each
//! shared genre to them, so Discover can show one row per genre with shows from every source switched on.
//! It also tells which shows come from a broadcaster (RFX-101), so search can keep only radio stations' shows.

/// One shared genre and where each source keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedGenre {
    pub id: &'static str,
    pub name: &'static str,
    /// BBC Sounds categories (`category-{id}` lists of the BBC provider), best first.
    pub bbc: &'static [&'static str],
    /// Apple Podcasts genre or sub-genre id (the old iTunes chart takes either).
    pub apple: Option<u32>,
    /// Podcast Index category name (`podcasts/trending?cat=`).
    pub podcastindex: Option<&'static str>,
}

/// The shared list, in the order Discover shows it.
pub const GENRES: &[SharedGenre] = &[
    SharedGenre {
        id: "comedy",
        name: "Comedy",
        bbc: &["comedy"],
        apple: Some(1303),
        podcastindex: Some("Comedy"),
    },
    SharedGenre {
        id: "drama",
        name: "Drama",
        bbc: &["drama"],
        apple: Some(1484),
        podcastindex: Some("Drama"),
    },
    SharedGenre {
        id: "scifi",
        name: "Science fiction and fantasy",
        bbc: &["drama-scifiandfantasy"],
        apple: Some(1485),
        podcastindex: Some("Science Fiction"),
    },
    SharedGenre {
        id: "crime",
        name: "Crime",
        bbc: &["drama-crime", "drama-thriller"],
        apple: Some(1488),
        podcastindex: Some("True Crime"),
    },
    SharedGenre {
        id: "readings",
        name: "Readings and audiobooks",
        bbc: &["readings", "audiobooks"],
        apple: Some(1482),
        podcastindex: Some("Books"),
    },
    SharedGenre {
        id: "documentaries",
        name: "Documentaries",
        bbc: &["documentaries"],
        apple: Some(1543),
        podcastindex: Some("Documentary"),
    },
    SharedGenre {
        id: "history",
        name: "History",
        bbc: &["factual-history"],
        apple: Some(1487),
        podcastindex: Some("History"),
    },
    SharedGenre {
        id: "news",
        name: "News",
        bbc: &["news"],
        apple: Some(1489),
        podcastindex: Some("News"),
    },
    SharedGenre {
        id: "sport",
        name: "Sport",
        bbc: &["sport"],
        apple: Some(1545),
        podcastindex: Some("Sports"),
    },
    SharedGenre {
        id: "music",
        name: "Music",
        bbc: &["music"],
        apple: Some(1310),
        podcastindex: Some("Music"),
    },
    SharedGenre {
        id: "arts",
        name: "Arts",
        bbc: &["factual-artscultureandthemedia"],
        apple: Some(1301),
        podcastindex: Some("Arts"),
    },
    SharedGenre {
        id: "science",
        name: "Science",
        bbc: &["factual-scienceandnature"],
        apple: Some(1533),
        podcastindex: Some("Science"),
    },
    SharedGenre {
        id: "kids",
        name: "Children's",
        bbc: &[],
        apple: Some(1305),
        podcastindex: Some("Kids"),
    },
];

/// The shared genre with this id.
pub fn genre(id: &str) -> Option<&'static SharedGenre> {
    GENRES.iter().find(|g| g.id == id)
}

/// Broadcasters whose shows count as "from a radio station" (RFX-101): matched, ignoring case, against the
/// start of a show's author or station name, so "BBC Radio 4" and "LBC" both match.
pub const BROADCASTERS: &[&str] = &[
    "BBC",
    "LBC",
    "Global",
    "Classic FM",
    "Capital",
    "Heart",
    "Radio X",
    "Smooth",
    "Gold Radio",
    "Times Radio",
    "talkSPORT",
    "Absolute Radio",
    "Kiss",
    "Magic Radio",
    "Greatest Hits Radio",
    "Hits Radio",
    "Planet Rock",
    "Scala Radio",
    "Virgin Radio",
    "Jazz FM",
    "Boom Radio",
    "GB News",
    "RTÉ",
    "RTE",
    "Newstalk",
    "Today FM",
    "ABC",
    "CBC",
    "NPR",
    "PRX",
    "WNYC",
    "Radio France",
    "France Inter",
    "France Culture",
    "Deutschlandfunk",
    "Monocle",
    "NTS",
    "Resonance FM",
    "Soho Radio",
    "Worldwide FM",
];

/// The broadcaster a show comes from, when its author or station name starts with one (whole words only, so
/// "Global" does not match "Globalisation Today").
pub fn broadcaster(author: Option<&str>, station: Option<&str>) -> Option<&'static str> {
    // A short name (BBC, ABC, Heart) counts only alone or before a radio word, so "Abc Studios" is not ABC.
    const RADIO_WORDS: &[&str] = &[
        "radio",
        "sounds",
        "news",
        "podcasts",
        "fm",
        "listen",
        "audio",
        "world service",
    ];
    let starts = |text: &str, name: &str| {
        let t = text.trim();
        if !(t.len() >= name.len()
            && t.is_char_boundary(name.len())
            && t[..name.len()].eq_ignore_ascii_case(name))
        {
            return false;
        }
        let rest = t[name.len()..].trim_start();
        if rest.is_empty() {
            return true;
        }
        if t[name.len()..].chars().next().is_some_and(char::is_alphanumeric) {
            return false;
        }
        name.chars().count() > 5 || RADIO_WORDS.iter().any(|w| rest.to_lowercase().starts_with(w))
    };
    [station, author]
        .into_iter()
        .flatten()
        .find_map(|text| BROADCASTERS.iter().copied().find(|b| starts(text, b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_genre_has_a_source_and_unique_id() {
        let mut ids: Vec<&str> = GENRES.iter().map(|g| g.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), GENRES.len());
        assert!(
            GENRES
                .iter()
                .all(|g| !g.bbc.is_empty() || g.apple.is_some() || g.podcastindex.is_some())
        );
        assert_eq!(genre("scifi").unwrap().bbc, ["drama-scifiandfantasy"]);
        assert!(genre("nope").is_none());
    }

    #[test]
    fn bbc_categories_are_ones_the_bbc_offers() {
        let known: Vec<&str> = crate::providers::bbc::categories().collect();
        for g in GENRES {
            for c in g.bbc {
                assert!(known.contains(c), "{c} is not a BBC Sounds category Reelix lists");
            }
        }
    }

    #[test]
    fn broadcasters_by_whole_word() {
        assert_eq!(broadcaster(Some("BBC Radio 4"), None), Some("BBC"));
        assert_eq!(broadcaster(None, Some("LBC")), Some("LBC"));
        assert_eq!(broadcaster(Some("talkSPORT"), None), Some("talkSPORT"));
        assert_eq!(broadcaster(Some("Times Radio"), None), Some("Times Radio"));
        assert_eq!(broadcaster(Some("Globalisation Today"), None), None);
        assert_eq!(broadcaster(Some("Abc Studios Ltd"), None), None);
        assert_eq!(broadcaster(Some("ABC listen"), None), Some("ABC"));
        assert_eq!(broadcaster(Some("Heart of the Matter Media"), None), None);
        assert_eq!(broadcaster(Some("BBC World Service"), None), Some("BBC"));
        assert_eq!(broadcaster(Some("Jane Smith"), None), None);
        assert_eq!(broadcaster(None, None), None);
    }
}
