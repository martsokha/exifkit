//! Reading any file must never panic, and the streaming reader must agree
//! with the in-memory one.

#![no_main]

use std::io::Cursor;

use exifkit::Exif;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let in_memory = Exif::read(data);
    let streamed = Exif::read_from(Cursor::new(data));
    match (&in_memory, &streamed) {
        (Ok(a), Ok(b)) => assert_eq!(a, b),
        (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string()),
        _ => panic!("read and read_from disagree: {in_memory:?} vs {streamed:?}"),
    }
});
