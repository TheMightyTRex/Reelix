//! A CSS selector engine with the jQuery/AngleSharp extensions Cardigann definitions rely on.
//!
//! `scraper`'s selectors reject `:contains()` and friends, so definitions are matched with this
//! small engine instead. Supported:
//!
//! - type, universal, `#id`, `.class` and attribute selectors (`=`, `~=`, `|=`, `^=`, `$=`, `*=`,
//!   jQuery's `!=`, and the `i`/`s` flags);
//! - all combinators (descendant, `>`, `+`, `~`), selector lists and leading (relative) combinators;
//! - `:contains("…")`, `:containsOwn("…")`, `:has(…)` (relative selectors), `:not(…)`,
//!   `:is()`/`:matches()`/`:where()`, `:root`, `:scope`, `:empty`, `:checked`,
//!   `:nth-child()`/`:nth-last-child()`/`:nth-of-type()`/`:nth-last-of-type()`,
//!   `:first-child`, `:last-child`, `:only-child` and the `-of-type` variants.
//!
//! Everything nests: `tr:has(td:not(:contains("x")) > a)` works as expected.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use ego_tree::NodeRef;
use scraper::{ElementRef, Node};

const CACHE_LIMIT: usize = 8192;

static CACHE: LazyLock<Mutex<HashMap<String, Arc<Selector>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Parses `src`, caching by source string (templated selectors are cached per rendering).
pub fn compile(src: &str) -> Result<Arc<Selector>, String> {
    if let Some(s) = CACHE.lock().ok().and_then(|c| c.get(src).cloned()) {
        return Ok(s);
    }
    let s = Arc::new(Selector::parse(src)?);
    if let Ok(mut c) = CACHE.lock() {
        if c.len() >= CACHE_LIMIT {
            c.clear();
        }
        c.insert(src.to_string(), s.clone());
    }
    Ok(s)
}

/// A parsed selector list.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    list: Vec<Complex>,
}

/// Compound selectors joined by combinators. `parts[i].0` is the combinator between
/// `parts[i - 1]` and `parts[i]`; it is ignored for `parts[0]`.
#[derive(Debug, Clone, PartialEq)]
struct Complex {
    parts: Vec<(Comb, Vec<Simple>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comb {
    Descendant,
    Child,
    Next,
    Later,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttrOp {
    Exists,
    Eq,
    NotEq,
    Includes,
    Dash,
    Prefix,
    Suffix,
    Substr,
}

#[derive(Debug, Clone, PartialEq)]
enum Simple {
    Universal,
    Tag(String),
    Id(String),
    Class(String),
    Attr {
        name: String,
        op: AttrOp,
        value: String,
        ci: bool,
    },
    Root,
    Scope,
    Empty,
    Contains(String),
    ContainsOwn(String),
    Has(Vec<Complex>),
    Not(Vec<Complex>),
    Is(Vec<Complex>),
    Nth {
        a: i64,
        b: i64,
        last: bool,
        of_type: bool,
    },
    Only {
        of_type: bool,
    },
}

impl Selector {
    /// Parses a selector list.
    pub fn parse(src: &str) -> Result<Self, String> {
        let mut p = Parser { s: src, i: 0 };
        let list = p.list(false).map_err(|e| format!("bad selector `{src}`: {e}"))?;
        p.ws();
        if p.i != src.len() {
            return Err(format!(
                "bad selector `{src}`: unexpected `{}` at offset {}",
                &src[p.i..],
                p.i
            ));
        }
        Ok(Self { list })
    }

    /// Does `el` match? `scope` is what `:scope` (and leading combinators) refer to; `None`
    /// means the document root element.
    pub fn matches(&self, el: ElementRef<'_>, scope: Option<ElementRef<'_>>) -> bool {
        any_matches(&self.list, el, scope)
    }

    /// Matching descendants of `root` (excluding `root` itself), in document order.
    pub fn select<'a, 's>(
        &'s self,
        root: NodeRef<'a, Node>,
        scope: Option<ElementRef<'a>>,
    ) -> impl Iterator<Item = ElementRef<'a>> + 's
    where
        'a: 's,
    {
        root.descendants()
            .skip(1)
            .filter_map(ElementRef::wrap)
            .filter(move |e| self.matches(*e, scope))
    }

    /// First matching descendant of `root`.
    pub fn first<'a>(
        &self,
        root: NodeRef<'a, Node>,
        scope: Option<ElementRef<'a>>,
    ) -> Option<ElementRef<'a>> {
        self.select(root, scope).next()
    }

    /// `el` itself when it matches, else its first matching descendant (Cardigann semantics
    /// for field selectors evaluated against a row).
    pub fn first_self_or_descendant<'a>(&self, el: ElementRef<'a>) -> Option<ElementRef<'a>> {
        if self.matches(el, Some(el)) {
            return Some(el);
        }
        self.first(*el, Some(el))
    }
}

