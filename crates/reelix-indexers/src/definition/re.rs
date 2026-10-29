//! Regexes supplied by definitions. They are written for .NET, so patterns the `regex` crate
//! rejects (lookaround, backreferences, atomic groups …) fall back to `fancy-regex`, and
//! .NET replacement strings (`$1abc`, `$&`) are translated.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

/// A compiled definition regex.
#[derive(Debug)]
pub enum Re {
    Std(regex::Regex),
    Fancy(fancy_regex::Regex),
}

const CACHE_LIMIT: usize = 4096;

static CACHE: LazyLock<Mutex<HashMap<String, Arc<Re>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Compiles (and caches) `pattern`.
pub fn compile(pattern: &str) -> Result<Arc<Re>, String> {
    if let Some(r) = CACHE.lock().ok().and_then(|c| c.get(pattern).cloned()) {
        return Ok(r);
    }
    let build = |p: &str| match regex::Regex::new(p) {
        Ok(r) => Ok(Re::Std(r)),
        Err(std_err) => match fancy_regex::Regex::new(p) {
            Ok(r) => Ok(Re::Fancy(r)),
            Err(e) => Err(format!("bad regex `{pattern}`: {e} ({std_err})")),
        },
    };
    let re = Arc::new(match build(pattern) {
        Ok(r) => r,
        Err(e) => {
            let translated = translate(pattern);
            if translated == pattern {
                return Err(e);
            }
            build(&translated).map_err(|_| e)?
        }
    });
    if let Ok(mut c) = CACHE.lock() {
        if c.len() >= CACHE_LIMIT {
            c.clear();
        }
        c.insert(pattern.to_string(), re.clone());
    }
    Ok(re)
}

impl Re {
    /// Text of capture group `i` of the first match.
    pub fn group(&self, s: &str, i: usize) -> Option<String> {
        match self {
            Self::Std(r) => r.captures(s)?.get(i).map(|m| m.as_str().to_string()),
            Self::Fancy(r) => r.captures(s).ok()??.get(i).map(|m| m.as_str().to_string()),
        }
    }

    /// Replaces every match; `rep` uses .NET syntax (`$1`, `${name}`, `$&`, `$$`).
    pub fn replace_all(&self, s: &str, rep: &str) -> String {
        let rep = dotnet_replacement(rep);
        match self {
            Self::Std(r) => r.replace_all(s, rep.as_str()).into_owned(),
            Self::Fancy(r) => match r.try_replacen(s, 0, rep.as_str()) {
                Ok(Cow::Owned(v)) => v,
                Ok(Cow::Borrowed(v)) => v.to_string(),
                Err(e) => {
                    tracing::debug!(error = %e, "regex replace failed");
                    s.to_string()
                }
            },
        }
    }
}

