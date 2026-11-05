//! A small, conservative SVG cleaner for station logos and artwork.
//!
//! Allow-list based: only plain drawing elements and presentation attributes survive.
//! Removed: `<script>`, `<foreignObject>`, `<image>`, `<a>`, animation and every other element
//! not on the list (with everything inside it), event handlers (`on*`), `href`/`xlink:href`
//! other than same-document `#id` references, any value with `javascript:`, `data:`,
//! `expression(` or a `url()` that is not `url(#id)`, `<style>` with `url()`/`@import`,
//! comments, processing instructions and attributes in other namespaces. A document with a
//! DOCTYPE (entity tricks), that is not well-formed, nests deeper than 64 elements or whose
//! root is not `<svg>` is rejected. The output is re-serialised from the parsed events, so
//! nothing unparsed passes through.

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

pub const SVG_NS: &str = "http://www.w3.org/2000/svg";
pub const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
const MAX_DEPTH: usize = 64;

const ELEMENTS: &[&str] = &[
    "svg",
    "g",
    "defs",
    "symbol",
    "use",
    "title",
    "desc",
    "path",
    "rect",
    "circle",
    "ellipse",
    "line",
    "polyline",
    "polygon",
    "text",
    "tspan",
    "linearGradient",
    "radialGradient",
    "stop",
    "clipPath",
    "mask",
    "pattern",
    "style",
];

const ATTRIBUTES: &[&str] = &[
    "id",
    "class",
    "style",
    "version",
    "viewBox",
    "preserveAspectRatio",
    "width",
    "height",
    "x",
    "y",
    "x1",
    "y1",
    "x2",
    "y2",
    "cx",
    "cy",
    "r",
    "rx",
    "ry",
    "fx",
    "fy",
    "dx",
    "dy",
    "d",
    "points",
    "pathLength",
    "transform",
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-width",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dasharray",
    "stroke-dashoffset",
    "stroke-opacity",
    "opacity",
    "color",
    "display",
    "visibility",
    "clip-path",
    "clip-rule",
    "clipPathUnits",
    "mask",
    "maskUnits",
    "maskContentUnits",
    "patternUnits",
    "patternContentUnits",
    "patternTransform",
    "gradientUnits",
    "gradientTransform",
    "spreadMethod",
    "offset",
    "stop-color",
    "stop-opacity",
    "font-family",
    "font-size",
    "font-weight",
    "font-style",
    "text-anchor",
    "dominant-baseline",
    "letter-spacing",
    "word-spacing",
    "textLength",
    "lengthAdjust",
    "role",
    "aria-label",
    "aria-hidden",
    "focusable",
    "href",
    "xlink:href",
    "xml:space",
    "xmlns",
    "xmlns:xlink",
];

/// Whether a value is free of script and outside references. `url(#id)` is allowed.
fn safe_value(v: &str) -> bool {
    let squashed: String = v
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .flat_map(char::to_lowercase)
        .collect();
    if [
        "javascript:",
        "vbscript:",
        "data:",
        "expression(",
        "@import",
        "behavior:",
        "-moz-binding",
        "\\",
    ]
    .iter()
    .any(|bad| squashed.contains(bad))
    {
        return false;
    }
    let mut rest = squashed.as_str();
    while let Some(i) = rest.find("url(") {
        rest = &rest[i + 4..];
        let inner = rest.trim_start_matches(['"', '\'']);
        if !inner.starts_with('#') {
            return false;
        }
    }
    true
}

/// `#id` (a same-document reference).
fn internal_ref(v: &str) -> bool {
    let v = v.trim();
    v.len() > 1
        && v.starts_with('#')
        && v[1..]
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// The kept attributes of `e`, escaped and ready to write.
fn attributes(e: &BytesStart<'_>, root: bool) -> Result<String, String> {
    let mut out = String::new();
    let mut has_ns = false;
    let mut has_xlink = false;
    for a in e.attributes() {
        let a = a.map_err(|e| e.to_string())?;
        let key = std::str::from_utf8(a.key.as_ref()).map_err(|e| e.to_string())?;
        let value = a
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|e| e.to_string())?;
        let lower = key.to_ascii_lowercase();
        let local = lower.rsplit(':').next().unwrap_or(&lower);
        if local.starts_with("on") || !ATTRIBUTES.contains(&key) {
            continue;
        }
        let keep = match key {
            "xmlns" => value == SVG_NS,
            "xmlns:xlink" => value == XLINK_NS,
            "href" | "xlink:href" => internal_ref(&value),
            _ => safe_value(&value),
        };
        if !keep {
            continue;
        }
        has_ns |= key == "xmlns";
        has_xlink |= key == "xmlns:xlink";
        out.push(' ');
        out.push_str(key);
        out.push_str("=\"");
        out.push_str(&quick_xml::escape::escape(value.as_ref()));
        out.push('"');
    }
    if root {
        // Served as image/svg+xml, the document needs its namespaces.
        if !has_ns {
            out.push_str(&format!(" xmlns=\"{SVG_NS}\""));
        }
        if !has_xlink {
            out.push_str(&format!(" xmlns:xlink=\"{XLINK_NS}\""));
        }
    }
    Ok(out)
}

