//! Property test: decoding rTorrent XML-RPC responses must never panic.

use proptest::prelude::*;
use reelix_downloaders::xmlrpc::decode_response;

const SEEDS: &[&str] = &[
    include_str!("fixtures/rtorrent/directory.xml"),
    include_str!("fixtures/rtorrent/fault.xml"),
    include_str!("fixtures/rtorrent/multicall.xml"),
    include_str!("fixtures/rtorrent/version.xml"),
    include_str!("fixtures/rtorrent/zero.xml"),
];

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
    fn mutated_responses_never_panic(
        seed in 0..SEEDS.len(),
        edits in prop::collection::vec(
            (0.0f64..1.0, 0usize..40, "(<|>|/|value|array|struct|i8|int|data|member|9999999999999999999999|-|\\PC){0,8}"),
            1..6,
        )
    ) {
        let mut s = SEEDS[seed].to_owned();
        for (p, del, ins) in edits {
            let at = boundary(&s, (s.len() as f64 * p) as usize);
            let end = boundary(&s, at + del).max(at);
            s.replace_range(at..end, &ins);
        }
        let _ = decode_response(&s);
    }

    #[test]
    fn random_responses_never_panic(s in "\\PC{0,300}") {
        let _ = decode_response(&s);
    }
}
