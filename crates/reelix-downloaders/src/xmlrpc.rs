//! Minimal XML-RPC encoder/decoder (enough for rTorrent).

use base64::Engine;
use quick_xml::Reader;
use quick_xml::escape::escape;
use quick_xml::events::Event;

use crate::ClientError;

/// An XML-RPC value.
#[derive(Debug, Clone, PartialEq)]
pub enum XValue {
    Int(i64),
    Bool(bool),
    Double(f64),
    Str(String),
    Base64(Vec<u8>),
    Array(Vec<XValue>),
    Struct(Vec<(String, XValue)>),
}

impl XValue {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Bool(b) => Some(i64::from(*b)),
            Self::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[XValue]> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Double(d) => Some(*d),
            Self::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    fn write(&self, out: &mut String) {
        out.push_str("<value>");
        match self {
            Self::Int(i) => out.push_str(&format!("<i8>{i}</i8>")),
            Self::Bool(b) => out.push_str(&format!("<boolean>{}</boolean>", u8::from(*b))),
            Self::Double(d) => out.push_str(&format!("<double>{d}</double>")),
            Self::Str(s) => {
                out.push_str("<string>");
                out.push_str(&escape(s.as_str()));
                out.push_str("</string>");
            }
            Self::Base64(b) => {
                out.push_str("<base64>");
                out.push_str(&base64::engine::general_purpose::STANDARD.encode(b));
                out.push_str("</base64>");
            }
            Self::Array(a) => {
                out.push_str("<array><data>");
                for v in a {
                    v.write(out);
                }
                out.push_str("</data></array>");
            }
            Self::Struct(m) => {
                out.push_str("<struct>");
                for (k, v) in m {
                    out.push_str("<member><name>");
                    out.push_str(&escape(k.as_str()));
                    out.push_str("</name>");
                    v.write(out);
                    out.push_str("</member>");
                }
                out.push_str("</struct>");
            }
        }
        out.push_str("</value>");
    }
}

impl From<&str> for XValue {
    fn from(s: &str) -> Self {
        Self::Str(s.to_owned())
    }
}

impl From<String> for XValue {
    fn from(s: String) -> Self {
        Self::Str(s)
    }
}

impl From<i64> for XValue {
    fn from(i: i64) -> Self {
        Self::Int(i)
    }
}

/// Serialises a `methodCall`.
pub fn encode_call(method: &str, params: &[XValue]) -> String {
    let mut out = String::from("<?xml version=\"1.0\"?><methodCall><methodName>");
    out.push_str(&escape(method));
    out.push_str("</methodName><params>");
    for p in params {
        out.push_str("<param>");
        p.write(&mut out);
        out.push_str("</param>");
    }
    out.push_str("</params></methodCall>");
    out
}

/// Simple element tree used while decoding.
#[derive(Debug, Default)]
struct Node {
    name: String,
    text: String,
    children: Vec<Node>,
}

/// Deepest element nesting accepted. Bounding the tree also bounds the recursion in
/// [`decode_value`] and in dropping the tree, so a hostile server can't overflow the stack.
const MAX_DEPTH: usize = 256;

fn parse_tree(xml: &str) -> Result<Node, ClientError> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<Node> = vec![Node::default()];
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                // `stack[0]` is the synthetic document node.
                if stack.len() > MAX_DEPTH {
                    return Err(ClientError::Parse(format!(
                        "XML-RPC nested deeper than {MAX_DEPTH} elements"
                    )));
                }
                stack.push(Node {
                    name: String::from_utf8_lossy(e.local_name().as_ref()).into_owned(),
                    ..Node::default()
                });
            }
            Ok(Event::Empty(e)) => {
                let n = Node {
                    name: String::from_utf8_lossy(e.local_name().as_ref()).into_owned(),
                    ..Node::default()
                };
                if let Some(top) = stack.last_mut() {
                    top.children.push(n);
                }
            }
            Ok(Event::Text(t)) => {
                let s = t.xml10_content().map_err(|e| ClientError::Parse(e.to_string()))?;
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&s);
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&reference_text(&r));
                }
            }
            Ok(Event::CData(t)) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Ok(Event::End(_)) => {
                let n = stack
                    .pop()
                    .ok_or_else(|| ClientError::Parse("unbalanced XML".into()))?;
                stack
                    .last_mut()
                    .ok_or_else(|| ClientError::Parse("unbalanced XML".into()))?
                    .children
                    .push(n);
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(ClientError::Parse(format!("XML-RPC: {e}"))),
        }
    }
    if stack.len() != 1 {
        return Err(ClientError::Parse("truncated XML-RPC response".into()));
    }
    Ok(stack.pop().unwrap_or_default())
}

impl Node {
    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }
}