/// Rewrites .NET-only syntax: a bare `[` (or `&`/`~`, set operators in Rust) inside a character
/// class is literal in .NET, and `\p{IsBlock}` names Unicode blocks.
fn translate(p: &str) -> String {
    let mut out = String::with_capacity(p.len() + 8);
    let chars: Vec<char> = p.chars().collect();
    let mut i = 0;
    let mut in_class = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            if chars.get(i + 1) == Some(&'p') || chars.get(i + 1) == Some(&'P') {
                let neg = chars[i + 1] == 'P';
                if chars.get(i + 2) == Some(&'{')
                    && let Some(end) = chars[i + 3..].iter().position(|&c| c == '}')
                {
                    let name: String = chars[i + 3..i + 3 + end].iter().collect();
                    let block = match name.strip_prefix("Is") {
                        Some("CJKUnifiedIdeographs") => {
                            Some("\\u{4E00}-\\u{9FFF}\\u{3400}-\\u{4DBF}".to_string())
                        }
                        Some(script) => Some(format!("\\p{{{script}}}")),
                        None => None,
                    };
                    if let Some(b) = block {
                        match (in_class, neg) {
                            (true, false) => out.push_str(&b),
                            (false, false) => out.push_str(&format!("[{b}]")),
                            (_, true) => out.push_str(&format!("[^{b}]")),
                        }
                        i += 4 + end;
                        continue;
                    }
                }
            }
            out.push(c);
            if let Some(&n) = chars.get(i + 1) {
                out.push(n);
                // `[\s-_]`: a `-` after a class escape is literal in .NET, a bad range in Rust.
                if in_class && "sSdDwW".contains(n) && chars.get(i + 2) == Some(&'-') {
                    out.push_str("\\-");
                    i += 1;
                }
            }
            i += 2;
            continue;
        }
        if in_class {
            match c {
                ']' => in_class = false,
                '[' if chars.get(i + 1) != Some(&':') => {
                    out.push_str("\\[");
                    i += 1;
                    continue;
                }
                '&' | '~' => {
                    out.push('\\');
                }
                _ => {}
            }
        } else if c == '[' {
            in_class = true;
            out.push(c);
            i += 1;
            if chars.get(i) == Some(&'^') {
                out.push('^');
                i += 1;
            }
            // A `]` right after the opening bracket is literal.
            if chars.get(i) == Some(&']') {
                out.push_str("\\]");
                i += 1;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// .NET replacement → `regex` replacement: `$12abc` means group 12 followed by `abc` in .NET
/// but a group named `12abc` in Rust, so numbered groups get braces.
fn dotnet_replacement(rep: &str) -> String {
    let mut out = String::with_capacity(rep.len() + 4);
    let mut chars = rep.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('$') => {
                chars.next();
                out.push_str("$$");
            }
            Some('&') => {
                chars.next();
                out.push_str("${0}");
            }
            Some('{') => {
                out.push('$');
                for ch in chars.by_ref() {
                    out.push(ch);
                    if ch == '}' {
                        break;
                    }
                }
            }
            Some(d) if d.is_ascii_digit() => {
                let mut n = String::new();
                while let Some(&d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                    n.push(d);
                    chars.next();
                }
                out.push_str(&format!("${{{n}}}"));
            }
            _ => out.push_str("$$"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_and_fancy_patterns() {
        let r = compile(r"(\d+) GB").unwrap();
        assert!(matches!(*r, Re::Std(_)));
        assert_eq!(r.group("size 12 GB", 1).as_deref(), Some("12"));
        let la = compile(r"(?i)\b(MULTI(?!.*(?:FRENCH|ENGLISH)))\b").unwrap();
        assert!(matches!(*la, Re::Fancy(_)));
        assert_eq!(
            la.replace_all("Film MULTI 1080p", "MULTI.FRENCH"),
            "Film MULTI.FRENCH 1080p"
        );
        assert_eq!(la.replace_all("Film MULTI FRENCH", "X"), "Film MULTI FRENCH");
        let lb = compile(r"(?<=\d)\s+(?=\d)").unwrap();
        assert_eq!(lb.replace_all("1 000 000 B", ""), "1000000 B");
        let br = compile(r"(\.MULTI)\1").unwrap();
        assert_eq!(br.replace_all("a.MULTI.MULTI.b", "$1"), "a.MULTI.b");
        assert!(br.group(".MULTI.MULTI", 0).is_some());
        assert!(compile("(unclosed").is_err());
        let punct = compile(r##"[\[!"#$%&'()*+,\-.\/:;<=>?@[\]^_`{|}~]"##).unwrap();
        assert_eq!(punct.replace_all("a[b]c&d~e", ""), "abcde");
        let cjk = compile(r"([\p{IsCJKUnifiedIdeographs}\W]+)").unwrap();
        assert_eq!(cjk.replace_all("中文 Title", " "), " Title");
        let dash = compile(r"T[\s-_]?(\d{1,2})").unwrap();
        assert_eq!(dash.group("T-3", 1).as_deref(), Some("3"));
    }

    #[test]
    fn dotnet_replacements() {
        assert_eq!(dotnet_replacement("$1abc"), "${1}abc");
        assert_eq!(dotnet_replacement("${name}-$$-$&"), "${name}-$$-${0}");
        assert_eq!(dotnet_replacement("cost $"), "cost $$");
        let r = compile(r"(\w+) (\d{4})").unwrap();
        assert_eq!(r.replace_all("Show 2026", "$1.($2)x"), "Show.(2026)x");
    }
}
