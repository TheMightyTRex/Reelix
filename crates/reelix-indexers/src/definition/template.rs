//! A small Go-template-like engine covering what definitions use:
//! `{{ .Path }}`, `{{ if }}…{{ else if }}…{{ else }}…{{ end }}`, `{{ range .List }}{{ . }}{{ end }}`,
//! `{{- trim -}}` markers and the functions `and`, `or`, `not`, `eq`, `ne`, `join`, `re_replace`.

use std::collections::HashMap;

/// A template value.
#[derive(Debug, Clone, PartialEq)]
pub enum TValue {
    Null,
    Bool(bool),
    Str(String),
    List(Vec<String>),
}

impl TValue {
    pub fn truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Str(s) => !s.is_empty(),
            Self::List(l) => !l.is_empty(),
        }
    }

    pub fn render(&self) -> String {
        match self {
            Self::Null => String::new(),
            // Unticked checkboxes are empty, so `eq .Config.x .False` holds as in Cardigann.
            Self::Bool(true) => "true".into(),
            Self::Bool(false) => String::new(),
            Self::Str(s) => s.clone(),
            Self::List(l) => l.join(" "),
        }
    }
}

impl From<&str> for TValue {
    fn from(s: &str) -> Self {
        Self::Str(s.into())
    }
}

impl From<String> for TValue {
    fn from(s: String) -> Self {
        Self::Str(s)
    }
}

/// Variables keyed by their dotted path without the leading dot (`Config.username`).
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub vars: HashMap<String, TValue>,
}

impl Context {
    pub fn set(&mut self, k: impl Into<String>, v: impl Into<TValue>) {
        self.vars.insert(k.into(), v.into());
    }

