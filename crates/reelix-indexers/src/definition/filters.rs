//! Field filters (`replace`, `re_replace`, `regexp`, `split`, `trim`, `dateparse`, `timeago`, …).

use chrono::format::{Parsed, StrftimeItems, parse};
use chrono::{DateTime, Datelike, Duration, Utc};

use super::jsonsel;
use super::model::FilterDef;
use super::re;
use super::template::{Context, render};
use crate::util::{parse_date, parse_time_ago};

/// Every filter name the engine understands.
pub const KNOWN: &[&str] = &[
    "replace",
    "re_replace",
    "regexp",
    "split",
    "trim",
    "append",
    "prepend",
    "tolower",
    "toupper",
    "urldecode",
    "urlencode",
    "querystring",
    "htmldecode",
    "htmlencode",
    "dateparse",
    "timeparse",
    "timeago",
    "reltime",
    "fuzzytime",
    "diacritics",
    "validfilename",
    "validate",
    "jsonjoinarray",
    "andmatch",
    "strdump",
    "hexdump",
];

/// Load-time check: known name, and literal regex arguments compile.
pub fn check(f: &FilterDef) -> Result<(), String> {
    if !KNOWN.contains(&f.name.as_str()) {
        return Err(format!("unsupported: filter `{}`", f.name));
    }
    if matches!(f.name.as_str(), "re_replace" | "regexp")
        && let Some(p) = f.args().first().filter(|p| !p.contains("{{"))
    {
        re::compile(p)?;
    }
    Ok(())
}

/// Applies filters in order. Unknown filters are an error so broken definitions surface.
pub fn apply(
    mut v: String,
    filters: &[FilterDef],
    ctx: &Context,
    now: DateTime<Utc>,
) -> Result<String, String> {
    for f in filters {
        // Filter arguments may themselves be templates (e.g. `{{ .Config.sitelink }}`).
        let args: Vec<String> = f
            .args()
            .into_iter()
            .map(|a| render(&a, ctx).unwrap_or(a))
            .collect();
        let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
        v = match f.name.as_str() {
            "replace" => v.replace(&arg(0), &arg(1)),
            "re_replace" => re::compile(&arg(0))?.replace_all(&v, &arg(1)),
            "regexp" => {
                let r = re::compile(&arg(0))?;
                r.group(&v, 1).or_else(|| r.group(&v, 0)).unwrap_or_default()
            }
            "split" => {
                let parts: Vec<&str> = v.split(arg(0).as_str()).collect();
                let i: i64 = arg(1).trim().parse().unwrap_or(0);
                let idx = if i < 0 { parts.len() as i64 + i } else { i };
                usize::try_from(idx)
                    .ok()
                    .and_then(|i| parts.get(i))
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            }
            "trim" => match args.first().filter(|a| !a.is_empty()) {
                Some(cut) => v.trim_matches(|c| cut.contains(c)).to_string(),
                None => v.trim().to_string(),
            },
            "append" => v + &arg(0),
            "prepend" => arg(0) + &v,
            "tolower" => v.to_lowercase(),
            "toupper" => v.to_uppercase(),
            "urldecode" => url::form_urlencoded::parse(format!("x={}", v.replace('&', "%26")).as_bytes())
                .next()
                .map(|(_, s)| s.into_owned())
                .unwrap_or_default(),
            "urlencode" => url::form_urlencoded::byte_serialize(v.as_bytes()).collect(),
            "querystring" => query_param(&v, &arg(0)).unwrap_or_default(),
            "htmldecode" => html_decode(&v),
            "htmlencode" => v
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;"),
            "dateparse" | "timeparse" => {
                let layout = arg(0);
                let d = parse_layout(v.trim(), &layout, now)
                    .or_else(|| from_unknown(&v, now))
                    .ok_or_else(|| format!("unparseable date `{v}` (layout `{layout}`)"))?;
                d.to_rfc3339()
            }
            "timeago" | "reltime" | "fuzzytime" => from_unknown(&v, now)
                .map(|d| d.to_rfc3339())
                .ok_or_else(|| format!("unparseable relative time `{v}`"))?,
            "diacritics" => remove_diacritics(&v),
            "validfilename" => v
                .chars()
                .map(|c| {
                    if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                        '_'
                    } else {
                        c
                    }
                })
                .collect(),
            "validate" => validate(&v, &arg(0)),
            "jsonjoinarray" => {
                let j: serde_json::Value =
                    serde_json::from_str(&v).map_err(|e| format!("jsonjoinarray: bad JSON: {e}"))?;
                match jsonsel::path(&j, &arg(0)) {
                    Some(serde_json::Value::Array(a)) => {
                        a.iter().map(jsonsel::to_text).collect::<Vec<_>>().join(&arg(1))
                    }
                    Some(other) => jsonsel::to_text(other),
                    None => String::new(),
                }
            }
            // `andmatch` is a row filter (handled by the engine); the rest are debugging aids.
            "andmatch" | "strdump" | "hexdump" => v,
            other => return Err(format!("unsupported: filter `{other}`")),
        };
    }
    Ok(v)
}

