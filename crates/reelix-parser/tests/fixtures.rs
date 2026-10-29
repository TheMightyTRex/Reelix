//! Golden fixture suite: `tests/fixtures/*.txt`, one release per line:
//!
//! ```text
//! Release.Name.S01E01.720p.HDTV.x264-GRP => t=Release Name; s=1; e=1; q=HDTV-720p; g=GRP
//! ```
//!
//! Only the listed fields are checked. Keys: `t` title, `y` year (`-` = none), `kind`
//! (movie/episode/unknown), `s` season, `e` episodes, `abs` absolute episodes, `date`
//! air date, `full` full season, `range` season range `a-b`, `special`, `q` quality name,
//! `v` revision version, `real`, `repack`, `lang` languages (set), `g` group (`-` = none),
//! `ed` edition, `hdr`, `vc` video codec, `ac` audio codecs, `ch` channels, `3d`, `hc`
//! hardcoded subs, `hash`, `svc` streaming service.

use std::fs;
use std::path::Path;
use std::time::Instant;

use reelix_parser::{ParsedRelease, ReleaseKind, parse};
use serde_json::Value;

fn snake<T: serde::Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(Value::String(s)) => s,
        other => format!("{other:?}"),
    }
}

fn list<T: serde::Serialize>(items: &[T]) -> Vec<String> {
    let mut v: Vec<String> = items.iter().map(snake).collect();
    v.sort();
    v
}

fn split_list(v: &str) -> Vec<String> {
    let mut out: Vec<String> = v
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    out.sort();
    out
}

fn opt<T: ToString>(v: Option<T>) -> String {
    v.map_or_else(|| "-".to_string(), |x| x.to_string())
}

/// Returns a description of every mismatching field.
fn check(p: &ParsedRelease, expect: &str) -> Vec<String> {
    let ep = p.episode().cloned().unwrap_or_default();
    let mut errs = Vec::new();
    for pair in expect.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let Some((k, v)) = pair.split_once('=') else {
            errs.push(format!("bad expectation {pair:?}"));
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        let nums = |xs: &[u32]| xs.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        let actual: String = match k {
            "t" => p.title.clone(),
            "y" => opt(p.year),
            "kind" => match p.kind {
                ReleaseKind::Movie => "movie".into(),
                ReleaseKind::Episode(_) => "episode".into(),
                ReleaseKind::Unknown => "unknown".into(),
            },
            "s" => opt(ep.season),
            "e" => nums(&ep.episodes),
            "abs" => nums(&ep.absolute_episodes),
            "date" => opt(ep.air_date),
            "full" => ep.full_season.to_string(),
            "range" => ep.season_range.map_or("-".into(), |(a, b)| format!("{a}-{b}")),
            "special" => ep.is_special.to_string(),
            "q" => p.quality.quality.name().to_string(),
            "v" => p.quality.revision.version.to_string(),
            "real" => p.quality.revision.real.to_string(),
            "repack" => p.quality.revision.is_repack.to_string(),
            "lang" => {
                let names: Vec<&str> = p.languages.iter().map(|l| l.name()).collect();
                if split_list(v) == split_list(&names.join(",")) {
                    continue;
                }
                names.join(",")
            }
            "g" => opt(p.release_group.clone()),
            "ed" => opt(p.edition.clone()),
            "hdr" | "ac" => {
                let got = if k == "hdr" {
                    list(&p.hdr)
                } else {
                    list(&p.audio_codecs)
                };
                if got == split_list(v) {
                    continue;
                }
                got.join(",")
            }
            "vc" => p.video_codec.map_or("-".into(), |c| snake(&c)),
            "ch" => opt(p.audio_channels.map(|c| format!("{c:.1}"))),
            "3d" => p.is_3d.to_string(),
            "hc" => p.is_hardcoded_subs.to_string(),
            "hash" => opt(p.hash.clone()),
            "svc" => opt(p.streaming_service.clone()),
            other => {
                errs.push(format!("unknown key {other:?}"));
                continue;
            }
        };
        if actual != v {
            errs.push(format!("{k}: expected {v:?}, got {actual:?}"));
        }
    }
    errs
}

fn load() -> Vec<(String, String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut files: Vec<_> = fs::read_dir(&dir)
        .expect("fixtures dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .collect();
    files.sort();
    let mut cases = Vec::new();
    for f in files {
        let name = f.file_name().unwrap_or_default().to_string_lossy().into_owned();
        for line in fs::read_to_string(&f).expect("read fixture").lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (title, expect) = line.split_once(" => ").unwrap_or((line, ""));
            cases.push((name.clone(), title.to_string(), expect.to_string()));
        }
    }
    cases
}

#[test]
fn fixture_pass_rate() {
    let cases = load();
    assert!(cases.len() >= 600, "only {} fixtures", cases.len());
    let mut failures = Vec::new();
    for (file, title, expect) in &cases {
        let errs = check(&parse(title), expect);
        if !errs.is_empty() {
            failures.push(format!("[{file}] {title}\n    {}", errs.join("\n    ")));
        }
    }
    let passed = cases.len() - failures.len();
    let rate = passed as f64 * 100.0 / cases.len() as f64;
    for f in &failures {
        println!("{f}");
    }
    println!("fixtures: {passed}/{} passed ({rate:.2}%)", cases.len());
    assert!(rate >= 99.0, "fixture pass rate {rate:.2}% < 99%");
}

#[test]
fn throughput_sanity() {
    let cases = load();
    let titles: Vec<&str> = cases.iter().map(|(_, t, _)| t.as_str()).collect();
    let start = Instant::now();
    let mut n = 0usize;
    while n < 10_000 {
        for t in &titles {
            std::hint::black_box(parse(t));
            n += 1;
        }
    }
    let elapsed = start.elapsed();
    // Generous bound so unoptimised debug builds pass too; release is ~20x faster.
    let limit = if cfg!(debug_assertions) { 30.0 } else { 1.0 };
    assert!(elapsed.as_secs_f64() < limit, "{n} parses took {elapsed:?}");
}

#[test]
fn never_panics_on_garbage() {
    // Deterministic pseudo-random strings over an alphabet heavy in separators and tag fragments.
    const ALPHABET: &[&str] = &[
        ".", "-", " ", "_", "[", "]", "(", ")", "S", "E", "0", "1", "9", "x", "264", "1080p", "WEB", "DL",
        "-", "v2", "REAL", "é", "日本", "DTS", "HD", "MA", "5.1", "2024", "~", "+", "&", "DV", "HDR10+",
        "Season", "#",
    ];
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..20_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let len = (seed % 24) as usize;
        let mut s = String::new();
        let mut x = seed;
        for _ in 0..len {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            s.push_str(ALPHABET[(x >> 33) as usize % ALPHABET.len()]);
        }
        let _ = parse(&s);
        let _ = reelix_parser::parse_path(&s.replace('_', "/"));
        let _ = reelix_parser::clean_title(&s);
    }
    let long = "S01E01-".repeat(5000);
    let _ = parse(&long);
}
