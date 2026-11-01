//! EPUB OPF metadata: identifiers for matching, and metadata/cover writing.
//!
//! Writing replaces `dc:title`, `dc:creator` and the `calibre:series*` / cover metas inside
//! `<metadata>`, adds a `urn:isbn:` identifier when missing and stores the cover as
//! `reelix-cover.{jpg,png}` next to the OPF. The archive is rewritten to a temp file (with
//! `mimetype` first and stored) and renamed over the original.

use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::{BookTags, image_mime};

const COVER_ID: &str = "reelix-cover";

/// Metadata read from an EPUB's OPF.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EpubInfo {
    pub title: Option<String>,
    pub creators: Vec<String>,
    /// ISBN-10/13 digits found in `dc:identifier`s.
    pub isbns: Vec<String>,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Largest container.xml / OPF read from an EPUB (they're a few KB; this stops zip bombs).
const MAX_XML_BYTES: u64 = 8 * 1024 * 1024;

/// Reads a zip entry as text, failing when it's bigger than [`MAX_XML_BYTES`].
fn read_entry<R: Read + std::io::Seek>(zip: &mut ZipArchive<R>, name: &str) -> Result<String, String> {
    let entry = zip.by_name(name).map_err(err)?;
    let mut text = String::new();
    entry
        .take(MAX_XML_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(err)?;
    if text.len() as u64 > MAX_XML_BYTES {
        return Err(format!(
            "{name} is larger than {} MB",
            MAX_XML_BYTES / (1024 * 1024)
        ));
    }
    Ok(text)
}

fn opf_path<R: Read + std::io::Seek>(zip: &mut ZipArchive<R>) -> Result<String, String> {
    let container = read_entry(zip, "META-INF/container.xml")?;
    let mut reader = Reader::from_str(&container);
    loop {
        match reader.read_event().map_err(err)? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"rootfile" => {
                for a in e.attributes().flatten() {
                    if a.key.local_name().as_ref() == b"full-path" {
                        return Ok(a.unescape_value().map_err(err)?.into_owned());
                    }
                }
            }
            Event::Eof => return Err("container.xml has no rootfile".into()),
            _ => {}
        }
    }
}

/// ISBN digits of an identifier text (`urn:isbn:978…`, `978-…`, `ISBN 0…`).
fn isbn_digits(text: &str) -> Option<String> {
    let lower = text.trim().to_ascii_lowercase();
    let body = lower
        .strip_prefix("urn:isbn:")
        .or_else(|| lower.strip_prefix("isbn:"))
        .or_else(|| lower.strip_prefix("isbn"))
        .unwrap_or(&lower);
    let digits: String = body.chars().filter(|c| c.is_ascii_digit() || *c == 'x').collect();
    let plausible = body
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '-' | ' ' | 'x'));
    (plausible && matches!(digits.len(), 10 | 13)).then(|| digits.to_ascii_uppercase())
}

