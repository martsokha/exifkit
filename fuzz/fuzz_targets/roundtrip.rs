//! Writing any readable TIFF block must succeed or fail cleanly, produce a
//! block that reads back with every writable entry intact, and be stable:
//! writing what was read back gives the same bytes again.

#![no_main]

use exifkit::tag::Role;
use exifkit::{Exif, MakerNotePolicy, Value, WriteOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok((exif, _)) = Exif::from_tiff(data) else {
        return;
    };
    let options = [
        WriteOptions::DEFAULT,
        WriteOptions::DEFAULT.source(data),
        WriteOptions::DEFAULT.maker_note(MakerNotePolicy::Move),
    ];
    for options in options {
        let Ok(bytes) = exif.to_tiff_with(&options) else {
            continue;
        };
        assert_eq!(exif.encoded_len(&options).ok(), Some(bytes.len()));
        let (back, _) = Exif::from_tiff(&bytes).expect("writer output must be a valid block");

        // Every writable entry survives with its value, in its IFD. With a
        // source, data references that dangled in it are dropped on purpose.
        for ifd in exif.ifds() {
            for entry in ifd.entries() {
                let data_ref = entry
                    .tag()
                    .info()
                    .is_some_and(|i| matches!(i.role, Role::DataOffset | Role::DataLength));
                if matches!(entry.value(), Value::Unknown { .. }) || (data_ref && options.source.is_some()) {
                    continue;
                }
                let found = back.ifd(ifd.kind()).and_then(|i| i.get(entry.tag()));
                assert_eq!(found, Some(entry), "{:?} {}", ifd.kind(), entry.tag());
            }
        }
        if exif.thumbnail().is_some() {
            assert_eq!(back.thumbnail(), exif.thumbnail());
        }

        // Stable: writing it again changes nothing.
        if options.source.is_none() {
            assert_eq!(back.to_tiff_with(&options).ok(), Some(bytes));
        }
    }
});
