#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = reelix_parser::parse(s);
        let _ = reelix_parser::parse_path(s);
        let _ = reelix_parser::parse_book(s, None);
        let _ = reelix_parser::parse_music(s);
        let _ = reelix_parser::clean_title(s);
    }
});