/// Reads title, creators and ISBNs from the OPF.
pub fn read_info(path: &Path) -> Result<EpubInfo, String> {
    let mut zip = ZipArchive::new(File::open(path).map_err(err)?).map_err(err)?;
    let opf = opf_path(&mut zip)?;
    let xml = read_entry(&mut zip, &opf)?;
    let mut reader = Reader::from_str(&xml);
    let mut info = EpubInfo::default();
    let mut current: Option<Vec<u8>> = None;
    loop {
        match reader.read_event().map_err(err)? {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_vec();
                current = matches!(name.as_slice(), b"title" | b"creator" | b"identifier").then_some(name);
            }
            Event::Text(t) => {
                let Some(name) = &current else { continue };
                let text = t.unescape().map_err(err)?.trim().to_string();
                if text.is_empty() {
                    continue;
                }
                match name.as_slice() {
                    b"title" if info.title.is_none() => info.title = Some(text),
                    b"creator" => info.creators.push(text),
                    b"identifier" => {
                        if let Some(i) = isbn_digits(&text) {
                            info.isbns.push(i);
                        }
                    }
                    _ => {}
                }
            }
            Event::End(_) => current = None,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(info)
}

fn attr(e: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// Whether an element inside `<metadata>` is replaced by ours.
fn replaced(e: &BytesStart<'_>, cover: bool) -> bool {
    match e.local_name().as_ref() {
        b"title" | b"creator" => true,
        b"meta" => {
            let name = attr(e, b"name").unwrap_or_default();
            let property = attr(e, b"property").unwrap_or_default();
            name == "calibre:series"
                || name == "calibre:series_index"
                || (cover && name == "cover")
                || matches!(
                    property.as_str(),
                    "belongs-to-collection"
                        | "collection-type"
                        | "group-position"
                        | "title-type"
                        | "file-as"
                        | "role"
                        | "display-seq"
                )
        }
        _ => false,
    }
}

fn text_el<W: Write>(w: &mut Writer<W>, name: &str, text: &str) -> Result<(), String> {
    w.write_event(Event::Start(BytesStart::new(name))).map_err(err)?;
    w.write_event(Event::Text(BytesText::new(text))).map_err(err)?;
    w.write_event(Event::End(BytesEnd::new(name))).map_err(err)
}

fn meta_el<W: Write>(w: &mut Writer<W>, name: &str, content: &str) -> Result<(), String> {
    let mut e = BytesStart::new("meta");
    e.push_attribute(("name", name));
    e.push_attribute(("content", content));
    w.write_event(Event::Empty(e)).map_err(err)
}

/// Rewrites the OPF XML with `t`. `cover_href` is the manifest href of the new cover.
pub(crate) fn rewrite_opf(xml: &str, t: &BookTags, cover: Option<(&str, &str)>) -> Result<String, String> {
    let mut reader = Reader::from_str(xml);
    let mut w = Writer::new(Cursor::new(Vec::new()));
    let mut in_metadata = false;
    let mut skip_depth = 0usize;
    let mut has_isbn = false;
    let mut dc = "dc".to_string();
    let isbn = t.isbn.as_deref().and_then(isbn_digits);
    loop {
        let ev = reader.read_event().map_err(err)?;
        if skip_depth > 0 {
            match ev {
                Event::Start(_) => skip_depth += 1,
                Event::End(_) => skip_depth -= 1,
                Event::Eof => break,
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Start(e) if e.local_name().as_ref() == b"metadata" => {
                in_metadata = true;
                let mut declares_dc = false;
                for a in e.attributes().flatten() {
                    let v = a.unescape_value().map_err(err)?;
                    if v == "http://purl.org/dc/elements/1.1/"
                        && let Some(p) = a.key.as_ref().strip_prefix(b"xmlns:")
                    {
                        dc = String::from_utf8_lossy(p).into_owned();
                        declares_dc = true;
                    }
                }
                if !declares_dc && !xml.contains("http://purl.org/dc/elements/1.1/") {
                    let mut e2 = e.to_owned();
                    e2.push_attribute(("xmlns:dc", "http://purl.org/dc/elements/1.1/"));
                    w.write_event(Event::Start(e2)).map_err(err)?;
                } else {
                    if !declares_dc
                        && let Some(p) = xml
                            .split("=\"http://purl.org/dc/elements/1.1/\"")
                            .next()
                            .and_then(|s| s.rsplit("xmlns:").next())
                            .filter(|p| p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                    {
                        dc = p.to_string();
                    }
                    w.write_event(Event::Start(e)).map_err(err)?;
                }
            }
            Event::Start(e) if in_metadata && replaced(&e, cover.is_some()) => skip_depth = 1,
            Event::Empty(e) if in_metadata && replaced(&e, cover.is_some()) => {}
            Event::Start(e) if in_metadata && e.local_name().as_ref() == b"identifier" => {
                w.write_event(Event::Start(e)).map_err(err)?;
                match reader.read_event().map_err(err)? {
                    Event::Text(tx) => {
                        let text = tx.unescape().map_err(err)?.into_owned();
                        if isbn.is_some() && isbn_digits(&text) == isbn {
                            has_isbn = true;
                        }
                        w.write_event(Event::Text(BytesText::new(&text))).map_err(err)?;
                    }
                    Event::Eof => break,
                    other => w.write_event(other).map_err(err)?,
                }
            }
            Event::End(e) if e.local_name().as_ref() == b"metadata" => {
                in_metadata = false;
                text_el(&mut w, &format!("{dc}:title"), &t.title)?;
                for a in &t.authors {
                    text_el(&mut w, &format!("{dc}:creator"), a)?;
                }
                if let (Some(i), false) = (&isbn, has_isbn) {
                    text_el(&mut w, &format!("{dc}:identifier"), &format!("urn:isbn:{i}"))?;
                }
                if let Some(s) = t.series.as_deref().filter(|s| !s.trim().is_empty()) {
                    meta_el(&mut w, "calibre:series", s)?;
                    if let Some(p) = t.series_position.as_deref().filter(|p| !p.trim().is_empty()) {
                        meta_el(&mut w, "calibre:series_index", p)?;
                    }
                }
                if cover.is_some() {
                    meta_el(&mut w, "cover", COVER_ID)?;
                }
                w.write_event(Event::End(e)).map_err(err)?;
            }
            Event::Empty(e)
                if e.local_name().as_ref() == b"item" && attr(&e, b"id").as_deref() == Some(COVER_ID) => {}
            Event::End(e) if e.local_name().as_ref() == b"manifest" => {
                if let Some((href, mime)) = cover {
                    let mut item = BytesStart::new("item");
                    item.push_attribute(("id", COVER_ID));
                    item.push_attribute(("href", href));
                    item.push_attribute(("media-type", mime));
                    if xml.contains("version=\"3") {
                        item.push_attribute(("properties", "cover-image"));
                    }
                    w.write_event(Event::Empty(item)).map_err(err)?;
                }
                w.write_event(Event::End(e)).map_err(err)?;
            }
            Event::Eof => break,
            e => w.write_event(e).map_err(err)?,
        }
    }
    String::from_utf8(w.into_inner().into_inner()).map_err(err)
}

/// Writes `t` into the EPUB at `path` (atomically: temp file + rename).
pub fn write(path: &Path, t: &BookTags) -> Result<(), String> {
    let mut zip = ZipArchive::new(File::open(path).map_err(err)?).map_err(err)?;
    let opf = opf_path(&mut zip)?;
    let xml = read_entry(&mut zip, &opf)?;
    let cover = t.cover.as_ref().and_then(|c| image_mime(c).map(|m| (c, m)));
    let cover_name = cover.map(|(_, m)| {
        if m == "image/png" {
            format!("{COVER_ID}.png")
        } else {
            format!("{COVER_ID}.jpg")
        }
    });
    let opf_dir = opf
        .rsplit_once('/')
        .map(|(d, _)| format!("{d}/"))
        .unwrap_or_default();
    let new_opf = rewrite_opf(
        &xml,
        t,
        cover.zip(cover_name.as_deref()).map(|((_, m), n)| (n, m)),
    )?;

    let tmp = path.with_extension("epub.reelix-tmp");
    let result = (|| -> Result<(), String> {
        let mut out = ZipWriter::new(File::create(&tmp).map_err(err)?);
        if let Ok(m) = zip.by_name("mimetype") {
            out.raw_copy_file(m).map_err(err)?;
        } else {
            out.start_file(
                "mimetype",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .map_err(err)?;
            out.write_all(b"application/epub+zip").map_err(err)?;
        }
        for i in 0..zip.len() {
            let f = zip.by_index_raw(i).map_err(err)?;
            let name = f.name().to_string();
            let ours = name.starts_with(&format!("{opf_dir}{COVER_ID}."));
            if name == "mimetype" || name == opf || ours {
                continue;
            }
            out.raw_copy_file(f).map_err(err)?;
        }
        let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        out.start_file(opf.as_str(), deflate).map_err(err)?;
        out.write_all(new_opf.as_bytes()).map_err(err)?;
        if let (Some((data, _)), Some(n)) = (cover, &cover_name) {
            out.start_file(
                format!("{opf_dir}{n}"),
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .map_err(err)?;
            out.write_all(data).map_err(err)?;
        }
        out.finish().map_err(err)?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        err(e)
    })
}

/// Fuzzing entry point: rewrites an untrusted OPF document with and without a cover. Must
/// never panic.
#[doc(hidden)]
pub fn fuzz_rewrite_opf(xml: &str) {
    let t = BookTags {
        title: "T".into(),
        authors: vec!["A".into()],
        isbn: Some("9780000000002".into()),
        series: Some("S".into()),
        series_position: Some("1".into()),
        ..BookTags::default()
    };
    let _ = rewrite_opf(xml, &t, None);
    let _ = rewrite_opf(xml, &t, Some(("cover.jpg", "image/jpeg")));
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const OPF: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
    <dc:title>Old Title</dc:title>
    <dc:creator opf:role="aut">Someone</dc:creator>
    <dc:identifier id="uid">urn:uuid:1234</dc:identifier>
    <dc:identifier opf:scheme="ISBN">978-0-7653-1178-8</dc:identifier>
    <dc:language>en</dc:language>
    <meta name="calibre:series" content="Old"/>
  </metadata>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="ch1"/></spine>
</package>"#;

    /// A minimal EPUB 2 file with the given OPF.
    pub(crate) fn epub_bytes(opf: &str) -> Vec<u8> {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        w.start_file("mimetype", stored).unwrap();
        w.write_all(b"application/epub+zip").unwrap();
        w.start_file("META-INF/container.xml", SimpleFileOptions::default())
            .unwrap();
        w.write_all(
            br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
        )
        .unwrap();
        w.start_file("OEBPS/content.opf", SimpleFileOptions::default())
            .unwrap();
        w.write_all(opf.as_bytes()).unwrap();
        w.start_file("OEBPS/ch1.xhtml", SimpleFileOptions::default())
            .unwrap();
        w.write_all(b"<html><body>Hi</body></html>").unwrap();
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn isbn_variants() {
        assert_eq!(
            isbn_digits("urn:isbn:9780765311788").as_deref(),
            Some("9780765311788")
        );
        assert_eq!(isbn_digits("978-0-7653-1178-8").as_deref(), Some("9780765311788"));
        assert_eq!(isbn_digits("ISBN 076531178x").as_deref(), Some("076531178X"));
        assert_eq!(isbn_digits("urn:uuid:1234"), None);
    }

    #[test]
    fn reads_and_writes_opf() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("book.epub");
        std::fs::write(&p, epub_bytes(OPF)).unwrap();
        let info = read_info(&p).unwrap();
        assert_eq!(info.title.as_deref(), Some("Old Title"));
        assert_eq!(info.isbns, ["9780765311788"]);

        let tags = BookTags {
            title: "The Final Empire".into(),
            authors: vec!["Brandon Sanderson".into()],
            series: Some("Mistborn".into()),
            series_position: Some("1".into()),
            isbn: Some("9780765311788".into()),
            cover: Some(vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4]),
            ..Default::default()
        };
        write(&p, &tags).unwrap();
        // Writing twice keeps a single cover and title.
        write(&p, &tags).unwrap();
        let info = read_info(&p).unwrap();
        assert_eq!(info.title.as_deref(), Some("The Final Empire"));
        assert_eq!(info.creators, ["Brandon Sanderson"]);
        assert_eq!(info.isbns, ["9780765311788"]);

        let mut zip = ZipArchive::new(File::open(&p).unwrap()).unwrap();
        assert_eq!(zip.by_index(0).unwrap().name(), "mimetype");
        assert_eq!(zip.by_index(0).unwrap().compression(), CompressionMethod::Stored);
        let mut opf = String::new();
        zip.by_name("OEBPS/content.opf")
            .unwrap()
            .read_to_string(&mut opf)
            .unwrap();
        assert!(
            opf.contains(r#"<meta name="calibre:series" content="Mistborn"/>"#),
            "{opf}"
        );
        assert!(opf.contains(r#"<meta name="calibre:series_index" content="1"/>"#));
        assert!(!opf.contains("Old"));
        assert_eq!(opf.matches("reelix-cover.jpg").count(), 1, "{opf}");
        assert!(opf.contains("urn:uuid:1234"));
        assert!(zip.by_name("OEBPS/reelix-cover.jpg").is_ok());
        assert!(zip.by_name("OEBPS/ch1.xhtml").is_ok());
        assert!(!dir.path().join("book.epub.reelix-tmp").exists());
    }

    #[test]
    fn adds_missing_isbn_and_namespace() {
        let opf =
            r#"<package version="3.0"><metadata><title>X</title></metadata><manifest></manifest></package>"#;
        let t = BookTags {
            title: "Dune".into(),
            isbn: Some("9780441013593".into()),
            ..Default::default()
        };
        let out = rewrite_opf(opf, &t, Some(("reelix-cover.png", "image/png"))).unwrap();
        assert!(
            out.contains(r#"xmlns:dc="http://purl.org/dc/elements/1.1/""#),
            "{out}"
        );
        assert!(out.contains("<dc:title>Dune</dc:title>"));
        assert!(out.contains("urn:isbn:9780441013593"));
        assert!(out.contains(r#"properties="cover-image""#));
        assert!(!out.contains("<title>X</title>"));
    }
}
