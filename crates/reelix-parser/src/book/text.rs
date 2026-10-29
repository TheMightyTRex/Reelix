//! Text helpers shared by the book and music parsers. Everything here works on `char`
//! boundaries (brackets and separators are ASCII), so slicing can never panic.

/// A run of plain text or one outermost bracket group of a title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Piece<'a> {
    Text(&'a str),
    Group { open: char, inner: &'a str },
}

/// Splits `s` into plain text and outermost `[...]` / `(...)` / `{...}` groups. Unbalanced
/// brackets stay in the text.
pub(crate) fn pieces(s: &str) -> Vec<Piece<'_>> {
    let mut stack: Vec<(usize, char)> = Vec::new();
    let mut pairs: Vec<(usize, usize, char)> = Vec::new();
    for (i, c) in s.char_indices() {
        let want = match c {
            '[' | '(' | '{' => {
                stack.push((i, c));
                continue;
            }
            ']' => '[',
            ')' => '(',
            '}' => '{',
            _ => continue,
        };
        // Only look a few levels down so hostile input stays linear.
        let floor = stack.len().saturating_sub(8);
        if let Some(pos) = stack[floor..].iter().rposition(|&(_, o)| o == want) {
            let pos = floor + pos;
            let (start, open) = stack[pos];
            stack.truncate(pos);
            pairs.push((start, i, open));
        }
    }
    pairs.sort_unstable();
    let mut out = Vec::new();
    let mut cursor = 0;
    for (start, end, open) in pairs {
        if start < cursor {
            continue; // nested inside an already selected group
        }
        if start > cursor {
            out.push(Piece::Text(&s[cursor..start]));
        }
        out.push(Piece::Group {
            open,
            inner: &s[start + 1..end],
        });
        cursor = end + 1;
    }
    if cursor < s.len() {
        out.push(Piece::Text(&s[cursor..]));
    }
    out
}

