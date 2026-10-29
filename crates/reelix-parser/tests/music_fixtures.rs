//! Golden fixture suite for [`parse_music`]: `tests/fixtures/music.json`, an array of
//!
//! ```json
//! {"release": "Artist - Album (2019) [FLAC 24-96]", "expect": {"artist": "Artist", "bit_depth": 24}}
//! ```
//!
//! Only the listed fields are checked. Keys are the [`ParsedMusic`] field names as serialized
//! (`null` = `None`), plus `quality` (quality name), `languages` (set of names) and
//! `various_artists` ([`ParsedMusic::is_various_artists`]).

use std::fs;
use std::path::Path;
use std::time::Instant;

use reelix_parser::{ParsedMusic, parse_music};
use serde_json::Value;

fn load() -> Vec<(String, serde_json::Map<String, Value>)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/music.json");
    let raw: Vec<Value> =
        serde_json::from_str(&fs::read_to_string(path).expect("read music.json")).expect("valid json");
    raw.into_iter()
        .map(|c| {
            let release = c["release"].as_str().expect("release").to_string();
            (release, c["expect"].as_object().cloned().unwrap_or_default())
        })
        .collect()
}

/// Returns a description of every mismatching field.
fn check(p: &ParsedMusic, expect: &serde_json::Map<String, Value>) -> Vec<String> {
    let actual = serde_json::to_value(p).expect("serializable");
    let mut errs = Vec::new();
    for (k, want) in expect {
        let got = match k.as_str() {
            "quality" => Value::from(p.quality.quality.name()),
            "various_artists" => Value::from(p.is_various_artists()),
            "languages" => {
                let mut names: Vec<&str> = p.languages.iter().map(|l| l.name()).collect();
                names.sort_unstable();
                let mut want_names: Vec<&str> = want
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                want_names.sort_unstable();
                if names == want_names {
                    continue;
                }
                Value::from(names)
            }
            other => match actual.get(other) {
                Some(v) => v.clone(),
                None => {
                    errs.push(format!("unknown key {other:?}"));
                    continue;
                }
            },
        };
        if &got != want {
            errs.push(format!("{k}: expected {want}, got {got}"));
        }
    }
    errs
}

#[test]
fn music_fixture_pass_rate() {
    let cases = load();
    assert!(cases.len() >= 200, "only {} music fixtures", cases.len());
    let mut failures = Vec::new();
    for (release, expect) in &cases {
        let errs = check(&parse_music(release), expect);
        if !errs.is_empty() {
            failures.push(format!("{release:?}\n    {}", errs.join("\n    ")));
        }
    }
    let passed = cases.len() - failures.len();
    let rate = passed as f64 * 100.0 / cases.len() as f64;
    for f in &failures {
        println!("{f}");
    }
    println!("music fixtures: {passed}/{} passed ({rate:.2}%)", cases.len());
    assert!(rate >= 99.0, "music fixture pass rate {rate:.2}% < 99%");
}

#[test]
fn music_throughput_sanity() {
    let cases = load();
    let start = Instant::now();
    let mut n = 0usize;
    while n < 5_000 {
        for (release, _) in &cases {
            std::hint::black_box(parse_music(release));
            n += 1;
        }
    }
    let limit = if cfg!(debug_assertions) { 30.0 } else { 1.0 };
    assert!(
        start.elapsed().as_secs_f64() < limit,
        "{n} parses took {:?}",
        start.elapsed()
    );
}

#[test]
fn music_never_panics_on_garbage() {
    const ALPHABET: &[&str] = &[
        ".",
        "-",
        " ",
        "_",
        "[",
        "]",
        "(",
        ")",
        "{",
        "}",
        "_-_",
        "FLAC",
        "24",
        "-96",
        "/",
        "44.1",
        "kHz",
        "bit",
        "MP3",
        "320",
        "V0",
        "WEB",
        "CD",
        "2CD",
        "Vinyl",
        "2019",
        "1990-2020",
        "VA",
        "Discography",
        "Deluxe",
        "Edition",
        "Remastered",
        "CAT123",
        "é",
        "日本",
        "@",
        "-GRP",
        "kbps",
        "~",
        "\u{2014}",
        "!",
    ];
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..20_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let len = (seed % 28) as usize;
        let mut s = String::new();
        let mut x = seed;
        for _ in 0..len {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            s.push_str(ALPHABET[(x >> 33) as usize % ALPHABET.len()]);
        }
        let _ = parse_music(&s).to_release();
    }
    let hostile = [
        String::new(),
        " ".into(),
        "-".into(),
        "--".into(),
        "_-_".into(),
        "()".into(),
        ")(".into(),
        "[[[[".into(),
        "]]]]".into(),
        ".flac".into(),
        "\u{0}\u{1}\u{7f}".into(),
        "\u{feff}\u{200b}".into(),
        "é".repeat(3000),
        "(".repeat(20_000),
        "{[(".repeat(7_000),
        "a-".repeat(10_000),
        "A_-_".repeat(5_000),
        "a - ".repeat(5_000),
        "24-96 ".repeat(3_000),
        "1990-2020-".repeat(2_000),
        "x.-.".repeat(5_000),
        "Artist - Album [FLAC]-".repeat(1_000),
    ];
    for s in &hostile {
        let _ = parse_music(s);
    }
}