fn decode_value(v: &Node) -> Result<XValue, ClientError> {
    let Some(inner) = v.children.first() else {
        return Ok(XValue::Str(v.text.clone())); // untyped = string
    };
    let t = inner.text.trim();
    let bad = |what: &str| ClientError::Parse(format!("bad XML-RPC {what}: {t}"));
    Ok(match inner.name.as_str() {
        "i4" | "i8" | "int" => XValue::Int(t.parse().map_err(|_| bad("int"))?),
        "boolean" => XValue::Bool(t == "1"),
        "double" => XValue::Double(t.parse().map_err(|_| bad("double"))?),
        "string" => XValue::Str(inner.text.clone()),
        "base64" => XValue::Base64(
            base64::engine::general_purpose::STANDARD
                .decode(t.split_whitespace().collect::<String>())
                .map_err(|_| bad("base64"))?,
        ),
        "array" => XValue::Array(
            inner
                .child("data")
                .map(|d| {
                    d.children
                        .iter()
                        .filter(|c| c.name == "value")
                        .map(decode_value)
                        .collect()
                })
                .transpose()?
                .unwrap_or_default(),
        ),
        "struct" => XValue::Struct(
            inner
                .children
                .iter()
                .filter(|m| m.name == "member")
                .map(|m| {
                    let name = m.child("name").map(|n| n.text.clone()).unwrap_or_default();
                    let val = m
                        .child("value")
                        .map(decode_value)
                        .transpose()?
                        .unwrap_or(XValue::Str(String::new()));
                    Ok((name, val))
                })
                .collect::<Result<_, ClientError>>()?,
        ),
        other => return Err(ClientError::Parse(format!("unknown XML-RPC type {other}"))),
    })
}

/// Parses a `methodResponse`; faults become [`ClientError::Api`].
pub fn decode_response(xml: &str) -> Result<XValue, ClientError> {
    let root = parse_tree(xml)?;
    let resp = root
        .child("methodResponse")
        .ok_or_else(|| ClientError::Parse("not an XML-RPC response".into()))?;
    if let Some(fault) = resp.child("fault") {
        let v = fault.child("value").map(decode_value).transpose()?;
        let msg = match v {
            Some(XValue::Struct(m)) => {
                let code = m
                    .iter()
                    .find(|(k, _)| k == "faultCode")
                    .and_then(|(_, v)| v.as_i64())
                    .unwrap_or(0);
                let s = m
                    .iter()
                    .find(|(k, _)| k == "faultString")
                    .and_then(|(_, v)| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                format!("fault {code}: {s}")
            }
            _ => "XML-RPC fault".into(),
        };
        return Err(ClientError::Api(msg));
    }
    let value = resp
        .child("params")
        .and_then(|p| p.child("param"))
        .and_then(|p| p.child("value"))
        .ok_or_else(|| ClientError::Parse("XML-RPC response without value".into()))?;
    decode_value(value)
}

/// The text of an entity or character reference; unknown entities are kept verbatim.
fn reference_text(r: &quick_xml::events::BytesRef<'_>) -> String {
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
    use super::*;

    #[test]
    fn rejects_deep_nesting() {
        // Run on a small stack so recursive decoding or dropping of a deep tree would crash.
        let res = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let depth = 100_000;
                let xml = format!(
                    "<methodResponse><params><param>{}{}</param></params></methodResponse>",
                    "<value><array><data>".repeat(depth),
                    "</data></array></value>".repeat(depth)
                );
                decode_response(&xml).map(|_| ())
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(matches!(res, Err(ClientError::Parse(m)) if m.contains("nested")));
    }

    #[test]
    fn roundtrip_shapes() {
        let call = encode_call(
            "d.multicall2",
            &["".into(), "main".into(), XValue::Base64(b"hi".to_vec())],
        );
        assert!(call.contains("<methodName>d.multicall2</methodName>"));
        assert!(call.contains("<base64>aGk=</base64>"));
        assert!(encode_call("x", &["a<b".into()]).contains("a&lt;b"));

        let resp = r#"<?xml version="1.0" encoding="UTF-8"?>
<methodResponse><params><param><value><array><data>
<value><array><data><value><string>ABC</string></value><value><i8>42</i8></value><value>plain</value></data></array></value>
</data></array></value></param></params></methodResponse>"#;
        let v = decode_response(resp).unwrap();
        let row = v.as_array().unwrap()[0].as_array().unwrap();
        assert_eq!(row[0].as_str(), Some("ABC"));
        assert_eq!(row[1].as_i64(), Some(42));
        assert_eq!(row[2].as_str(), Some("plain"));
    }

    #[test]
    fn faults() {
        let f = r#"<methodResponse><fault><value><struct>
<member><name>faultCode</name><value><i4>-506</i4></value></member>
<member><name>faultString</name><value><string>Method 'x' not defined</string></value></member>
</struct></value></fault></methodResponse>"#;
        let e = decode_response(f).unwrap_err().to_string();
        assert!(e.contains("-506") && e.contains("not defined"), "{e}");
        assert!(decode_response("<html/>").is_err());
    }
}
