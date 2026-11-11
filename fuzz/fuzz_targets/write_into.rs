//! Writing Exif back into any readable file must succeed or fail cleanly,
//! give the same bytes in memory and streamed,
//! keep every writable entry, and leave every byte outside the Exif data
//! unchanged: stripping Exif from the rewrite equals stripping it from the
//! original.

#![no_main]

use exifkit::tag::Role;
use exifkit::{Exif, Value, remove_exif};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok((exif, _)) = Exif::read(data) else {
        return;
    };
    let Ok(out) = exif.write_into(data) else {
        return;
    };
    let mut streamed = Vec::new();
    exif.write_stream(std::io::Cursor::new(data), &mut streamed, &exifkit::WriteOptions::DEFAULT)
        .expect("write_stream must succeed where write_into does");
    assert!(streamed == out, "write_stream and write_into disagree");
    let (back, _) = Exif::read(&out).expect("a written file must read back");
    for ifd in exif.ifds() {
        for entry in ifd.entries() {
            let data_ref = entry
                .tag()
                .info()
                .is_some_and(|i| matches!(i.role, Role::DataOffset | Role::DataLength));
            if matches!(entry.value(), Value::Unknown { .. }) || data_ref {
                continue;
            }
            let found = back.ifd(ifd.kind()).and_then(|i| i.get(entry.tag()));
            assert_eq!(found, Some(entry), "{:?} {}", ifd.kind(), entry.tag());
        }
    }
    if let (Ok(before), Ok(after)) = (remove_exif(data), remove_exif(&out)) {
        assert!(before == after, "bytes outside the Exif data changed");
    }
});
