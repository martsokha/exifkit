//! Reading any TIFF block must never panic, display must never panic, and
//! strict mode must fail exactly when lenient mode reports an error.

#![no_main]

use std::fmt::Write;

use exifkit::{Exif, ReadOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok((exif, diagnostics)) = Exif::from_tiff(data) else {
        return;
    };
    let mut sink = String::new();
    for (_, entry) in exif.entries() {
        let _ = write!(sink, "{entry}");
    }
    for diagnostic in &diagnostics {
        let _ = write!(sink, "{diagnostic}");
    }
    let strict = Exif::from_tiff_with(data, &ReadOptions::DEFAULT.strict(true));
    assert_eq!(strict.is_ok(), diagnostics.first_error().is_none());
});