/// Trims, maps typographic dashes and odd spaces to ASCII and drops control characters.
pub(crate) fn normalize(s: &str) -> String {
    let mapped: String = s
        .chars()
        .filter_map(|c| match c {
            '\u{2010}'..='\u{2015}' | '\u{2212}' => Some('-'),
            '\u{00a0}' | '\u{2000}'..='\u{200a}' | '\u{3000}' | '\t' | '\n' | '\r' => Some(' '),
            '\u{200b}'..='\u{200d}' | '\u{feff}' => None,
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    collapse(&mapped)
}

/// Collapses whitespace runs to one space and trims.
pub(crate) fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// Turns a dotted scene name into spaced words, keeping initials together
/// (`J.R.R.Tolkien.-.The.Hobbit` → `J.R.R. Tolkien - The Hobbit`).
pub(crate) fn undot(s: &str) -> String {
    let tokens: Vec<&str> = s.split('.').collect();
    let initial = |t: &str| t.chars().count() == 1 && t.chars().all(char::is_alphabetic);
    let mut out = String::with_capacity(s.len() + 4);
    for (i, t) in tokens.iter().enumerate() {
        if t.is_empty() {
            continue;
        }
        out.push_str(t);
        if initial(t) && i + 1 < tokens.len() {
            out.push('.');
            if !tokens.get(i + 1).is_some_and(|n| initial(n)) {
                out.push(' ');
            }
        } else {
            out.push(' ');
        }
    }
    collapse(&out)
}

/// Splits on spaced dashes (`Artist - Album`, `A -- B`), dropping empty parts.
pub(crate) fn segments(s: &str) -> Vec<String> {
    re!(SEP, r"\s+-+\s+|^-+\s+|\s+-+$|\s+\|\s+");
    SEP.split(s).map(tidy).filter(|p| !p.is_empty()).collect()
}

/// Collapses whitespace and trims separator junk from both ends.
pub(crate) fn tidy(s: &str) -> String {
    collapse(s)
        .trim_matches(|c: char| {
            c.is_whitespace() || matches!(c, '-' | '_' | ',' | ':' | ';' | '/' | '|' | '~' | '+')
        })
        .to_string()
}

/// A four-digit year inside `min..=2099`.
pub(crate) fn year(w: &str, min: u16) -> Option<u16> {
    if w.len() != 4 || !w.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    w.parse::<u16>().ok().filter(|y| (min..=2099).contains(y))
}

/// `true` when every word starts with an uppercase letter (or is a connector such as `&`,
/// `and`, `de`) and contains only name characters: `Ray Porter`, `Kate Reading & Michael Kramer`.
pub(crate) fn name_like(s: &str, max_words: usize) -> bool {
    let words: Vec<&str> = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() || words.len() > max_words {
        return false;
    }
    let mut capitals = 0;
    for w in &words {
        if matches!(
            *w,
            "&" | "and" | "de" | "van" | "von" | "der" | "le" | "la" | "di" | "da" | "du"
        ) {
            continue;
        }
        let mut chars = w.chars();
        let Some(first) = chars.next() else { continue };
        if !first.is_uppercase()
            || !w
                .chars()
                .all(|c| c.is_alphabetic() || matches!(c, '.' | '\'' | '-' | '’'))
        {
            return false;
        }
        capitals += 1;
    }
    capitals >= 1
}

/// Language from a word: full English/native names always, short codes only when `codes`.
pub(crate) fn language(w: &str, codes: bool) -> Option<crate::Language> {
    use crate::Language as L;
    let w = w.to_lowercase();
    let full = match w.as_str() {
        "english" => Some(L::English),
        "german" | "deutsch" => Some(L::German),
        "french" | "francais" | "français" => Some(L::French),
        "spanish" | "espanol" | "español" | "castellano" => Some(L::Spanish),
        "italian" | "italiano" => Some(L::Italian),
        "dutch" | "nederlands" => Some(L::Dutch),
        "polish" | "polski" => Some(L::Polish),
        "russian" => Some(L::Russian),
        "japanese" => Some(L::Japanese),
        "swedish" | "svenska" => Some(L::Swedish),
        "danish" | "dansk" => Some(L::Danish),
        "norwegian" | "norsk" => Some(L::Norwegian),
        "finnish" | "suomi" => Some(L::Finnish),
        "portuguese" | "portugues" | "português" => Some(L::Portuguese),
        "chinese" => Some(L::Chinese),
        "korean" => Some(L::Korean),
        "czech" => Some(L::Czech),
        "hungarian" => Some(L::Hungarian),
        "turkish" => Some(L::Turkish),
        "greek" => Some(L::Greek),
        _ => None,
    };
    if full.is_some() || !codes {
        return full;
    }
    match w.as_str() {
        "en" | "eng" => Some(L::English),
        "de" | "ger" | "deu" => Some(L::German),
        "fr" | "fre" | "fra" => Some(L::French),
        "es" | "spa" | "esp" => Some(L::Spanish),
        "it" | "ita" => Some(L::Italian),
        "nl" | "dut" | "nld" => Some(L::Dutch),
        "pl" | "pol" => Some(L::Polish),
        "ru" | "rus" => Some(L::Russian),
        "jp" | "ja" | "jpn" => Some(L::Japanese),
        "sv" | "swe" => Some(L::Swedish),
        "da" | "dan" => Some(L::Danish),
        "no" | "nor" => Some(L::Norwegian),
        "fi" | "fin" => Some(L::Finnish),
        "pt" | "por" => Some(L::Portuguese),
        _ => None,
    }
}

/// Upper-case-only text in title case (`DELUXE EDITION` → `Deluxe Edition`); other text as is.
pub(crate) fn soften_caps(s: &str) -> String {
    if s.chars().any(char::is_lowercase) || !s.chars().any(char::is_alphabetic) {
        return s.to_string();
    }
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracket_pieces() {
        let p = pieces("A (b [c]) [d] (e");
        assert_eq!(
            p,
            vec![
                Piece::Text("A "),
                Piece::Group {
                    open: '(',
                    inner: "b [c]"
                },
                Piece::Text(" "),
                Piece::Group {
                    open: '[',
                    inner: "d"
                },
                Piece::Text(" (e"),
            ]
        );
        assert!(pieces("").is_empty());
        let _ = pieces(&"(".repeat(10_000));
        let _ = pieces(&"([".repeat(5_000));
    }

    #[test]
    fn helpers() {
        assert_eq!(undot("J.R.R.Tolkien.-.The.Hobbit"), "J.R.R. Tolkien - The Hobbit");
        assert_eq!(segments("A - B -- C - "), vec!["A", "B", "C"]);
        assert_eq!(year("1999", 1900), Some(1999));
        assert_eq!(year("1899", 1900), None);
        assert!(name_like("Kate Reading & Michael Kramer", 6));
        assert!(!name_like("FLAC 24", 6));
        assert_eq!(soften_caps("DELUXE EDITION"), "Deluxe Edition");
        assert_eq!(normalize(" a \u{2013}\tb "), "a - b");
    }
}