fn any_matches(list: &[Complex], el: ElementRef<'_>, scope: Option<ElementRef<'_>>) -> bool {
    list.iter()
        .any(|c| match_complex(c, c.parts.len() - 1, el, scope))
}

fn parent_el(el: ElementRef<'_>) -> Option<ElementRef<'_>> {
    el.parent().and_then(ElementRef::wrap)
}

fn prev_el(el: ElementRef<'_>) -> Option<ElementRef<'_>> {
    el.prev_siblings().find_map(ElementRef::wrap)
}

fn match_complex(c: &Complex, idx: usize, el: ElementRef<'_>, scope: Option<ElementRef<'_>>) -> bool {
    let (comb, compound) = &c.parts[idx];
    if !compound.iter().all(|s| match_simple(s, el, scope)) {
        return false;
    }
    if idx == 0 {
        return true;
    }
    let next = |e: ElementRef<'_>| match_complex(c, idx - 1, e, scope);
    match comb {
        Comb::Child => parent_el(el).is_some_and(next),
        Comb::Descendant => el.ancestors().filter_map(ElementRef::wrap).any(next),
        Comb::Next => prev_el(el).is_some_and(next),
        Comb::Later => el.prev_siblings().filter_map(ElementRef::wrap).any(next),
    }
}

fn is_root(el: ElementRef<'_>) -> bool {
    el.parent()
        .is_some_and(|p| p.value().is_document() || p.value().is_fragment())
}

fn match_simple(s: &Simple, el: ElementRef<'_>, scope: Option<ElementRef<'_>>) -> bool {
    let v = el.value();
    match s {
        Simple::Universal => true,
        Simple::Tag(t) => v.name().eq_ignore_ascii_case(t),
        Simple::Id(id) => attr(el, "id").is_some_and(|x| x == id),
        Simple::Class(c) => attr(el, "class").is_some_and(|x| x.split_ascii_whitespace().any(|k| k == c)),
        Simple::Attr { name, op, value, ci } => match_attr(attr(el, name), *op, value, *ci),
        Simple::Root => is_root(el),
        Simple::Scope => match scope {
            Some(sc) => sc.id() == el.id(),
            None => is_root(el),
        },
        Simple::Empty => el.children().all(|c| match c.value() {
            Node::Element(_) => false,
            Node::Text(t) => t.text.is_empty(),
            _ => true,
        }),
        Simple::Contains(t) => t.is_empty() || el.text().collect::<String>().contains(t.as_str()),
        Simple::ContainsOwn(t) => {
            t.is_empty()
                || el
                    .children()
                    .filter_map(|c| c.value().as_text().map(|x| &**x))
                    .collect::<String>()
                    .contains(t.as_str())
        }
        Simple::Has(list) => has(list, el),
        Simple::Not(list) => !any_matches(list, el, scope),
        Simple::Is(list) => any_matches(list, el, scope),
        Simple::Nth { a, b, last, of_type } => {
            let idx = sibling_index(el, *last, *of_type);
            nth_matches(*a, *b, idx)
        }
        Simple::Only { of_type } => {
            sibling_index(el, false, *of_type) == 1 && sibling_index(el, true, *of_type) == 1
        }
    }
}

/// `:has()` — the relative selectors are anchored on `el` through `:scope`.
fn has(list: &[Complex], el: ElementRef<'_>) -> bool {
    list.iter().any(|c| {
        let sibling = c
            .parts
            .get(1)
            .is_some_and(|p| matches!(p.0, Comb::Next | Comb::Later));
        let last = c.parts.len() - 1;
        if sibling {
            el.next_siblings()
                .flat_map(|s| s.descendants())
                .filter_map(ElementRef::wrap)
                .any(|x| match_complex(c, last, x, Some(el)))
        } else {
            el.descendants()
                .skip(1)
                .filter_map(ElementRef::wrap)
                .any(|x| match_complex(c, last, x, Some(el)))
        }
    })
}

fn attr<'a>(el: ElementRef<'a>, name: &str) -> Option<&'a str> {
    el.value()
        .attrs()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

fn match_attr(actual: Option<&str>, op: AttrOp, want: &str, ci: bool) -> bool {
    let Some(actual) = actual else {
        return op == AttrOp::NotEq;
    };
    let (a, w) = if ci {
        (actual.to_lowercase(), want.to_lowercase())
    } else {
        (actual.to_string(), want.to_string())
    };
    match op {
        AttrOp::Exists => true,
        AttrOp::Eq => a == w,
        AttrOp::NotEq => a != w,
        AttrOp::Includes => !w.is_empty() && a.split_ascii_whitespace().any(|x| x == w),
        AttrOp::Dash => a == w || a.starts_with(&format!("{w}-")),
        AttrOp::Prefix => !w.is_empty() && a.starts_with(&w),
        AttrOp::Suffix => !w.is_empty() && a.ends_with(&w),
        AttrOp::Substr => !w.is_empty() && a.contains(&w),
    }
}

/// 1-based position among element siblings (optionally of the same type, optionally from the end).
fn sibling_index(el: ElementRef<'_>, from_end: bool, of_type: bool) -> i64 {
    let name = el.value().name();
    let same = |n: &NodeRef<'_, Node>| match n.value() {
        Node::Element(e) => !of_type || e.name().eq_ignore_ascii_case(name),
        _ => false,
    };
    let before = if from_end {
        el.next_siblings().filter(same).count()
    } else {
        el.prev_siblings().filter(same).count()
    };
    before as i64 + 1
}

