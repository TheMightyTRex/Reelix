//! Title clean-up: the human-readable title shown in the UI and the normalised key
//! used to compare titles.

re!(AKA, r"(?i)[ .]+a\.?k\.?a[ .]+");

/// Turns the raw title portion of a release name into a display title:
/// separators become spaces, acronyms such as `S.H.I.E.L.D.` keep their dots,
/// and stray brackets/dashes at either end are removed.
pub(crate) fn display_title(raw: &str) -> String {
    let raw = match AKA.find(raw) {
        Some(m) if m.start() > 0 => &raw[..m.start()],
        _ => raw,
    };
    let trimmed = raw.trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, '.' | '-' | '_' | '(' | '[' | '{' | ',' | '+' | '~' | ':')
    });
    if trimmed.is_empty() {
        return String::new();
    }
    let dots = trimmed.matches('.').count();
    let spaces = trimmed.matches(' ').count();
    let joined = if dots > spaces {
        join_dotted(trimmed)
    } else {
        trimmed.to_string()
    };
    let mut out = String::with_capacity(joined.len());
    for word in joined.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    // "Title - " left over from "Title - S01E01" style names.
    while let Some(stripped) = out.strip_suffix(" -") {
        out = stripped.trim_end().to_string();
    }
    out
}

/// Splits on dots, keeping runs of single letters together as an acronym (`S.W.A.T.`).
fn join_dotted(s: &str) -> String {
    let tokens: Vec<&str> = s.split('.').collect();
    let single = |t: &str| t.chars().count() == 1 && t.chars().all(char::is_alphabetic);
    let mut out = String::with_capacity(s.len() + 1);
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if single(t) && tokens.get(i + 1).is_some_and(|n| single(n)) {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            while i < tokens.len() && single(tokens[i]) {
                out.push_str(tokens[i]);
                out.push('.');
                i += 1;
            }
            continue;
        }
        if !t.is_empty() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            out.push_str(t);
        }
        i += 1;
    }
    out
}

const STOP_WORDS: &[&str] = &["a", "an", "the", "and", "or", "of"];

/// Normalised comparison key for matching titles: lowercase ASCII letters and digits only,
/// accents folded, `&` treated as "and", and the words a/an/the/and/or/of dropped
/// (unless that would leave nothing). `"The Lord of the Rings: Return"` → `"lordringsreturn"`.
pub fn clean_title(title: &str) -> String {
    let mut folded = String::with_capacity(title.len());
    for c in title.chars() {
        if c == '&' {
            folded.push_str(" and ");
        } else if c == '\'' || c == '’' || c == '`' {
            // "Marvel's" → "marvels"
        } else {
            fold_char(c, &mut folded);
        }
    }
    let words: Vec<&str> = folded
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let kept: String = words
        .iter()
        .filter(|w| !STOP_WORDS.contains(w))
        .copied()
        .collect();
    if kept.is_empty() { words.concat() } else { kept }
}

/// Lowercases `c` and folds common Latin diacritics to ASCII.
fn fold_char(c: char, out: &mut String) {
    let lower = c.to_lowercase();
    for l in lower {
        let s = match l {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
            'ç' | 'ć' | 'č' => "c",
            'ď' | 'đ' => "d",
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
            'ğ' => "g",
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ı' | 'į' => "i",
            'ł' | 'ľ' => "l",
            'ñ' | 'ń' | 'ň' => "n",
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
            'ř' => "r",
            'ś' | 'š' | 'ş' => "s",
            'ť' | 'ţ' => "t",
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => "u",
            'ý' | 'ÿ' => "y",
            'ź' | 'ż' | 'ž' => "z",
            'ß' => "ss",
            'æ' => "ae",
            'œ' => "oe",
            'þ' => "th",
            other => {
                out.push(other);
                continue;
            }
        };
        out.push_str(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_title_variants() {
        assert_eq!(display_title("The.Show.Name."), "The Show Name");
        assert_eq!(
            display_title("Marvels.Agents.of.S.H.I.E.L.D."),
            "Marvels Agents of S.H.I.E.L.D."
        );
        assert_eq!(display_title("S.W.A.T."), "S.W.A.T.");
        assert_eq!(display_title("Movie Name ("), "Movie Name");
        assert_eq!(display_title("Show Name - "), "Show Name");
        assert_eq!(display_title("Mr. Robot "), "Mr. Robot");
        assert_eq!(display_title("Movie.Name.AKA.Other.Name."), "Movie Name");
        assert_eq!(display_title(""), "");
    }

    #[test]
    fn clean_title_normalises() {
        assert_eq!(clean_title("The Lord of the Rings: Return"), "lordringsreturn");
        assert_eq!(clean_title("Amélie"), "amelie");
        assert_eq!(
            clean_title("Marvel's Agents of S.H.I.E.L.D."),
            "marvelsagentsshield"
        );
        assert_eq!(clean_title("Law & Order"), clean_title("Law and Order"));
        assert_eq!(clean_title("The"), "the");
        assert_eq!(clean_title("Pokémon: 2000"), "pokemon2000");
    }
}