fn element_name(e: &BytesStart<'_>) -> Result<String, String> {
    std::str::from_utf8(e.name().as_ref())
        .map(str::to_owned)
        .map_err(|e| e.to_string())
}

/// A cleaned copy of the SVG document, or why it was refused.
pub fn sanitise(input: &[u8]) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(input).map_err(|_| "the SVG is not UTF-8 text".to_owned())?;
    let mut reader = Reader::from_str(text.trim_start_matches('\u{feff}'));
    let mut out = String::with_capacity(text.len());
    // Open, kept elements.
    let mut stack: Vec<String> = Vec::new();
    // > 0 while inside a dropped element (its nesting depth).
    let mut skip = 0usize;
    let mut root_done = false;
    // Text of an open <style>, checked before it is written.
    let mut style: Option<String> = None;
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| format!("the SVG is not well-formed: {e}"))?;
        match ev {
            Event::DocType(_) => return Err("SVG with a DOCTYPE is not accepted".into()),
            // Inside a dropped element, or after the root: skipped with everything inside.
            Event::Start(_) if skip > 0 || root_done => skip += 1,
            Event::Empty(_) if skip > 0 || root_done => {}
            Event::Start(e) => {
                let name = element_name(&e)?;
                if stack.len() >= MAX_DEPTH {
                    return Err(format!("the SVG is nested deeper than {MAX_DEPTH} elements"));
                }
                if stack.is_empty() && name != "svg" {
                    return Err("the file is not an SVG image".into());
                }
                if style.is_some() || !ELEMENTS.contains(&name.as_str()) {
                    skip = 1;
                    continue;
                }
                out.push('<');
                out.push_str(&name);
                out.push_str(&attributes(&e, stack.is_empty())?);
                out.push('>');
                if name == "style" {
                    style = Some(String::new());
                }
                stack.push(name);
            }
            Event::Empty(e) => {
                let name = element_name(&e)?;
                if stack.is_empty() {
                    if name != "svg" {
                        return Err("the file is not an SVG image".into());
                    }
                    root_done = true;
                }
                if style.is_some() || !ELEMENTS.contains(&name.as_str()) || name == "style" {
                    continue;
                }
                out.push('<');
                out.push_str(&name);
                out.push_str(&attributes(&e, stack.is_empty())?);
                out.push_str("/>");
            }
            Event::End(_) if skip > 0 => skip -= 1,
            Event::End(_) => {
                let Some(name) = stack.pop() else {
                    return Err("the SVG is not well-formed".into());
                };
                if name == "style"
                    && let Some(css) = style.take()
                {
                    if safe_value(&css) {
                        out.push_str(&quick_xml::escape::escape(css.as_str()));
                        out.push_str("</style>");
                    } else {
                        // Drop the whole element, including its already written start tag.
                        let start = out.rfind("<style").unwrap_or(out.len());
                        out.truncate(start);
                    }
                } else {
                    out.push_str("</");
                    out.push_str(&name);
                    out.push('>');
                }
                if stack.is_empty() {
                    root_done = true;
                }
            }
            Event::Text(t) if skip == 0 && !stack.is_empty() => {
                let s = t.xml10_content().map_err(|e| e.to_string())?;
                push_text(&mut out, &mut style, &s);
            }
            Event::CData(c) if skip == 0 && !stack.is_empty() => {
                let s = String::from_utf8_lossy(&c.into_inner()).into_owned();
                push_text(&mut out, &mut style, &s);
            }
            Event::GeneralRef(r) if skip == 0 && !stack.is_empty() => {
                // Character references and XML's five entities; anything else is dropped.
                let s = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => {
                        let name = r.decode().unwrap_or_default();
                        quick_xml::escape::resolve_predefined_entity(&name)
                            .unwrap_or_default()
                            .to_owned()
                    }
                };
                push_text(&mut out, &mut style, &s);
            }
            Event::Eof => break,
            // Declarations, comments, processing instructions, text outside the root.
            _ => {}
        }
    }
    if !root_done || !stack.is_empty() {
        return Err("the SVG is incomplete".into());
    }
    Ok(out.into_bytes())
}

