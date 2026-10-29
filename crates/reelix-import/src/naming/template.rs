//! Template parsing and rendering.
//!
//! Syntax (Sonarr/Radarr compatible):
//! - `{Token Name}`; the word separator picks the output separator: `{Movie.Title}`
//!   → `The.Movie.Title`. All-uppercase / all-lowercase token names upper/lowercase
//!   the value.
//! - Prefix/suffix characters inside the braces are only emitted when the value is
//!   non-empty: `{-Release Group}`, `{[Quality Full]}`, `{(Release Year)}`.
//! - Format spec after `:`: zero padding for numbers (`{season:00}`), truncation for
//!   text (`{Episode Title:30}`), a name filter for `{Custom Format:Name}`.
//! - Nested blocks `{tmdb-{TmdbId}}` render with their outer braces kept
//!   (`{tmdb-603}`, the Plex/Jellyfin id-tag convention) and vanish entirely when every
//!   token inside them is empty.
//! - `S{season:00}E{episode:00}` (or `{season}x{episode:00}`) is recognised as one
//!   season/episode unit so multi-episode files follow [`MultiEpisodeStyle`].

use std::sync::LazyLock;

use regex::Regex;

use super::MultiEpisodeStyle;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node {
    Literal(String),
    Token(TokenSpec),
    /// `{literal{Token}literal}`; outer braces are kept in the output.
    Block(Vec<Node>),
    SeasonEpisode(SeasonEpisodeSpec),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TokenSpec {
    pub raw: String,
    pub prefix: String,
    pub suffix: String,
    /// Lowercased words joined by a single space, e.g. `movie cleantitle`.
    pub key: String,
    pub separator: char,
    pub case: Case,
    pub spec: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Case {
    AsIs,
    Upper,
    Lower,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SeasonEpisodeSpec {
    /// `S`, `s` or empty.
    pub season_prefix: String,
    pub season_pad: usize,
    /// `E`, `e`, `x`, `.E`…
    pub episode_sep: String,
    pub episode_pad: usize,
    /// Separator used between repeats for [`MultiEpisodeStyle::Duplicate`].
    pub duplicate_join: String,
}

static TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<prefix>[- ._\[(]*)(?P<name>[A-Za-z0-9]+(?:[- ._][A-Za-z0-9]+)*)(?::(?P<spec>[^{}]*?))?(?P<suffix>[- ._)\]]*)$",
    )
    .expect("token regex")
});

/// Parse a template into nodes. Never fails: unbalanced braces are kept literally.
pub(crate) fn parse(template: &str) -> Vec<Node> {
    let chars: Vec<char> = template.chars().collect();
    let mut nodes = parse_range(&chars);
    merge_season_episode(&mut nodes);
    nodes
}

fn parse_range(chars: &[char]) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut lit = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{'
            && let Some(end) = matching_brace(chars, i)
        {
            let inner = &chars[i + 1..end];
            let node = if inner.contains(&'{') {
                let mut children = parse_range(inner);
                merge_season_episode(&mut children);
                Some(Node::Block(children))
            } else {
                parse_token(&inner.iter().collect::<String>()).map(Node::Token)
            };
            if let Some(node) = node {
                if !lit.is_empty() {
                    nodes.push(Node::Literal(std::mem::take(&mut lit)));
                }
                nodes.push(node);
                i = end + 1;
                continue;
            }
        }
        lit.push(chars[i]);
        i += 1;
    }
    if !lit.is_empty() {
        nodes.push(Node::Literal(lit));
    }
    nodes
}