/// Keeps the words of `v` that appear in the comma-separated `allowed` list (lowercased, like Cardigann).
fn validate(v: &str, allowed: &str) -> String {
    const DELIMS: &[char] = &[',', ' ', '/', ')', '(', '.', ';', '[', ']', '"', '|', ':'];
    let allowed: Vec<String> = allowed
        .to_lowercase()
        .split(DELIMS)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let mut out: Vec<String> = Vec::new();
    for w in v.to_lowercase().split(DELIMS).filter(|s| !s.is_empty()) {
        if allowed.iter().any(|a| a == w) && !out.iter().any(|o| o == w) {
            out.push(w.to_string());
        }
    }
    out.join(",")
}

fn remove_diacritics(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    use unicode_normalization::char::is_combining_mark;
    s.nfd().filter(|c| !is_combining_mark(*c)).nfc().collect()
}

/// Value of query parameter `name` in a (possibly relative) URL.
pub fn query_param(u: &str, name: &str) -> Option<String> {
    let q = u.split_once('?').map_or(u, |(_, q)| q);
    let q = q.split('#').next().unwrap_or(q);
    url::form_urlencoded::parse(q.as_bytes())
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

/// Decodes named (common) and numeric HTML entities.
pub fn html_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(';').map(|e| e + 1).filter(|&e| e <= 10);
        let decoded = end.and_then(|e| {
            let ent = &rest[1..e];
            let c = if let Some(n) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                u32::from_str_radix(n, 16).ok().and_then(char::from_u32)
            } else if let Some(n) = ent.strip_prefix('#') {
                n.parse().ok().and_then(char::from_u32)
            } else {
                match ent {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    _ => None,
                }
            };
            c.map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Cardigann's `DateTimeUtil.FromUnknown`: absolute formats, unix times, relative times and
/// `today`/`yesterday` with an optional time of day.
pub fn from_unknown(s: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(d) = parse_date(t, now) {
        return Some(d);
    }
    let lower = t.to_lowercase();
    for (word, days) in [("today", 0), ("yesterday", 1), ("tomorrow", -1)] {
        if let Some(rest) = lower.strip_prefix(word) {
            let rest = rest
                .trim_start_matches([',', ' '])
                .trim_start_matches("at")
                .trim();
            let date = (now - Duration::days(days)).date_naive();
            if rest.is_empty() {
                return Some(date.and_hms_opt(0, 0, 0)?.and_utc());
            }
            for f in ["%H:%M:%S", "%H:%M", "%I:%M %p", "%I:%M:%S %p"] {
                if let Ok(time) = chrono::NaiveTime::parse_from_str(rest, f) {
                    return Some(date.and_time(time).and_utc());
                }
            }
        }
    }
    parse_time_ago(t, now)
}

/// Parses with a .NET custom format (`yyyy-MM-dd HH:mm zzz`) or, when the layout contains digits,
/// a Go reference layout (`2006-01-02 15:04`). Missing parts default like .NET's `ParseExact`:
/// current year, midnight, today's date.
pub fn parse_layout(v: &str, layout: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if layout.is_empty() {
        return None;
    }
    let fmt = if layout.chars().any(|c| c.is_ascii_digit()) {
        go_layout_to_chrono(layout)
    } else {
        dotnet_layout_to_chrono(layout)
    };
    let mut p = Parsed::new();
    parse(&mut p, v, StrftimeItems::new(&fmt)).ok()?;
    let has_date =
        p.year().is_some() || p.year_mod_100().is_some() || p.month().is_some() || p.day().is_some();
    if !has_date {
        let today = now.date_naive();
        p.set_year(today.year().into()).ok()?;
        p.set_month(today.month().into()).ok()?;
        p.set_day(today.day().into()).ok()?;
    }
    if p.year().is_none() && p.year_mod_100().is_none() {
        p.set_year(now.year().into()).ok()?;
    }
    if p.day().is_none() {
        p.set_day(1).ok()?;
    }
    if p.hour_div_12().is_none() {
        p.set_ampm(false).ok()?;
    }
    if p.hour_mod_12().is_none() {
        p.set_hour12(12).ok()?;
    }
    if p.minute().is_none() {
        p.set_minute(0).ok()?;
    }
    let offset = p.offset().unwrap_or(0);
    let naive = p.to_naive_datetime_with_offset(0).ok()?;
    Some((naive - Duration::seconds(offset.into())).and_utc())
}

/// Converts a .NET custom date/time format string to a chrono format string.
pub fn dotnet_layout_to_chrono(layout: &str) -> String {
    let chars: Vec<char> = layout.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let lit = |out: &mut String, c: char| {
        if c == '%' {
            out.push_str("%%");
        } else {
            out.push(c);
        }
    };
    while i < chars.len() {
        let c = chars[i];
        let mut n = 1;
        while i + n < chars.len() && chars[i + n] == c {
            n += 1;
        }
        let spec = match c {
            'y' => Some(if n <= 2 { "%y" } else { "%Y" }),
            'M' => Some(match n {
                1 | 2 => "%m",
                3 => "%b",
                _ => "%B",
            }),
            'd' => Some(match n {
                1 | 2 => "%d",
                3 => "%a",
                _ => "%A",
            }),
            'H' => Some("%H"),
            'h' => Some("%I"),
            'm' => Some("%M"),
            's' => Some("%S"),
            't' => Some("%p"),
            'z' => Some(if n >= 3 { "%:z" } else { "%#z" }),
            'K' => Some("%:z"),
            'f' | 'F' => {
                if out.ends_with('.') {
                    out.pop();
                    Some("%.f")
                } else {
                    Some(match n {
                        1..=3 => "%3f",
                        4..=6 => "%6f",
                        _ => "%9f",
                    })
                }
            }
            _ => None,
        };
        if let Some(s) = spec {
            out.push_str(s);
            i += n;
            continue;
        }
        match c {
            '\'' | '"' => {
                i += 1;
                while i < chars.len() && chars[i] != c {
                    lit(&mut out, chars[i]);
                    i += 1;
                }
                i += 1;
            }
            '\\' => {
                if let Some(&next) = chars.get(i + 1) {
                    lit(&mut out, next);
                }
                i += 2;
            }
            _ => {
                lit(&mut out, c);
                i += 1;
            }
        }
    }
    out
}

/// Converts a Go reference-time layout (`2006-01-02 15:04:05`) to a chrono format string.
pub fn go_layout_to_chrono(layout: &str) -> String {
    const MAP: &[(&str, &str)] = &[
        ("January", "%B"),
        ("Monday", "%A"),
        ("Z07:00", "%:z"),
        ("-07:00", "%:z"),
        ("-0700", "%z"),
        ("2006", "%Y"),
        ("Jan", "%b"),
        ("Mon", "%a"),
        ("MST", "%Z"),
        (".000", "%.3f"),
        ("01", "%m"),
        ("02", "%d"),
        ("_2", "%e"),
        ("03", "%I"),
        ("04", "%M"),
        ("05", "%S"),
        ("06", "%y"),
        ("15", "%H"),
        ("PM", "%p"),
        ("pm", "%P"),
        ("1", "%m"),
        ("2", "%d"),
        ("3", "%I"),
    ];
    let mut out = String::new();
    let mut rest = layout;
    'outer: while !rest.is_empty() {
        for (go, ch) in MAP {
            if let Some(r) = rest.strip_prefix(go) {
                out.push_str(ch);
                rest = r;
                continue 'outer;
            }
        }
        let c = rest.chars().next().unwrap_or_default();
        if c == '%' {
            out.push_str("%%");
        } else {
            out.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn f(name: &str, args: yaml_serde::Value) -> FilterDef {
        FilterDef {
            name: name.into(),
            args: Some(args),
        }
    }

    fn noarg(name: &str) -> FilterDef {
        FilterDef {
            name: name.into(),
            args: None,
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap()
    }

    fn run(v: &str, filters: &[FilterDef]) -> String {
        apply(v.into(), filters, &Context::default(), now()).unwrap()
    }

    fn s(v: &str) -> yaml_serde::Value {
        yaml_serde::Value::String(v.into())
    }

    fn seq(a: &[&str]) -> yaml_serde::Value {
        yaml_serde::Value::Sequence(a.iter().map(|x| s(x)).collect())
    }

    #[test]
    fn string_filters() {
        assert_eq!(run("a.b.c", &[f("replace", seq(&[".", " "]))]), "a b c");
        assert_eq!(
            run("Size: 1.2 GB", &[f("regexp", s(r"([\d.]+ [KMGT]B)"))]),
            "1.2 GB"
        );
        assert_eq!(run("x|y|z", &[f("split", seq(&["|", "-1"]))]), "z");
        assert_eq!(run("[abc]", &[f("trim", s("[]"))]), "abc");
        assert_eq!(run("details.php?id=42&x=1", &[f("querystring", s("id"))]), "42");
        assert_eq!(run("a%20b+c", &[noarg("urldecode")]), "a b c");
        assert_eq!(
            run("Show 2026", &[f("re_replace", seq(&[r"\s+(\d{4})$", " ($1)"]))]),
            "Show (2026)"
        );
        assert_eq!(
            run("S01E02x", &[f("re_replace", seq(&[r"S(\d+)E(\d+)", "$1x$2"]))]),
            "01x02x"
        );
        assert_eq!(run("x", &[f("append", s("y")), f("prepend", s("w"))]), "wxy");
        assert_eq!(
            run("a &amp; b &#39;c&#x27; &bogus; &", &[noarg("htmldecode")]),
            "a & b 'c' &bogus; &"
        );
    }

    #[test]
    fn fancy_regex_filters() {
        let multi = f(
            "re_replace",
            seq(&[r"(?i)\b(MULTI(?!.*(?:FRENCH|ENGLISH)))\b", "MULTI.FRENCH"]),
        );
        assert_eq!(
            run("Film.MULTI.1080p", std::slice::from_ref(&multi)),
            "Film.MULTI.FRENCH.1080p"
        );
        assert_eq!(run("Film.MULTI.ENGLISH", &[multi]), "Film.MULTI.ENGLISH");
        assert_eq!(
            run("1 024 MB", &[f("re_replace", seq(&[r"(?<=\d)\s+(?=\d)", ""]))]),
            "1024 MB"
        );
        assert_eq!(
            run("Movie (Tags: x)", &[f("regexp", s(r"(.+?)(?= \(Tags)"))]),
            "Movie"
        );
        assert!(check(&f("regexp", s(r"(?<=a)b"))).is_ok());
        assert!(check(&f("regexp", s(r"(unclosed"))).is_err());
        assert!(
            check(&noarg("bogus"))
                .unwrap_err()
                .contains("unsupported: filter `bogus`")
        );
    }

    #[test]
    fn misc_filters() {
        assert_eq!(
            run("Amélie à Noël", &[f("diacritics", s("replace"))]),
            "Amelie a Noel"
        );
        assert_eq!(run("a/b:c*?.mkv", &[noarg("validfilename")]), "a_b_c__.mkv");
        assert_eq!(
            run(
                "Drama, horror / Sci-Fi, Unknown",
                &[f("validate", s("Action, Drama, Horror, Sci-Fi"))]
            ),
            "drama,horror,sci-fi"
        );
        assert_eq!(
            run(
                r#"{"genres":["Action","Drama"]}"#,
                &[f("jsonjoinarray", seq(&["$.genres", ", "]))]
            ),
            "Action, Drama"
        );
        assert_eq!(
            run(r#"{"v":["aé\/b"]}"#, &[f("jsonjoinarray", seq(&["$.v", ""]))]),
            "aé/b"
        );
        assert_eq!(run("x", &[noarg("strdump"), noarg("andmatch")]), "x");
        assert_eq!(run("a b", &[noarg("urlencode")]), "a+b");
    }

    #[test]
    fn date_filters() {
        assert_eq!(go_layout_to_chrono("2006-01-02 15:04:05"), "%Y-%m-%d %H:%M:%S");
        assert_eq!(
            dotnet_layout_to_chrono("yyyy-MM-dd HH:mm:ss zzz"),
            "%Y-%m-%d %H:%M:%S %:z"
        );
        assert_eq!(
            dotnet_layout_to_chrono("ddd, dd MMM yyyy 'at' h tt"),
            "%a, %d %b %Y at %I %p"
        );
        let d = |v: &str, l: &str| run(v, &[f("dateparse", s(l))]);
        assert_eq!(
            d("28/04/2026 07:15", "02/01/2006 15:04"),
            "2026-04-28T07:15:00+00:00"
        );
        assert_eq!(d("Apr 28 2026", "Jan 02 2006"), "2026-04-28T00:00:00+00:00");
        assert_eq!(
            d("2026-04-28 07:15:00 +02:00", "yyyy-MM-dd HH:mm:ss zzz"),
            "2026-04-28T05:15:00+00:00"
        );
        assert_eq!(
            d("2026-04-2807:15:00 -07:00", "yyyy-MM-ddHH:mm:ss zzz"),
            "2026-04-28T14:15:00+00:00"
        );
        assert_eq!(
            d("04/28/2026 7:05:09 +00:00", "MM/dd/yyyy HH:mm:ss zzz"),
            "2026-04-28T07:05:09+00:00"
        );
        assert_eq!(
            d(
                "Tue, 28 Apr 2026 07:15:00 +01:00",
                "ddd, dd MMM yyyy HH:mm:ss zzz"
            ),
            "2026-04-28T06:15:00+00:00"
        );
        assert_eq!(
            d("Apr 3 2026 07:15 PM", "MMM d yyyy hh:mm tt"),
            "2026-04-03T19:15:00+00:00"
        );
        assert_eq!(d("28.04.2026", "dd.MM.yyyy"), "2026-04-28T00:00:00+00:00");
        assert_eq!(d("Mar 9, 14:20", "MMM d, HH:mm"), "2026-03-09T14:20:00+00:00");
        assert_eq!(
            d("Tuesday 28 April 2026", "dddd dd MMMM yyyy"),
            "2026-04-28T00:00:00+00:00"
        );
        assert_eq!(
            d("260428 01:02:03", "yyMMdd HH:mm:ss"),
            "2026-04-28T01:02:03+00:00"
        );
        assert_eq!(
            run("2 hours ago", &[noarg("timeago")]),
            "2026-05-01T10:00:00+00:00"
        );
        assert_eq!(
            run("Yesterday 10:30", &[noarg("fuzzytime")]),
            "2026-04-30T10:30:00+00:00"
        );
        assert_eq!(
            run("today, 08:00", &[noarg("fuzzytime")]),
            "2026-05-01T08:00:00+00:00"
        );
        assert!(
            apply(
                "garbage".into(),
                &[f("dateparse", s("yyyy"))],
                &Context::default(),
                now()
            )
            .is_err()
        );
    }
}
