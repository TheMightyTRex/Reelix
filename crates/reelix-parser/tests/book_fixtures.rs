//! Golden fixture suite for [`parse_book`]: `tests/fixtures/books.json`, an array of
//!
//! ```json
//! {"release": "Author - Title (2006) [EPUB]", "hint": "audiobook", "expect": {"author": "Author", "year": 2006}}
//! ```
//!
//! Only the listed fields are checked. Keys are the [`ParsedBook`] field names as serialized
//! (`null` = `None`), plus `quality` (quality name), `languages` (set of names) and `retail`
//! ([`ParsedBook::is_retail`]). `hint` is the optional `format_hint` (`ebook` / `audiobook`).

use std::fs;
use std::path::Path;
use std::time::Instant;

use reelix_core::BookFormat;
use reelix_parser::{ParsedBook, parse_book};
use serde_json::Value;

struct Case {
    release: String,
    hint: Option<BookFormat>,
    expect: serde_json::Map<String, Value>,
}

fn load() -> Vec<Case> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/books.json");
    let raw: Vec<Value> =
        serde_json::from_str(&fs::read_to_string(path).expect("read books.json")).expect("valid json");
    raw.into_iter()
        .map(|c| Case {
            release: c["release"].as_str().expect("release").to_string(),
            hint: c
                .get("hint")
                .and_then(Value::as_str)
                .map(|h| BookFormat::parse(h).expect("valid hint")),
            expect: c["expect"].as_object().cloned().unwrap_or_default(),
        })
        .collect()
}

/// Returns a description of every mismatching field.
fn check(p: &ParsedBook, expect: &serde_json::Map<String, Value>) -> Vec<String> {
    let actual = serde_json::to_value(p).expect("serializable");
    let mut errs = Vec::new();
    for (k, want) in expect {
        let got = match k.as_str() {
            "quality" => Value::from(p.quality.quality.name()),
            "retail" => Value::from(p.is_retail()),
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
fn book_fixture_pass_rate() {
    let cases = load();
    assert!(cases.len() >= 150, "only {} book fixtures", cases.len());
    let mut failures = Vec::new();
    for c in &cases {
        let errs = check(&parse_book(&c.release, c.hint), &c.expect);
        if !errs.is_empty() {
            failures.push(format!("{:?}\n    {}", c.release, errs.join("\n    ")));
        }
    }
    let passed = cases.len() - failures.len();
    let rate = passed as f64 * 100.0 / cases.len() as f64;
    for f in &failures {
        println!("{f}");
    }
    println!("book fixtures: {passed}/{} passed ({rate:.2}%)", cases.len());
    assert!(rate >= 99.0, "book fixture pass rate {rate:.2}% < 99%");
}

#[test]
fn book_throughput_sanity() {
    let cases = load();
    let start = Instant::now();
    let mut n = 0usize;
    while n < 5_000 {
        for c in &cases {
            std::hint::black_box(parse_book(&c.release, c.hint));
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
fn book_never_panics_on_garbage() {
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
        "#",
        ",",
        ":",
        "0",
        "1",
        "9",
        "2010",
        "by ",
        " by ",
        "Book",
        "Vol.",
        "EPUB",
        "m4b",
        "64kbps",
        "ISBN",
        "978",
        "B0",
        "ASIN",
        "Narrated",
        "read by",
        "é",
        "日本",
        "Unabridged",
        "RETAIL",
        "eBook",
        "-GRP",
        "Books 1-",
        "Collection",
        "Le Guin, ",
        "X",
        "~",
        "\u{2013}",
    ];
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
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
        for hint in [None, Some(BookFormat::Ebook), Some(BookFormat::Audiobook)] {
            let b = parse_book(&s, hint);
            let _ = b.to_release();
        }
    }
    let hostile = [
        String::new(),
        " ".into(),
        "-".into(),
        "()".into(),
        "[[[[".into(),
        "]]]]".into(),
        ")(".into(),
        " by ".into(),
        "by".into(),
        "#".into(),
        ".epub".into(),
        "...".into(),
        "_-_".into(),
        "\u{0}\u{1}\u{7f}".into(),
        "\u{feff}\u{200b}".into(),
        "é".repeat(3000),
        "(".repeat(20_000),
        "[(".repeat(10_000),
        "a - ".repeat(5_000),
        "Book 1-".repeat(2_000),
        "9780441013593".repeat(500),
        "B0".repeat(5_000),
        "x.".repeat(10_000),
        "A_".repeat(10_000),
        "Author - Title by ".repeat(1_000),
    ];
    for s in &hostile {
        let _ = parse_book(s, None);
        let _ = parse_book(s, Some(BookFormat::Audiobook));
    }
}
