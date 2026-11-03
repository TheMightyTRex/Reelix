//! A tiny owned XML tree on top of quick-xml, enough for RSS/caps documents.

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

#[derive(Debug, Clone, Default)]
pub(crate) struct Node {
    /// Qualified name, e.g. `torznab:attr`.
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub text: String,
    pub children: Vec<Node>,
}

impl Node {
    /// Local part of the name (`torznab:attr` → `attr`).
    pub fn local(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.0.eq_ignore_ascii_case(name))
            .map(|a| a.1.as_str())
    }

    pub fn child(&self, local: &str) -> Option<&Node> {
        self.children
            .iter()
            .find(|c| c.local().eq_ignore_ascii_case(local))
    }

    pub fn children_named<'a>(&'a self, local: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children
            .iter()
            .filter(move |c| c.local().eq_ignore_ascii_case(local))
    }

    pub fn child_text(&self, local: &str) -> Option<&str> {
        self.child(local).map(|c| c.text.trim()).filter(|t| !t.is_empty())
    }
}

/// Parses a document and returns its root element.
pub(crate) fn parse(input: &str) -> Result<Node, String> {
    let mut reader = Reader::from_str(input);
    let mut stack: Vec<Node> = vec![Node::default()];
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => stack.push(element(&e)?),
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

/// The text of an entity or character reference; unknown entities are kept verbatim.
pub(crate) fn reference_text(r: &quick_xml::events::BytesRef<'_>) -> String {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return c.to_string();
    }
    let name = r.decode().unwrap_or_default();
    match quick_xml::escape::resolve_predefined_entity(&name) {
        Some(text) => text.to_owned(),
        None => format!("&{name};"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_nested() {
        let n = super::parse(
            r#"<?xml version="1.0"?><a x="1"><b>hi &amp; bye</b><n:c v="2"/><d>&#169;&#x41; &foo;</d><![CDATA[raw]]></a>"#,
        )
        .unwrap();
        assert_eq!(n.attr("x"), Some("1"));
        assert_eq!(n.child_text("b"), Some("hi & bye"));
        assert_eq!(n.child("c").unwrap().attr("v"), Some("2"));
        assert_eq!(n.child_text("d"), Some("©A &foo;"));
        assert!(n.text.contains("raw"));
        assert!(super::parse("<a><b></a>").is_err());
    }
}