    fn lookup(&self, path: &str, dot: Option<&TValue>) -> TValue {
        if path == "." {
            return dot.cloned().unwrap_or(TValue::Null);
        }
        self.vars
            .get(path.trim_start_matches('.'))
            .cloned()
            .unwrap_or(TValue::Null)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Path(String),
    Lit(String),
    Call(String, Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Text(String),
    Out(Expr),
    If(Vec<(Expr, Vec<Node>)>, Vec<Node>),
    Range(Expr, Vec<Node>),
}

/// Template syntax error.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("template error: {0}")]
pub struct TemplateError(pub String);

/// A parsed template.
#[derive(Debug, Clone, PartialEq)]
pub struct Template(Vec<Node>);

enum Tok {
    Text(String),
    Action(String),
}

fn lex(src: &str) -> Result<Vec<Tok>, TemplateError> {
    let mut out = Vec::new();
    let mut rest = src;
    let mut trim_next = false;
    while let Some(start) = rest.find("{{") {
        let mut text = rest[..start].to_string();
        if trim_next {
            text = text.trim_start().to_string();
        }
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| TemplateError("unclosed {{".into()))?;
        let mut inner = &after[..end];
        if let Some(i) = inner.strip_prefix('-') {
            text = text.trim_end().to_string();
            inner = i;
        }
        trim_next = false;
        if let Some(i) = inner.strip_suffix('-') {
            trim_next = true;
            inner = i;
        }
        if !text.is_empty() {
            out.push(Tok::Text(text));
        }
        out.push(Tok::Action(inner.trim().to_string()));
        rest = &after[end + 2..];
    }
    let text = if trim_next { rest.trim_start() } else { rest };
    if !text.is_empty() {
        out.push(Tok::Text(text.to_string()));
    }
    Ok(out)
}

fn words(s: &str) -> Result<Vec<String>, TemplateError> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' || c == '`' {
            chars.next();
            let mut w = String::from("\"");
            let mut closed = false;
            while let Some(ch) = chars.next() {
                if ch == c {
                    closed = true;
                    break;
                }
                if ch == '\\' && c == '"' {
                    match chars.next() {
                        Some('n') => w.push('\n'),
                        Some('t') => w.push('\t'),
                        Some(o) => w.push(o),
                        None => {}
                    }
                } else {
                    w.push(ch);
                }
            }
            if !closed {
                return Err(TemplateError("unterminated string".into()));
            }
            out.push(w);
        } else if c == '(' || c == ')' {
            out.push(c.to_string());
            chars.next();
        } else {
            let mut w = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() || ch == '(' || ch == ')' {
                    break;
                }
                w.push(ch);
                chars.next();
            }
            out.push(w);
        }
    }
    Ok(out)
}

fn parse_expr(s: &str) -> Result<Expr, TemplateError> {
    let w = words(s)?;
    let mut pos = 0;
    let e = parse_call(&w, &mut pos)?;
    if pos != w.len() {
        return Err(TemplateError(format!("unexpected `{}`", w[pos])));
    }
    Ok(e)
}

fn parse_atom(w: &[String], pos: &mut usize) -> Result<Expr, TemplateError> {
    let t = w
        .get(*pos)
        .ok_or_else(|| TemplateError("missing operand".into()))?;
    *pos += 1;
    if t == "(" {
        let e = parse_call(w, pos)?;
        if w.get(*pos).map(String::as_str) != Some(")") {
            return Err(TemplateError("missing )".into()));
        }
        *pos += 1;
        Ok(e)
    } else if let Some(lit) = t.strip_prefix('"') {
        Ok(Expr::Lit(lit.to_string()))
    } else if t.starts_with('.') {
        Ok(Expr::Path(t.clone()))
    } else if t.chars().all(|c| c.is_ascii_digit() || c == '-' || c == '.') {
        Ok(Expr::Lit(t.clone()))
    } else if t == "true" || t == "false" {
        Ok(Expr::Lit(if t == "true" { "true".into() } else { String::new() }))
    } else {
        Ok(Expr::Call(t.clone(), Vec::new()))
    }
}

fn parse_call(w: &[String], pos: &mut usize) -> Result<Expr, TemplateError> {
    let first = parse_atom(w, pos)?;
    if let Expr::Call(name, _) = first {
        let mut args = Vec::new();
        while *pos < w.len() && w[*pos] != ")" {
            args.push(parse_atom(w, pos)?);
        }
        Ok(Expr::Call(name, args))
    } else {
        Ok(first)
    }
}

impl Template {
    pub fn parse(src: &str) -> Result<Self, TemplateError> {
        let toks = lex(src)?;
        let mut pos = 0;
        let (nodes, end) = parse_block(&toks, &mut pos)?;
        if end.is_some() {
            return Err(TemplateError("unexpected {{ end }} / {{ else }}".into()));
        }
        Ok(Self(nodes))
    }

    pub fn render(&self, ctx: &Context) -> Result<String, TemplateError> {
        let mut out = String::new();
        render_nodes(&self.0, ctx, None, None, &mut out)?;
        Ok(out)
    }

    /// Renders with every substituted value passed through `esc` (literal text is kept as is).
    pub fn render_escaped(&self, ctx: &Context, esc: Escape) -> Result<String, TemplateError> {
        let mut out = String::new();
        render_nodes(&self.0, ctx, None, Some(esc), &mut out)?;
        Ok(out)
    }

    /// Load-time check that every function used is implemented.
    pub fn check_functions(&self) -> Result<(), TemplateError> {
        fn expr(e: &Expr) -> Result<(), TemplateError> {
            if let Expr::Call(name, args) = e {
                if !FUNCTIONS.contains(&name.as_str()) {
                    return Err(TemplateError(format!("unsupported: template function `{name}`")));
                }
                args.iter().try_for_each(expr)?;
            }
            Ok(())
        }
        fn nodes(ns: &[Node]) -> Result<(), TemplateError> {
            for n in ns {
                match n {
                    Node::Text(_) => {}
                    Node::Out(e) => expr(e)?,
                    Node::If(branches, otherwise) => {
                        for (c, body) in branches {
                            expr(c)?;
                            nodes(body)?;
                        }
                        nodes(otherwise)?;
                    }
                    Node::Range(e, body) => {
                        expr(e)?;
                        nodes(body)?;
                    }
                }
            }
            Ok(())
        }
        nodes(&self.0)
    }

    /// Plain string without any `{{`?
    pub fn is_literal(src: &str) -> bool {
        !src.contains("{{")
    }
}

const FUNCTIONS: &[&str] = &["and", "or", "not", "eq", "ne", "join", "re_replace", "len"];

/// Parses `src` and checks its functions (load-time validation).
pub fn check(src: &str) -> Result<(), TemplateError> {
    if Template::is_literal(src) {
        return Ok(());
    }
    Template::parse(src)?.check_functions()
}

/// Escaping applied to substituted values (e.g. URL encoding for paths).
pub type Escape = fn(&str) -> String;

/// Parse + render, URL-encoding substituted values (Cardigann does this for paths and `$raw`).
pub fn render_urlencoded(src: &str, ctx: &Context) -> Result<String, TemplateError> {
    if Template::is_literal(src) {
        return Ok(src.to_string());
    }
    Template::parse(src)?.render_escaped(ctx, |v| {
        url::form_urlencoded::byte_serialize(v.as_bytes()).collect()
    })
}

/// Parse + render in one go.
pub fn render(src: &str, ctx: &Context) -> Result<String, TemplateError> {
    if Template::is_literal(src) {
        return Ok(src.to_string());
    }
    Template::parse(src)?.render(ctx)
}

/// Parses nodes until `end`/`else`; returns the terminating action text if any.
fn parse_block(toks: &[Tok], pos: &mut usize) -> Result<(Vec<Node>, Option<String>), TemplateError> {
    let mut nodes = Vec::new();
    while *pos < toks.len() {
        let t = &toks[*pos];
        *pos += 1;
        match t {
            Tok::Text(s) => nodes.push(Node::Text(s.clone())),
            Tok::Action(a) if a == "end" || a == "else" || a.starts_with("else ") => {
                return Ok((nodes, Some(a.clone())));
            }
            Tok::Action(a) if a.starts_with("if ") => {
                let mut branches = vec![];
                let mut cond = parse_expr(&a[3..])?;
                let otherwise;
                loop {
                    let (body, end) = parse_block(toks, pos)?;
                    branches.push((cond, body));
                    match end.as_deref() {
                        Some("end") => {
                            otherwise = Vec::new();
                            break;
                        }
                        Some("else") => {
                            let (body, end) = parse_block(toks, pos)?;
                            if end.as_deref() != Some("end") {
                                return Err(TemplateError("missing {{ end }}".into()));
                            }
                            otherwise = body;
                            break;
                        }
                        Some(e) if e.starts_with("else if ") => cond = parse_expr(&e[8..])?,
                        _ => return Err(TemplateError("missing {{ end }}".into())),
                    }
                }
                nodes.push(Node::If(branches, otherwise));
            }
            Tok::Action(a) if a.starts_with("range ") => {
                let e = parse_expr(&a[6..])?;
                let (body, end) = parse_block(toks, pos)?;
                if end.as_deref() != Some("end") {
                    return Err(TemplateError("missing {{ end }} for range".into()));
                }
                nodes.push(Node::Range(e, body));
            }
            Tok::Action(a) if a.starts_with("/*") => {}
            Tok::Action(a) => nodes.push(Node::Out(parse_expr(a)?)),
        }
    }
    Ok((nodes, None))
}

fn render_nodes(
    nodes: &[Node],
    ctx: &Context,
    dot: Option<&TValue>,
    esc: Option<Escape>,
    out: &mut String,
) -> Result<(), TemplateError> {
    for n in nodes {
        match n {
            Node::Text(s) => out.push_str(s),
            Node::Out(e) => {
                let v = eval(e, ctx, dot)?.render();
                match esc {
                    Some(f) => out.push_str(&f(&v)),
                    None => out.push_str(&v),
                }
            }
            Node::If(branches, otherwise) => {
                let mut done = false;
                for (c, body) in branches {
                    if eval(c, ctx, dot)?.truthy() {
                        render_nodes(body, ctx, dot, esc, out)?;
                        done = true;
                        break;
                    }
                }
                if !done {
                    render_nodes(otherwise, ctx, dot, esc, out)?;
                }
            }
            Node::Range(e, body) => {
                let items = match eval(e, ctx, dot)? {
                    TValue::List(l) => l,
                    TValue::Null => Vec::new(),
                    other => vec![other.render()],
                };
                for it in items {
                    render_nodes(body, ctx, Some(&TValue::Str(it)), esc, out)?;
                }
            }
        }
    }
    Ok(())
}

fn eval(e: &Expr, ctx: &Context, dot: Option<&TValue>) -> Result<TValue, TemplateError> {
    Ok(match e {
        Expr::Path(p) => ctx.lookup(p, dot),
        Expr::Lit(s) => TValue::Str(s.clone()),
        Expr::Call(name, args) => {
            let a = args
                .iter()
                .map(|x| eval(x, ctx, dot))
                .collect::<Result<Vec<_>, _>>()?;
            let arg = |i: usize| a.get(i).cloned().unwrap_or(TValue::Null);
            match name.as_str() {
                "and" => a
                    .iter()
                    .find(|v| !v.truthy())
                    .or(a.last())
                    .cloned()
                    .unwrap_or(TValue::Null),
                "or" => a
                    .iter()
                    .find(|v| v.truthy())
                    .or(a.last())
                    .cloned()
                    .unwrap_or(TValue::Null),
                "not" => TValue::Bool(!arg(0).truthy()),
                "eq" => TValue::Bool(a.len() > 1 && a[1..].iter().any(|v| v.render() == a[0].render())),
                "ne" => TValue::Bool(arg(0).render() != arg(1).render()),
                "join" => match arg(0) {
                    TValue::List(l) => TValue::Str(l.join(&arg(1).render())),
                    v => TValue::Str(v.render()),
                },
                "re_replace" => {
                    let re = super::re::compile(&arg(1).render()).map_err(TemplateError)?;
                    TValue::Str(re.replace_all(&arg(0).render(), &arg(2).render()))
                }
                "len" => TValue::Str(match arg(0) {
                    TValue::List(l) => l.len().to_string(),
                    v => v.render().chars().count().to_string(),
                }),
                other => return Err(TemplateError(format!("unknown function `{other}`"))),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        let mut c = Context::default();
        c.set("Keywords", "the matrix");
        c.set("Config.sort", "size");
        c.set("Config.freeleech", TValue::Bool(false));
        c.set("Categories", TValue::List(vec!["1".into(), "7".into()]));
        c
    }

    #[test]
    fn renders_paths_conditions_and_ranges() {
        let c = ctx();
        assert_eq!(render("q={{ .Keywords }}", &c).unwrap(), "q=the matrix");
        assert_eq!(
            render("{{ if .Config.freeleech }}fl{{ else }}all{{ end }}", &c).unwrap(),
            "all"
        );
        assert_eq!(
            render(
                r#"{{ if eq .Config.sort "date" }}d{{ else if eq .Config.sort "size" }}s{{ end }}"#,
                &c
            )
            .unwrap(),
            "s"
        );
        assert_eq!(
            render("{{ range .Categories }}c[]={{ . }}&{{ end }}", &c).unwrap(),
            "c[]=1&c[]=7&"
        );
        assert_eq!(render(r#"{{ join .Categories "," }}"#, &c).unwrap(), "1,7");
        assert_eq!(
            render(r#"{{ re_replace .Keywords " " "." }}"#, &c).unwrap(),
            "the.matrix"
        );
        assert_eq!(
            render(r#"{{ if and .Keywords (not .Config.freeleech) }}y{{ end }}"#, &c).unwrap(),
            "y"
        );
        assert_eq!(render("a {{- .Missing -}} b", &c).unwrap(), "ab");
        assert_eq!(render("{{ if .Missing }}x{{ end }}", &c).unwrap(), "");
        assert_eq!(
            render("{{ if eq .Config.freeleech .False }}off{{ end }}", &c).unwrap(),
            "off"
        );
        assert_eq!(
            render_urlencoded("s/{{ .Keywords }}/{{ range .Categories }}c={{.}}&{{end}}", &c).unwrap(),
            "s/the+matrix/c=1&c=7&"
        );
    }

    #[test]
    fn rejects_bad_syntax() {
        assert!(Template::parse("{{ if .A }}x").is_err());
        assert!(Template::parse("{{ .A").is_err());
        assert!(render("{{ nosuch .A }}", &ctx()).is_err());
        assert!(
            check("{{ if nosuch .A }}x{{ end }}")
                .unwrap_err()
                .0
                .contains("unsupported")
        );
        assert!(check(r#"{{ if and .A (eq .B "x") }}{{ join .C "," }}{{ end }}"#).is_ok());
    }
}
