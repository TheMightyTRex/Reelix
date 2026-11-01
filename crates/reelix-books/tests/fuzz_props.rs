//! Property test: rewriting the OPF of an untrusted EPUB must never panic.

use proptest::prelude::*;
use reelix_books::tags::epub::fuzz_rewrite_opf;

const OPF: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
    <dc:title>Old Title</dc:title>
    <dc:creator opf:role="aut">Someone</dc:creator>
    <dc:identifier id="uid">urn:uuid:1234</dc:identifier>
    <dc:identifier opf:scheme="ISBN">978-0-7653-1178-8</dc:identifier>
    <meta name="calibre:series" content="Old"/>
    <meta name="cover" content="img"/>
  </metadata>
  <manifest>
    <item id="img" href="cover.jpg" media-type="image/jpeg" properties="cover-image"/>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="ch1"/></spine>
</package>"#;

fn boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 1024, ..ProptestConfig::default() })]

    #[test]
    fn mutated_opf_never_panics(
        edits in prop::collection::vec((0.0f64..1.0, 0usize..40, "(<|>|/|\"|=|&|metadata|manifest|\\PC){0,8}"), 1..6)
    ) {
        let mut s = OPF.to_owned();
        for (p, del, ins) in edits {
            let at = boundary(&s, (s.len() as f64 * p) as usize);
            let end = boundary(&s, at + del).max(at);
            s.replace_range(at..end, &ins);
        }
        fuzz_rewrite_opf(&s);
    }

    #[test]
    fn random_opf_never_panics(s in "\\PC{0,300}") {
        fuzz_rewrite_opf(&s);
    }
}
