//! The TIFF header parser must never panic, and must round-trip what it accepts.

#![no_main]

use exifkit::tiff::Header;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(header) = Header::parse(data) {
        assert_eq!(Header::parse(&header.to_bytes()).ok(), Some(header));
    }
});
