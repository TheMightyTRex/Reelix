//! Templates with caller-defined tokens (books, music).
//!
//! Uses the same syntax as [`render`](super::render): prefix/suffix, separator and case
//! variants, nested blocks and tidy-up, but the token values come from a resolver
//! instead of a [`RenderContext`](super::RenderContext).

use super::template::{self, Node};
use super::{NamingConfig, sanitize};

/// Renders `template` with caller-defined tokens into one sanitised path component.
///
/// `resolve(key, spec)` gets the lowercased token key (e.g. `"author name"`, `"track"`) and
/// the format spec after `:` (e.g. `Some("00")`); `None` = unknown token (rendered literally
/// like [`render`](super::render)). Padding and truncation are up to the resolver
/// (see [`pad_number`]).
pub fn render_custom(
    template: &str,
    resolve: &dyn Fn(&str, Option<&str>) -> Option<String>,
    config: &NamingConfig,
) -> String {
    let (raw, _) = render_nodes(&template::parse(template), resolve);
    sanitize(&template::tidy(&raw), config)
}

/// Unknown tokens in `template` for validation; `known(key)` says whether a key exists.
pub fn unknown_custom_tokens(template: &str, known: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    collect_unknown(&template::parse(template), known, &mut out);
    out
}

/// Zero-pads `n` to the width of `spec` (`"00"` → 2); helper for track/part/series numbers.
pub fn pad_number(n: u32, spec: Option<&str>) -> String {
    let width = spec
        .filter(|s| !s.is_empty() && s.chars().all(|c| c == '0'))
        .map_or(1, str::len);
    template::pad(n, width)
}

fn collect_unknown(nodes: &[Node], known: &dyn Fn(&str) -> bool, out: &mut Vec<String>) {
    for n in nodes {
        match n {
            Node::Token(t) if !known(&t.key) => out.push(format!("{{{}}}", t.raw)),
            Node::Block(children) => collect_unknown(children, known, out),
            Node::Token(_) | Node::Literal(_) | Node::SeasonEpisode(_) => {}
        }
    }
}

/// Returns the rendered text and whether any token produced a non-empty value.
fn render_nodes(nodes: &[Node], resolve: &dyn Fn(&str, Option<&str>) -> Option<String>) -> (String, bool) {
    let mut out = String::new();
    let mut any_value = false;
    for n in nodes {
        match n {
            Node::Literal(l) => out.push_str(l),
            Node::Token(t) => match resolve(&t.key, t.spec.as_deref()) {
                Some(v) => {
                    if !v.is_empty() {
                        any_value = true;
                    }
                    out.push_str(&template::decorate(t, &v));
                }
                None => out.push_str(&format!("{{{}}}", t.raw)),
            },
            Node::Block(children) => {
                let (inner, has) = render_nodes(children, resolve);
                if has {
                    any_value = true;
                    out.push('{');
                    out.push_str(&inner);
                    out.push('}');
                }
            }
            // Season/episode units have no meaning outside TV: rendered literally.
            Node::SeasonEpisode(spec) => {
                let zeros = |w: usize| {
                    if w > 1 {
                        format!(":{}", "0".repeat(w))
                    } else {
                        String::new()
                    }
                };
                out.push_str(&format!(
                    "{}{{season{}}}{}{{episode{}}}",
                    spec.season_prefix,
                    zeros(spec.season_pad),
                    spec.episode_sep,
                    zeros(spec.episode_pad)
                ));
            }
        }
    }
    (out, any_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn music(key: &str, spec: Option<&str>) -> Option<String> {
        match key {
            "artist name" => Some("AC/DC".into()),
            "album title" => Some("Back in Black: Remastered".into()),
            "release year" => Some("1980".into()),
            "album disambiguation" => Some(String::new()),
            "track" => Some(pad_number(7, spec)),
            "medium" => Some(pad_number(1, spec)),
            "track title" => Some("You Shook Me All Night Long".into()),
            _ => None,
        }
    }

    #[test]
    fn renders_custom_tokens() {
        let cfg = NamingConfig::default();
        assert_eq!(render_custom("{Artist Name}", &music, &cfg), "AC+DC");
        assert_eq!(
            render_custom(
                "{Album Title} ({Release Year}){ [Album Disambiguation]}",
                &music,
                &cfg
            ),
            "Back in Black - Remastered (1980)"
        );
        assert_eq!(
            render_custom("{medium:00}-{track:00} - {Track Title}", &music, &cfg),
            "01-07 - You Shook Me All Night Long"
        );
        assert_eq!(render_custom("{Artist.Name}.{track}", &music, &cfg), "AC+DC.7");
        assert_eq!(render_custom("{Nope} {track:00}", &music, &cfg), "{Nope} 07");
    }

    #[test]
    fn reports_unknown_tokens() {
        let known = |k: &str| music(k, None).is_some();
        assert!(unknown_custom_tokens("{Artist Name} - {track:00}", &known).is_empty());
        assert_eq!(
            unknown_custom_tokens("{Artist Name} {Movie Title} {x-{Bogus}}", &known),
            ["{Movie Title}", "{Bogus}"]
        );
    }

    #[test]
    fn pads_numbers() {
        assert_eq!(pad_number(3, Some("00")), "03");
        assert_eq!(pad_number(3, Some("000")), "003");
        assert_eq!(pad_number(12, Some("0")), "12");
        assert_eq!(pad_number(3, None), "3");
        assert_eq!(pad_number(3, Some("30")), "3");
    }
}
