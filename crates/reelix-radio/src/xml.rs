//! A tiny owned XML tree on top of quick-xml, enough for podcast feeds (same shape as the
//! indexers' RSS reader, plus the HTML entities feeds put in descriptions).

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

#[derive(Debug, Clone, Default)]
pub(crate) struct Node {
    /// Qualified name, e.g. `itunes:image`.
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub text: String,
    pub children: Vec<Node>,
}

impl Node {
    /// Local part of the name (`itunes:image` → `image`).
    pub fn local(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }

    /// Namespace prefix (`itunes:image` → `itunes`, `title` → ``).
    pub fn prefix(&self) -> &str {
        self.name.rsplit_once(':').map_or("", |(p, _)| p)
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.0.eq_ignore_ascii_case(name))
            .map(|a| a.1.as_str())
    }

    /// First child with this local name and namespace prefix (`""` = none).
    pub fn child_ns(&self, prefix: &str, local: &str) -> Option<&Node> {
        self.children
            .iter()
            .find(|c| c.local().eq_ignore_ascii_case(local) && c.prefix().eq_ignore_ascii_case(prefix))
    }

    pub fn children_named<'a>(&'a self, local: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children
            .iter()
            .filter(move |c| c.local().eq_ignore_ascii_case(local) && c.prefix().is_empty())
    }

    /// Trimmed, non-empty text of the first child with this prefix and local name.
    pub fn text_ns(&self, prefix: &str, local: &str) -> Option<&str> {
        self.child_ns(prefix, local)
            .map(|c| c.text.trim())
            .filter(|t| !t.is_empty())
    }

    pub fn child_text(&self, local: &str) -> Option<&str> {
        self.text_ns("", local)
    }
}

/// Deepest element nesting accepted. Feeds are a handful of levels deep; a hostile feed could
/// otherwise nest elements deeply enough that dropping or cloning the (recursive) tree
/// overflows the stack.
pub(crate) const MAX_DEPTH: usize = 64;

/// Parses a document and returns its root element.
pub(crate) fn parse(input: &str) -> Result<Node, String> {
    let mut reader = Reader::from_str(input);
    let mut stack: Vec<Node> = vec![Node::default()];
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => {
                // `stack[0]` is the synthetic document node.
                if stack.len() > MAX_DEPTH {
                    return Err(format!("XML nested deeper than {MAX_DEPTH} elements"));
                }
                stack.push(element(&e)?);
            }
            Event::Empty(e) => {
                let n = element(&e)?;
                stack.last_mut().ok_or("unbalanced")?.children.push(n);
            }
            Event::End(_) => {
                let n = stack.pop().ok_or("unbalanced")?;
                stack.last_mut().ok_or("unbalanced end tag")?.children.push(n);
            }
            Event::Text(t) => {
                let s = t.xml10_content().map_err(|e| e.to_string())?;
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&s);
                }
            }
            Event::GeneralRef(r) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&reference_text(&r));
                }
            }
            Event::CData(c) => {
                let s = String::from_utf8_lossy(&c.into_inner()).into_owned();
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&s);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err("unexpected end of document".into());
    }
    stack
        .pop()
        .and_then(|mut d| d.children.drain(..).next())
        .ok_or_else(|| "empty document".into())
}

fn element(e: &quick_xml::events::BytesStart<'_>) -> Result<Node, String> {
    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut attrs = Vec::new();
    for a in e.attributes() {
        let a = a.map_err(|e| e.to_string())?;
        let k = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        let v = a
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|e| e.to_string())?
            .into_owned();
        attrs.push((k, v));
    }
    Ok(Node {
        name,
        attrs,
        ..Node::default()
    })
}

/// The text of an entity or character reference: XML's five, numeric references and the
/// HTML entities feeds commonly leave unescaped; anything else is kept verbatim.
fn reference_text(r: &quick_xml::events::BytesRef<'_>) -> String {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return c.to_string();
    }
    let name = r.decode().unwrap_or_default();
    if let Some(text) = quick_xml::escape::resolve_predefined_entity(&name) {
        return text.to_owned();
    }
    let html = match name.as_ref() {
        "nbsp" => "\u{a0}",
        "ndash" => "–",
        "mdash" => "—",
        "hellip" => "…",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "eacute" => "é",
        "egrave" => "è",
        "aacute" => "á",
        "uuml" => "ü",
        "ouml" => "ö",
        "auml" => "ä",
        "pound" => "£",
        "euro" => "€",
        _ => return format!("&{name};"),
    };
    html.to_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_deep_nesting() {
        // Run on a small stack so a recursive Drop/Clone of a deep tree would crash.
        let res = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let depth = 100_000;
                let doc = format!("{}{}", "<a>".repeat(depth), "</a>".repeat(depth));
                super::parse(&doc).map(|_| ())
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(res.unwrap_err().contains("nested"));
        let ok = format!("{}{}", "<a>".repeat(60), "</a>".repeat(60));
        assert!(super::parse(&ok).is_ok());
    }

    #[test]
    fn entities_and_namespaces() {
        let n = super::parse(
            r#"<?xml version="1.0"?><a x="1"><b>hi &amp; bye&nbsp;&mdash;</b><itunes:image href="i"/><d>&#169;&#x41; &foo;</d><![CDATA[raw]]></a>"#,
        )
        .unwrap();
        assert_eq!(n.attr("x"), Some("1"));
        assert_eq!(n.child_text("b"), Some("hi & bye\u{a0}—"));
        assert_eq!(n.child_ns("itunes", "image").unwrap().attr("href"), Some("i"));
        assert!(n.child_ns("", "image").is_none());
        assert_eq!(n.child_text("d"), Some("©A &foo;"));
        assert!(n.text.contains("raw"));
        assert!(super::parse("<a><b></a>").is_err());
    }
}
