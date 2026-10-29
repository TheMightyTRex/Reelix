//! JSON selectors as Cardigann evaluates them: a JSONPath-ish path (`data.items`, `files[0].name`,
//! `$[0].id`, `$`) optionally followed by `:has(…)`, `:not(…)` and `:contains(…)` filters, which
//! may nest (`data:not(promo.factor:contains(0))`).

use serde_json::Value;

/// Resolves a dotted/bracketed path. `""` and `$` are the value itself.
pub fn path<'a>(v: &'a Value, p: &str) -> Option<&'a Value> {
    let p = p.trim();
    let p = p.strip_prefix('$').unwrap_or(p);
    let mut cur = v;
    let mut rest = p;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('.') {
            rest = r;
            continue;
        }
        if let Some(r) = rest.strip_prefix('[') {
            let end = r.find(']')?;
            let key = r[..end].trim();
            rest = &r[end + 1..];
            cur = if let Some(k) = key
                .strip_prefix('\'')
                .and_then(|k| k.strip_suffix('\''))
                .or_else(|| key.strip_prefix('"').and_then(|k| k.strip_suffix('"')))
            {
                cur.get(k)?
            } else {
                let i: usize = key.parse().ok()?;
                cur.get(i)?
            };
            continue;
        }
        let end = rest.find(['.', '[']).unwrap_or(rest.len());
        let seg = &rest[..end];
        rest = &rest[end..];
        cur = match cur {
            Value::Object(o) => o.get(seg)?,
            Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// Splits `base:f1(k1):f2(k2)` into `base` and its filters. A filter's key runs to the first `)`
/// followed by `:` or the end, so nested filters stay inside their parent's key.
fn split(sel: &str) -> (&str, Vec<(&str, &str)>) {
    let Some(colon) = sel.find(':') else {
        return (sel, Vec::new());
    };
    let base = &sel[..colon];
    let mut filters = Vec::new();
    let mut rest = &sel[colon..];
    while let Some(r) = rest.strip_prefix(':') {
        let Some(open) = r.find('(') else { break };
        let name = &r[..open];
        let body = &r[open + 1..];
        let close = body
            .char_indices()
            .skip(1)
            .find(|&(i, c)| c == ')' && (i + 1 == body.len() || body[i + 1..].starts_with(':')));
        let Some((close, _)) = close else { break };
        filters.push((name.trim(), &body[..close]));
        rest = &body[close + 1..];
    }
    (base, filters)
}

/// Evaluates a field/row selector against `v`; `None` when the path is missing or a filter rejects.
pub fn select<'a>(v: &'a Value, sel: &str) -> Option<&'a Value> {
    let (base, filters) = split(sel);
    let target = if base.trim().is_empty() { v } else { path(v, base)? };
    for (name, key) in filters {
        let ok = match name {
            "has" => select(target, key).is_some(),
            "not" => select(target, key).is_none(),
            "contains" => to_text(target).contains(key.trim_matches(|c| c == '"' || c == '\'')),
            _ => false,
        };
        if !ok {
            return None;
        }
    }
    Some(target)
}

/// Rows for a rows selector: the base must name an array (or a single object); filters apply per row.
pub fn rows<'a>(v: &'a Value, sel: &str) -> Vec<&'a Value> {
    let colon = sel.find(':').unwrap_or(sel.len());
    let (base, filters) = sel.split_at(colon);
    let target = if base.trim().is_empty() {
        Some(v)
    } else {
        path(v, base)
    };
    let all: Vec<&Value> = match target {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![other],
    };
    all.into_iter()
        .filter(|r| filters.is_empty() || select(r, filters).is_some())
        .collect()
}

/// .NET `JToken.ToString()`-like text: strings raw, bools `True`/`False`, others as JSON.
pub fn to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A field value: scalars as text, arrays joined with `,`, `null` as missing.
pub fn field_value(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::Array(a) => Some(a.iter().map(to_text).collect::<Vec<_>>().join(",")),
        other => Some(to_text(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paths() {
        let v = json!({"data": {"items": [{"name": "a"}, {"name": "b"}]}, "n": 3});
        assert_eq!(path(&v, "data.items[1].name"), Some(&json!("b")));
        assert_eq!(path(&v, "$.data.items.0.name"), Some(&json!("a")));
        assert_eq!(path(&v, "$['n']"), Some(&json!(3)));
        assert_eq!(path(&v, "$"), Some(&v));
        assert_eq!(path(&v, "data.nope"), None);
        assert_eq!(path(&json!([{"id": 9}]), "$[0].id"), Some(&json!(9)));
    }

    #[test]
    fn filters_nest() {
        let rows_v = json!({"data": [
            {"name": "A", "promo": {"down": 1}, "user": "bob"},
            {"name": "B", "promo": {"down": 0.5}, "user": "amy"},
            {"name": "C", "promo": {"down": 0}, "user": "bob"}
        ]});
        let names =
            |sel: &str| -> Vec<String> { rows(&rows_v, sel).iter().map(|r| to_text(&r["name"])).collect() };
        assert_eq!(names("data"), ["A", "B", "C"]);
        assert_eq!(
            names("data:not(promo.down:contains(1)):not(promo.down:contains(.))"),
            ["C"]
        );
        assert_eq!(names("data:has(user:contains(bob))"), ["A", "C"]);
        assert_eq!(names("data:has(nothing)"), Vec::<String>::new());
        let r = &rows_v["data"][1];
        assert!(select(r, "name:contains(B)").is_some());
        assert!(select(r, "name:contains(Z)").is_none());
        assert!(select(r, "name:not(:contains(\"a\"))").is_some());
        assert_eq!(field_value(&json!(["x", "y"])).as_deref(), Some("x,y"));
        assert_eq!(field_value(&json!(true)).as_deref(), Some("True"));
        assert_eq!(field_value(&Value::Null), None);
    }
}