fn push_text(out: &mut String, style: &mut Option<String>, s: &str) {
    match style {
        Some(css) => css.push_str(s),
        None => out.push_str(&quick_xml::escape::escape(s)),
    }
}

#[cfg(test)]
mod tests {
    use super::sanitise;

    fn clean(s: &str) -> String {
        String::from_utf8(sanitise(s.as_bytes()).unwrap()).unwrap()
    }

    #[test]
    fn keeps_drawing() {
        let out = clean(
            r##"<?xml version="1.0"?><!-- logo --><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><defs><linearGradient id="g"><stop offset="0" stop-color="#f00"/></linearGradient></defs><rect width="10" height="10" fill="url(#g)"/><text x="1" y="5">R&amp;4 &#169;</text></svg>"##,
        );
        assert_eq!(
            out,
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10" xmlns:xlink="http://www.w3.org/1999/xlink"><defs><linearGradient id="g"><stop offset="0" stop-color="#f00"/></linearGradient></defs><rect width="10" height="10" fill="url(#g)"/><text x="1" y="5">R&amp;4 ©</text></svg>"##
        );
    }

    #[test]
    fn removes_scripts_and_foreign_content() {
        let out = clean(
            r#"<svg><script>alert(1)</script><foreignObject><div xmlns="http://www.w3.org/1999/xhtml"><script>x()</script></div></foreignObject><g><path d="M0 0"/></g><image href="https://evil.example/x.png"/><a href="javascript:alert(1)"><rect/></a></svg>"#,
        );
        assert!(!out.contains("script"), "{out}");
        assert!(!out.contains("foreignObject"), "{out}");
        assert!(!out.contains("evil"), "{out}");
        assert!(!out.contains("javascript"), "{out}");
        assert!(out.contains(r#"<g><path d="M0 0"/></g>"#), "{out}");
    }

    #[test]
    fn removes_event_handlers() {
        let out = clean(r#"<svg onload="alert(1)"><rect width="1" onclick="x()" OnMouseOver="y()"/></svg>"#);
        assert!(
            !out.contains("onload") && !out.contains("onclick") && !out.contains("OnMouse"),
            "{out}"
        );
        assert!(out.contains(r#"<rect width="1"/>"#), "{out}");
    }

    #[test]
    fn removes_outside_references() {
        let out = clean(
            r##"<svg xmlns:xlink="http://www.w3.org/1999/xlink"><use xlink:href="https://evil.example/s.svg#a"/><use href="#ok"/><use xlink:href="data:image/svg+xml;base64,PHN2Zz4="/><rect fill="url(https://evil.example/p)" style="fill: url( 'http://evil.example/x' )"/><rect style="fill:red"/></svg>"##,
        );
        assert!(!out.contains("evil"), "{out}");
        assert!(!out.contains("data:"), "{out}");
        assert!(out.contains(r##"<use href="#ok"/>"##), "{out}");
        assert!(out.contains(r#"<rect style="fill:red"/>"#), "{out}");
    }

    #[test]
    fn styles_with_urls_are_dropped() {
        let out = clean(
            r#"<svg><style>rect { fill: url(https://evil.example/x) }</style><style>@import "x.css";</style><style>.a { fill: #fff }</style></svg>"#,
        );
        assert!(!out.contains("evil") && !out.contains("@import"), "{out}");
        assert!(out.contains("<style>.a { fill: #fff }</style>"), "{out}");
    }

    #[test]
    fn refuses_bad_documents() {
        assert!(sanitise(b"<!DOCTYPE svg [<!ENTITY x \"y\">]><svg>&x;</svg>").is_err());
        assert!(sanitise(b"<html><body/></html>").is_err());
        assert!(sanitise(b"<svg><g></svg>").is_err());
        assert!(sanitise(b"<svg>").is_err());
        assert!(sanitise(b"not xml").is_err());
        assert!(sanitise(&[0xff, 0xfe]).is_err());
        let deep = format!("<svg>{}{}</svg>", "<g>".repeat(70), "</g>".repeat(70));
        assert!(sanitise(deep.as_bytes()).is_err());
    }

    #[test]
    fn unknown_entities_are_dropped() {
        assert_eq!(
            clean("<svg><title>a&nbsp;b&lt;</title></svg>"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><title>ab&lt;</title></svg>"#
        );
    }
}
