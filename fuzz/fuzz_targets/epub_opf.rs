#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        reelix_books::tags::epub::fuzz_rewrite_opf(s);
    }
});
