//! Property tests: release names built from real fixture titles with random edits, plus
//! random text, must never panic in any of the parsers.

use proptest::prelude::*;
use reelix_parser::{clean_title, parse, parse_book, parse_music, parse_path};

fn seeds() -> Vec<String> {
    [
        include_str!("fixtures/movies.txt"),
        include_str!("fixtures/tv.txt"),
        include_str!("fixtures/anime.txt"),
        include_str!("fixtures/misc.txt"),
        include_str!("fixtures/packs_daily.txt"),
        include_str!("fixtures/editions_tags.txt"),
    ]
    .iter()
    .flat_map(|f| f.lines())
    .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    .map(|l| l.split(" => ").next().unwrap_or(l).trim().to_owned())
    .chain(json_releases(include_str!("fixtures/books.json")))
    .chain(json_releases(include_str!("fixtures/music.json")))
    .collect()
}

/// The `"release"` strings of a JSON fixture file.
fn json_releases(text: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(text).expect("valid fixture JSON");
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.get("release").and_then(|t| t.as_str()).map(str::to_owned))
        .collect()
}

fn run_all(s: &str) {
    let _ = parse(s);
    let _ = parse_path(s);
    let _ = parse_path(&s.replace('.', "/"));
    let _ = parse_book(s, None);
    let _ = parse_music(s);
    let _ = clean_title(s);
}

/// Replaces, inserts or removes tokens of a seed title.
fn mutated() -> impl Strategy<Value = String> {
    let seeds = seeds();
    let n = seeds.len();
    let token = prop_oneof![
        Just(".".to_string()),
        Just("-".to_string()),
        Just(" ".to_string()),
        Just("[".to_string()),
        Just("]".to_string()),
        Just("(".to_string()),
        Just("S01E".to_string()),
        Just("E99999999999".to_string()),
        Just("Season 4294967296".to_string()),
        Just("99999999999999999999".to_string()),
        Just("日本語".to_string()),
        Just("\u{200b}".to_string()),
        Just("ẞ".to_string()),
        "\\PC{1,6}",
    ];
    (
        0..n,
        prop::collection::vec((0.0f64..1.0, token, any::<bool>()), 1..6),
    )
        .prop_map(move |(i, edits)| {
            let mut s = seeds[i].clone();
            for (p, tok, delete) in edits {
                let mut at = ((s.len() as f64) * p) as usize;
                while at > 0 && !s.is_char_boundary(at) {
                    at -= 1;
                }
                if delete {
                    let mut end = (at + tok.len()).min(s.len());
                    while end < s.len() && !s.is_char_boundary(end) {
                        end += 1;
                    }
                    s.replace_range(at..end, "");
                } else {
                    s.insert_str(at, &tok);
                }
            }
            s
        })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2048, ..ProptestConfig::default() })]

    #[test]
    fn mutated_titles_never_panic(s in mutated()) {
        run_all(&s);
    }

    #[test]
    fn random_text_never_panics(s in "\\PC{0,200}") {
        run_all(&s);
    }
}