fn matching_brace(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (j, c) in chars.iter().enumerate().skip(open) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_token(content: &str) -> Option<TokenSpec> {
    let caps = TOKEN_RE.captures(content)?;
    let name = &caps["name"];
    let separator = name.chars().find(|c| !c.is_ascii_alphanumeric()).unwrap_or(' ');
    let key = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    let has_alpha = name.chars().any(|c| c.is_ascii_alphabetic());
    let case = if has_alpha && !name.chars().any(|c| c.is_ascii_lowercase()) {
        Case::Upper
    } else if has_alpha && !name.chars().any(|c| c.is_ascii_uppercase()) && name.contains(separator) {
        // Single lowercase words (`{season}`, `{episode}`) are canonical, not a request.
        Case::Lower
    } else {
        Case::AsIs
    };
    Some(TokenSpec {
        raw: content.to_string(),
        prefix: caps["prefix"].to_string(),
        suffix: caps["suffix"].to_string(),
        key,
        separator,
        case,
        spec: caps
            .name("spec")
            .map(|m| m.as_str().to_string())
            .filter(|s| !s.is_empty()),
    })
}

fn pad_of(spec: &Option<String>) -> usize {
    spec.as_deref()
        .filter(|s| s.chars().all(|c| c == '0'))
        .map_or(1, str::len)
}

/// Collapse `[S]{season}<sep>{episode}` sequences into a single node.
fn merge_season_episode(nodes: &mut Vec<Node>) {
    let mut i = 0;
    while i + 2 < nodes.len() {
        let is_match = matches!(
            (&nodes[i], &nodes[i + 1], &nodes[i + 2]),
            (Node::Token(s), Node::Literal(sep), Node::Token(e))
                if s.key == "season" && e.key == "episode"
                    && s.prefix.is_empty() && s.suffix.is_empty()
                    && e.prefix.is_empty() && e.suffix.is_empty()
                    && is_episode_sep(sep)
        );
        if !is_match {
            i += 1;
            continue;
        }
        let (Node::Token(s), Node::Literal(sep), Node::Token(e)) =
            (nodes[i].clone(), nodes[i + 1].clone(), nodes[i + 2].clone())
        else {
            unreachable!()
        };
        let mut season_prefix = String::new();
        let mut duplicate_join = " - ".to_string();
        if i > 0
            && let Node::Literal(prev) = &mut nodes[i - 1]
        {
            let mut chars: Vec<char> = prev.chars().collect();
            if let Some(&last) = chars.last()
                && (last == 'S' || last == 's')
                && chars
                    .len()
                    .checked_sub(2)
                    .is_none_or(|k| !chars[k].is_alphanumeric())
            {
                season_prefix.push(last);
                chars.pop();
            }
            let join: String = chars
                .iter()
                .rev()
                .take_while(|c| matches!(c, ' ' | '-' | '.' | '_'))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if !join.is_empty() {
                duplicate_join = join;
            }
            *prev = chars.into_iter().collect();
        }
        let spec = SeasonEpisodeSpec {
            season_prefix,
            season_pad: pad_of(&s.spec),
            episode_sep: sep,
            episode_pad: pad_of(&e.spec),
            duplicate_join,
        };
        nodes.splice(i..i + 3, [Node::SeasonEpisode(spec)]);
        i += 1;
    }
    nodes.retain(|n| !matches!(n, Node::Literal(l) if l.is_empty()));
}

fn is_episode_sep(s: &str) -> bool {
    let mut chars = s.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some(c), None, None) => matches!(c, 'E' | 'e' | 'x' | 'X'),
        (Some(p), Some(c), None) => matches!(p, ' ' | '-' | '.' | '_') && matches!(c, 'E' | 'e' | 'x' | 'X'),
        _ => false,
    }
}

/// Pad a number to `width` digits.
pub(crate) fn pad(n: u32, width: usize) -> String {
    format!("{n:0width$}")
}

/// Render a season/episode unit for one or more episode numbers.
pub(crate) fn render_season_episode(
    spec: &SeasonEpisodeSpec,
    season: u32,
    episodes: &[u32],
    style: MultiEpisodeStyle,
) -> String {
    let s = format!("{}{}", spec.season_prefix, pad(season, spec.season_pad));
    let e = |n: u32| pad(n, spec.episode_pad);
    let sep = &spec.episode_sep;
    let bare_sep = sep.trim_start_matches([' ', '-', '.', '_']);
    let Some((&first, rest)) = episodes.split_first() else {
        return s;
    };
    let mut out = format!("{s}{sep}{}", e(first));
    if rest.is_empty() {
        return out;
    }
    let last = *rest.last().unwrap_or(&first);
    match style {
        MultiEpisodeStyle::Extend => rest.iter().for_each(|n| out.push_str(&format!("-{}", e(*n)))),
        MultiEpisodeStyle::Duplicate => rest
            .iter()
            .for_each(|n| out.push_str(&format!("{}{s}{sep}{}", spec.duplicate_join, e(*n)))),
        MultiEpisodeStyle::Repeat => rest
            .iter()
            .for_each(|n| out.push_str(&format!("{bare_sep}{}", e(*n)))),
        MultiEpisodeStyle::Scene => rest
            .iter()
            .for_each(|n| out.push_str(&format!("-{bare_sep}{}", e(*n)))),
        MultiEpisodeStyle::Range => out.push_str(&format!("-{}", e(last))),
        MultiEpisodeStyle::PrefixedRange => out.push_str(&format!("-{bare_sep}{}", e(last))),
    }
    out
}

