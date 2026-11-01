//! Property tests over the untrusted-input parsers: indexer responses, YAML definitions,
//! templates, selectors and dates. Each case mutates a real fixture (or is random text) and
//! only checks that nothing panics. The `fuzz/` crate runs the same entry points under
//! libFuzzer for longer sessions.

use proptest::prelude::*;
use reelix_indexers::fuzz;

const XML_SEEDS: &[&str] = &[
    include_str!("fixtures/newznab_caps.xml"),
    include_str!("fixtures/newznab_caps_media.xml"),
    include_str!("fixtures/newznab_search.xml"),
    include_str!("fixtures/newznab_media_search.xml"),
    include_str!("fixtures/torznab_caps.xml"),
    include_str!("fixtures/torznab_search.xml"),
];

const YAML_SEEDS: &[&str] = &[
    include_str!("fixtures/definitions/kestrel.yml"),
    include_str!("fixtures/definitions/lumentracker.yml"),
    include_str!("fixtures/definitions/owlbarn.yml"),
];

/// Edits applied to a seed: each picks positions as fractions of the text length.
#[derive(Debug, Clone)]
enum Edit {
    Delete(f64, usize),
    Insert(f64, String),
    Duplicate(f64, usize),
    Truncate(f64),
}

fn edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        (0.0..1.0, 1usize..64).prop_map(|(p, n)| Edit::Delete(p, n)),
        (
            0.0..1.0,
            prop_oneof![
                Just("<".to_string()),
                Just(">".to_string()),
                Just("&amp;".to_string()),
                Just("&#xFFFFFF;".to_string()),
                Just("{{ if .Config.x }}".to_string()),
                Just("\u{0}".to_string()),
                Just("ü😀".to_string()),
                "\\PC{0,24}",
            ]
        )
            .prop_map(|(p, s)| Edit::Insert(p, s)),
        (0.0..1.0, 1usize..256).prop_map(|(p, n)| Edit::Duplicate(p, n)),
        (0.0..1.0).prop_map(Edit::Truncate),
    ]
}

fn at(s: &str, frac: f64) -> usize {
    let mut i = ((s.len() as f64) * frac) as usize;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i.min(s.len())
}

fn end(s: &str, start: usize, n: usize) -> usize {
    let mut j = (start + n).min(s.len());
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

fn mutate(seed: &str, edits: &[Edit]) -> String {
    let mut s = seed.to_owned();
    for e in edits {
        match e {
            Edit::Delete(p, n) => {
                let i = at(&s, *p);
                let j = end(&s, i, *n);
                s.replace_range(i..j, "");
            }
            Edit::Insert(p, t) => {
                let i = at(&s, *p);
                s.insert_str(i, t);
            }
            Edit::Duplicate(p, n) => {
                let i = at(&s, *p);
                let j = end(&s, i, *n);
                let chunk = s[i..j].to_owned();
                s.insert_str(j, &chunk);
            }
            Edit::Truncate(p) => {
                let i = at(&s, *p);
                s.truncate(i);
            }
        }
    }
    s
}

fn mutated(seeds: &'static [&'static str]) -> impl Strategy<Value = String> {
    (0..seeds.len(), prop::collection::vec(edit(), 1..8)).prop_map(move |(i, e)| mutate(seeds[i], &e))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn newznab_responses_never_panic(input in mutated(XML_SEEDS)) {
        fuzz::newznab(&input);
    }

    #[test]
    fn newznab_random_text_never_panics(input in "\\PC{0,400}") {
        fuzz::newznab(&input);
    }

    #[test]
    fn definitions_never_panic(input in mutated(YAML_SEEDS)) {
        fuzz::definition(&input);
    }

    #[test]
    fn templates_never_panic(input in "(\\{\\{|\\}\\}|\\.|[a-zA-Z ]|\\(|\\)|\"|re_replace|join|if|else|end|range|and|or|eq|\\PC){0,60}") {
        fuzz::template(&input);
    }

    #[test]
    fn selectors_and_dates_never_panic(input in "\\PC{0,80}") {
        fuzz::selector_and_dates(&input);
    }

    #[test]
    fn date_layouts_never_panic(value in "[0-9a-zA-Z :/.,+-]{0,30}", layout in "[0-9a-zA-Z :/.,+-]{0,30}") {
        fuzz::selector_and_dates(&format!("{value}\n{layout}"));
    }
}