fn nth_matches(a: i64, b: i64, idx: i64) -> bool {
    if a == 0 {
        return idx == b;
    }
    let d = idx - b;
    d % a == 0 && d / a >= 0
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }

    fn peek2(&self) -> Option<char> {
        self.s[self.i..].chars().nth(1)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn ws(&mut self) -> bool {
        let start = self.i;
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
        self.i != start
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        self.ws();
        if self.eat(c) {
            Ok(())
        } else {
            Err(format!("expected `{c}` at offset {}", self.i))
        }
    }

    fn list(&mut self, relative: bool) -> Result<Vec<Complex>, String> {
        let mut out = Vec::new();
        loop {
            self.ws();
            out.push(self.complex(relative)?);
            self.ws();
            if !self.eat(',') {
                return Ok(out);
            }
        }
    }

    fn comb(&mut self) -> Option<Comb> {
        let c = match self.peek()? {
            '>' => Comb::Child,
            '+' => Comb::Next,
            '~' => Comb::Later,
            _ => return None,
        };
        self.bump();
        self.ws();
        Some(c)
    }

    fn complex(&mut self, relative: bool) -> Result<Complex, String> {
        let mut parts = Vec::new();
        let lead = self.comb();
        let mut pending = Comb::Descendant;
        if lead.is_some() || relative {
            parts.push((Comb::Descendant, vec![Simple::Scope]));
            pending = lead.unwrap_or(Comb::Descendant);
        }
        loop {
            let compound = self.compound()?;
            parts.push((pending, compound));
            let had_ws = self.ws();
            if let Some(c) = self.comb() {
                pending = c;
                continue;
            }
            match self.peek() {
                None | Some(',') | Some(')') => break,
                Some(_) if had_ws => pending = Comb::Descendant,
                Some(c) => return Err(format!("unexpected `{c}` at offset {}", self.i)),
            }
        }
        Ok(Complex { parts })
    }

    fn compound(&mut self) -> Result<Vec<Simple>, String> {
        let mut out = Vec::new();
        loop {
            match self.peek() {
                Some('*') if out.is_empty() => {
                    self.bump();
                    out.push(Simple::Universal);
                }
                // A type selector after other simple selectors (`a:not(x)a`) is invalid CSS but
                // AngleSharp accepts it; treat it as an extra constraint.
                Some(c) if is_ident_start(c) && c != '-' || out.is_empty() && c == '-' => {
                    out.push(Simple::Tag(self.ident()?.to_lowercase()))
                }
                Some('#') => {
                    self.bump();
                    out.push(Simple::Id(self.ident()?));
                }
                Some('.') => {
                    self.bump();
                    out.push(Simple::Class(self.ident()?));
                }
                Some('[') => {
                    self.bump();
                    out.push(self.attr()?);
                }
                Some(':') => {
                    self.bump();
                    if self.peek() == Some(':') {
                        return Err("pseudo-elements are not supported".into());
                    }
                    out.push(self.pseudo()?);
                }
                _ => break,
            }
        }
        if out.is_empty() {
            return Err(match self.peek() {
                Some(c) => format!("unexpected `{c}` at offset {}", self.i),
                None => "unexpected end of selector".into(),
            });
        }
        Ok(out)
    }

    fn ident(&mut self) -> Result<String, String> {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == '\\' {
                self.bump();
                let mut hex = String::new();
                while hex.len() < 6 && self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                    hex.push(self.bump().unwrap_or_default());
                }
                if hex.is_empty() {
                    out.push(self.bump().ok_or("dangling escape")?);
                } else {
                    let cp = u32::from_str_radix(&hex, 16).map_err(|e| e.to_string())?;
                    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    if self.peek().is_some_and(char::is_whitespace) {
                        self.bump();
                    }
                }
            } else if is_ident_char(c) {
                out.push(c);
                self.bump();
            } else {
                break;
            }
        }
        if out.is_empty() {
            return Err(format!("expected an identifier at offset {}", self.i));
        }
        Ok(out)
    }

    fn string(&mut self) -> Result<String, String> {
        let q = self.bump().ok_or("expected a string")?;
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err("unterminated string".into()),
                Some(c) if c == q => return Ok(out),
                Some('\\') => out.push(self.bump().ok_or("unterminated string")?),
                Some(c) => out.push(c),
            }
        }
    }

    fn attr(&mut self) -> Result<Simple, String> {
        self.ws();
        let name = self.ident()?.to_lowercase();
        self.ws();
        let op = match self.peek() {
            Some(']') => {
                self.bump();
                return Ok(Simple::Attr {
                    name,
                    op: AttrOp::Exists,
                    value: String::new(),
                    ci: false,
                });
            }
            Some('=') => AttrOp::Eq,
            Some(c) if self.peek2() == Some('=') => match c {
                '!' => AttrOp::NotEq,
                '~' => AttrOp::Includes,
                '|' => AttrOp::Dash,
                '^' => AttrOp::Prefix,
                '$' => AttrOp::Suffix,
                '*' => AttrOp::Substr,
                _ => return Err(format!("bad attribute operator at offset {}", self.i)),
            },
            _ => return Err(format!("bad attribute selector at offset {}", self.i)),
        };
        if op != AttrOp::Eq {
            self.bump();
        }
        self.bump();
        self.ws();
        let value = match self.peek() {
            Some('"' | '\'') => self.string()?,
            _ => {
                // Unquoted: an identifier, but be lenient (digits, `/`, `.` …) like jQuery.
                let start = self.i;
                while self.peek().is_some_and(|c| c != ']' && !c.is_whitespace()) {
                    self.bump();
                }
                self.s[start..self.i].to_string()
            }
        };
        self.ws();
        let mut ci = false;
        if let Some(c @ ('i' | 'I' | 's' | 'S')) = self.peek() {
            self.bump();
            ci = c.eq_ignore_ascii_case(&'i');
            self.ws();
        }
        if !self.eat(']') {
            return Err(format!("expected `]` at offset {}", self.i));
        }
        Ok(Simple::Attr { name, op, value, ci })
    }

    /// Raw text up to the `)` closing the current argument (nested parens and quotes skipped).
    fn raw_arg(&mut self) -> Result<String, String> {
        let start = self.i;
        let mut depth = 0;
        loop {
            match self.peek() {
                None => return Err("unclosed `(`".into()),
                Some(')') if depth == 0 => return Ok(self.s[start..self.i].trim().to_string()),
                Some(')') => depth -= 1,
                Some('(') => depth += 1,
                Some('"' | '\'') => {
                    self.string()?;
                    continue;
                }
                _ => {}
            }
            self.bump();
        }
    }

    fn pseudo(&mut self) -> Result<Simple, String> {
        let name = self.ident()?.to_ascii_lowercase();
        let has_args = self.eat('(');
        let simple = match (name.as_str(), has_args) {
            ("contains" | "containsown", true) => {
                self.ws();
                let text = match self.peek() {
                    Some('"' | '\'') => self.string()?,
                    _ => self.raw_arg()?,
                };
                if name == "contains" {
                    Simple::Contains(text)
                } else {
                    Simple::ContainsOwn(text)
                }
            }
            ("has", true) => Simple::Has(self.list(true)?),
            ("not", true) => Simple::Not(self.list(false)?),
            ("is" | "matches" | "where" | "any", true) => Simple::Is(self.list(false)?),
            ("nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type", true) => {
                let arg = self.raw_arg()?;
                let (a, b) = parse_nth(&arg).ok_or_else(|| format!("bad :{name}({arg})"))?;
                Simple::Nth {
                    a,
                    b,
                    last: name.contains("last"),
                    of_type: name.ends_with("of-type"),
                }
            }
            ("root", false) => Simple::Root,
            ("scope", false) => Simple::Scope,
            ("empty", false) => Simple::Empty,
            ("first-child" | "first-of-type" | "last-child" | "last-of-type", false) => Simple::Nth {
                a: 0,
                b: 1,
                last: name.starts_with("last"),
                of_type: name.ends_with("of-type"),
            },
            ("only-child", false) => Simple::Only { of_type: false },
            ("only-of-type", false) => Simple::Only { of_type: true },
            ("checked" | "selected" | "disabled", false) => Simple::Attr {
                name,
                op: AttrOp::Exists,
                value: String::new(),
                ci: false,
            },
            _ => return Err(format!("unsupported pseudo-class `:{name}`")),
        };
        if has_args {
            self.expect(')')?;
        }
        Ok(simple)
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '-' || c == '\\' || !c.is_ascii()
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || !c.is_ascii()
}