/// Render a bare number list (`{episode}`/`{absolute}` outside a season unit).
pub(crate) fn render_number_list(nums: &[u32], width: usize, style: MultiEpisodeStyle) -> String {
    match (nums.first(), nums.last()) {
        (None, _) | (_, None) => String::new(),
        (Some(f), Some(l))
            if nums.len() > 1
                && matches!(style, MultiEpisodeStyle::Range | MultiEpisodeStyle::PrefixedRange) =>
        {
            format!("{}-{}", pad(*f, width), pad(*l, width))
        }
        _ => nums.iter().map(|n| pad(*n, width)).collect::<Vec<_>>().join("-"),
    }
}

/// Apply a token's separator, case and prefix/suffix to a resolved value.
pub(crate) fn decorate(token: &TokenSpec, value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let mut v = if token.separator == ' ' {
        value.to_string()
    } else {
        value.replace(' ', &token.separator.to_string())
    };
    match token.case {
        Case::Upper => v = v.to_uppercase(),
        Case::Lower => v = v.to_lowercase(),
        Case::AsIs => {}
    }
    format!("{}{}{}", token.prefix, v, token.suffix)
}

static EMPTY_PAIRS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(\s*\)|\[\s*\]").expect("empty pairs regex"));
static MULTI_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s{2,}").expect("space regex"));
static MULTI_DASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?: -){2,} ").expect("dash regex"));
static MULTI_DOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.{2,}|_{2,}").expect("dot regex"));

/// Tidy a rendered name: drop empty brackets, collapse doubled separators, trim ends.
pub(crate) fn tidy(s: &str) -> String {
    let s = EMPTY_PAIRS.replace_all(s, "");
    let s = MULTI_SPACE.replace_all(&s, " ");
    let s = MULTI_DASH.replace_all(&s, " - ");
    let s = MULTI_DOT.replace_all(&s, |c: &regex::Captures| c[0][..1].to_string());
    s.trim_start_matches([' ', '.'])
        .trim_end_matches([' ', '.', '-', '_'])
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tokens_blocks_and_season_episode() {
        let nodes = parse("{Series Title} - S{season:00}E{episode:00} {tmdb-{TmdbId}}{-Release Group}");
        assert!(matches!(&nodes[0], Node::Token(t) if t.key == "series title"));
        assert!(matches!(&nodes[1], Node::Literal(l) if l == " - "));
        assert!(
            matches!(&nodes[2], Node::SeasonEpisode(s) if s.season_prefix == "S" && s.season_pad == 2 && s.episode_sep == "E")
        );
        assert!(matches!(&nodes[4], Node::Block(_)));
        assert!(matches!(&nodes[5], Node::Token(t) if t.prefix == "-" && t.key == "release group"));
    }

    #[test]
    fn token_variants() {
        let t = parse_token("Movie.Title").unwrap();
        assert_eq!((t.key.as_str(), t.separator), ("movie title", '.'));
        assert_eq!(parse_token("MOVIE TITLE").unwrap().case, Case::Upper);
        assert_eq!(parse_token("movie title").unwrap().case, Case::Lower);
        assert_eq!(parse_token("season").unwrap().case, Case::AsIs);
        let t = parse_token("Air-Date").unwrap();
        assert_eq!((t.key.as_str(), t.separator), ("air date", '-'));
        let t = parse_token("[Quality Full]").unwrap();
        assert_eq!((t.prefix.as_str(), t.suffix.as_str()), ("[", "]"));
        assert_eq!(parse_token("absolute:000").unwrap().spec.as_deref(), Some("000"));
    }

    #[test]
    fn unbalanced_braces_are_literal() {
        assert_eq!(
            parse("abc {Movie Title"),
            vec![Node::Literal("abc {Movie Title".into())]
        );
    }

    #[test]
    fn tidy_cleans_leftovers() {
        assert_eq!(tidy("Movie (2010) [] - "), "Movie (2010)");
        assert_eq!(tidy("Show..S01E01..x264"), "Show.S01E01.x264");
        assert_eq!(tidy("A - - B"), "A - B");
    }
}
