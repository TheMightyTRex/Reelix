//! Entry points for fuzzing and property tests: each takes untrusted text and must never panic.
//! Not part of the public API.

use chrono::{TimeZone, Utc};

use crate::newznab::{Flavor, parse_caps, parse_rss};

/// Parses an indexer response the way a search does (`t=search` RSS and `t=caps`).
pub fn newznab(input: &str) {
    if let Ok(root) = crate::xml::parse(input) {
        let _ = parse_rss(&root, Flavor::Torznab, 1, "fuzz");
        let _ = parse_rss(&root, Flavor::Newznab, 1, "fuzz");
        let _ = parse_caps(&root);
    }
}

/// Parses and validates a YAML indexer definition.
pub fn definition(input: &str) {
    if let Ok(d) = crate::definition::parse(input) {
        let _ = crate::definition::schema(&d);
    }
}

/// Parses, checks and renders a definition template against an empty context.
pub fn template(input: &str) {
    let _ = crate::definition::template::check(input);
    let ctx = crate::definition::template::Context::default();
    let _ = crate::definition::template::render(input, &ctx);
}

/// Compiles a CSS selector and runs a date filter on the same text.
pub fn selector_and_dates(input: &str) {
    if let Ok(sel) = crate::definition::selector::Selector::parse(input) {
        let html = scraper::Html::parse_fragment("<div><a href=\"x\">t</a><span>1</span></div>");
        let _ = sel.first_self_or_descendant(html.root_element());
    }
    let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let _ = crate::definition::filters::from_unknown(input, now);
    if let Some((value, layout)) = input.split_once('\n') {
        let _ = crate::definition::filters::parse_layout(value, layout, now);
    }
}