/// `an+b`, `odd`, `even`, `3`, `-n+2` (an optional ` of S` suffix is ignored).
fn parse_nth(arg: &str) -> Option<(i64, i64)> {
    let arg = arg.split(" of ").next().unwrap_or(arg);
    let s: String = arg
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    match s.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    let Some((a, b)) = s.split_once('n') else {
        return Some((0, s.parse().ok()?));
    };
    let a = match a {
        "" | "+" => 1,
        "-" => -1,
        x => x.parse().ok()?,
    };
    let b = if b.is_empty() {
        0
    } else {
        b.trim_start_matches('+').parse().ok()?
    };
    Some((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use scraper::Html;

    const DOC: &str = r#"<html><head><title>Results 403</title></head><body>
      <div id="top" class="box main"><h2>Login failed: <b>bad password</b></h2></div>
      <table id="t">
        <tr id="r1" class="row"><td id="c1">Cat A</td><td id="c2"><a id="a1" href="details.php?id=1">One</a></td><td id="c3"></td></tr>
        <tr id="r2" class="row hot"><td id="c4">Cat B</td><td id="c5"><a id="a2" href="details.php?id=2">Two *TCG*</a><img id="i2" src="/img/free.png"></td><td id="c6"> </td></tr>
        <tr id="r3" class="row"><td id="c7">Cat A</td><td id="c8"><a id="a3" href="download.php?id=3" data-x="Foo-bar">Three</a><font id="f3" color="blue">x</font></td><td id="c9">9</td></tr>
      </table>
      <p id="p1">own <span id="s1">inner</span></p>
    </body></html>"#;

    fn ids(sel: &str) -> Vec<String> {
        let doc = Html::parse_document(DOC);
        let s = Selector::parse(sel).unwrap();
        s.select(doc.tree.root(), None)
            .filter_map(|e| e.value().attr("id").map(str::to_string))
            .collect()
    }

    fn tags(sel: &str) -> Vec<String> {
        let doc = Html::parse_document(DOC);
        let s = Selector::parse(sel).unwrap();
        s.select(doc.tree.root(), None)
            .map(|e| e.value().name().to_string())
            .collect()
    }

    #[test]
    fn basic_selectors_and_combinators() {
        assert_eq!(ids("tr.row"), ["r1", "r2", "r3"]);
        assert_eq!(ids("#t tr.hot > td"), ["c4", "c5", "c6"]);
        assert_eq!(ids("tr#r1 + tr"), ["r2"]);
        assert_eq!(ids("tr#r1 ~ tr"), ["r2", "r3"]);
        assert_eq!(ids("table td a"), ["a1", "a2", "a3"]);
        assert_eq!(ids("a[href^=\"details.php\"]"), ["a1", "a2"]);
        assert_eq!(ids("a[href$='=3']"), ["a3"]);
        assert_eq!(ids("a[href*=id]"), ["a1", "a2", "a3"]);
        assert_eq!(ids("a[data-x|=foo i]"), ["a3"]);
        assert_eq!(ids("a[data-x|=foo]"), Vec::<String>::new());
        assert_eq!(ids("div[class~=main]"), ["top"]);
        assert_eq!(ids("a[href!=\"details.php?id=1\"]"), ["a2", "a3"]);
        assert_eq!(ids("tr[id!=r2].row"), ["r1", "r3"]);
        assert_eq!(
            ids("#c1, #a3,#c1"),
            ["c1", "a3"],
            "lists dedupe in document order"
        );
        assert_eq!(ids("TR.row:first-child"), ["r1"]);
        assert_eq!(ids("tr:last-child"), ["r3"]);
        assert_eq!(ids("tr > td:nth-child(2)"), ["c2", "c5", "c8"]);
        assert_eq!(ids("tr:nth-child(odd)"), ["r1", "r3"]);
        assert_eq!(ids("tr:nth-child(-n+2)"), ["r1", "r2"]);
        assert_eq!(ids("tr:nth-child(2n + 2)"), ["r2"]);
        assert_eq!(ids("#r3 td:nth-last-child(1)"), ["c9"]);
        assert_eq!(ids("#c5 > :nth-of-type(1)"), ["a2", "i2"]);
        assert_eq!(ids("#c5 > :nth-last-of-type(1)"), ["a2", "i2"]);
        assert_eq!(ids("#c8 > a:only-of-type"), ["a3"]);
        assert_eq!(ids("#c2 > :only-child"), ["a1"]);
        assert_eq!(ids("td:empty"), ["c3"]);
        assert_eq!(ids("tr:not(:first-child):not(.hot)"), ["r3"]);
        assert_eq!(ids("tr:is(#r1, #r3)"), ["r1", "r3"]);
    }

    #[test]
    fn contains_root_and_scope() {
        assert_eq!(tags(":root:contains(\"bad password\")"), ["html"]);
        assert_eq!(tags(":root:contains(\"nope\")"), Vec::<String>::new());
        assert_eq!(tags("title:contains(\"403\")"), ["title"]);
        assert_eq!(ids("td:contains(Cat A)"), ["c1", "c7"]);
        assert_eq!(ids("td:contains('Cat B')"), ["c4"]);
        assert_eq!(ids("p:containsOwn(\"own\")"), ["p1"]);
        assert_eq!(ids("p:containsOwn(\"inner\")"), Vec::<String>::new());
        assert_eq!(ids("p:contains(\"inner\")"), ["p1"]);
        assert_eq!(ids("div:contains(\"failed: bad\")"), ["top"]);
        assert_eq!(ids(":root #c9"), ["c9"]);
        assert_eq!(ids(":scope > body > p"), ["p1"]);
        assert_eq!(ids("a:contains(\"(paren)\")"), Vec::<String>::new());
    }

    #[test]
    fn nested_has_not_contains() {
        assert_eq!(ids("tr:has(img[src$=\"/free.png\"])"), ["r2"]);
        assert_eq!(ids("tr:has(> td > font[color=blue])"), ["r3"]);
        assert_eq!(ids("tr:has(> font)"), Vec::<String>::new());
        assert_eq!(
            ids("tr:not(:has(font[color=\"blue\"])):not(:contains(\"*TCG*\"))"),
            ["r1"]
        );
        assert_eq!(
            ids("tr:has(a[href^=\"details\"]):not(:has(a:contains(\"Two\")))"),
            ["r1"]
        );
        assert_eq!(ids("div:has(h2:contains(\"failed\"))"), ["top"]);
        assert_eq!(ids("div:has(h2:contains(\"nope\"))"), Vec::<String>::new());
        assert_eq!(ids("tr:has(+ tr.hot)"), ["r1"]);
        assert_eq!(ids("tr:has(~ tr td:contains(\"9\"))"), ["r1", "r2"]);
        assert_eq!(
            ids("td:not(:contains(\"Cat\")):not(:empty)"),
            ["c2", "c5", "c6", "c8", "c9"]
        );
        assert_eq!(
            ids("tr:has(td:not(:contains(\"Cat\")) > a:contains(\"Three\"))"),
            ["r3"]
        );
        assert_eq!(ids("tr:contains(\"Cat A\") td:has(a)"), ["c2", "c8"]);
        assert_eq!(
            ids("table:contains('Two') tr.hot:has(img), table:contains('Nope') tr"),
            ["r2"]
        );
    }

    #[test]
    fn scoped_matching_against_a_row() {
        let doc = Html::parse_document(DOC);
        let row = Selector::parse("#r2")
            .unwrap()
            .first(doc.tree.root(), None)
            .unwrap();
        let s = Selector::parse(":scope > td:nth-child(2) > a").unwrap();
        assert_eq!(s.first(*row, Some(row)).unwrap().value().attr("id"), Some("a2"));
        let s = Selector::parse("> td:last-child").unwrap();
        assert_eq!(s.first(*row, Some(row)).unwrap().value().attr("id"), Some("c6"));
        let s = Selector::parse("tr.hot").unwrap();
        assert_eq!(
            s.first_self_or_descendant(row).unwrap().value().attr("id"),
            Some("r2")
        );
        let s = Selector::parse(":has(img)").unwrap();
        assert_eq!(
            s.first_self_or_descendant(row).unwrap().value().attr("id"),
            Some("r2")
        );
    }

    #[test]
    fn parse_errors() {
        for bad in [
            "tr[[[",
            "",
            "a,",
            "td:nth-child(x)",
            "a::before",
            ":bogus",
            "a:contains(\"x\"",
            "[a=\"x]",
        ] {
            assert!(Selector::parse(bad).is_err(), "{bad} should fail");
        }
        assert!(Selector::parse("a\\:b").is_ok());
        assert_eq!(
            ids("td a:not(:contains(\"One\"))a:not(:contains(\"Two\"))"),
            ["a3"]
        );
        assert_eq!(parse_nth("3"), Some((0, 3)));
        assert_eq!(parse_nth("-2n-1"), Some((-2, -1)));
    }
}
