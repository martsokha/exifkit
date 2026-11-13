//! Writer tests.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::error::Error;
use crate::ifd::IfdKind;
use crate::model::Entry;
use crate::model::tests_support::exif_with;
use crate::tag::{Namespace, Tag};
use crate::tiff::{Header, RawIfd, Reader};
use crate::value::{Rational, Value};

fn read(data: &[u8]) -> Exif {
    let (exif, diagnostics) = Exif::from_tiff(data).expect("writer output is a valid TIFF block");
    assert!(
        diagnostics.is_empty(),
        "writer output has diagnostics: {diagnostics:?}"
    );
    exif
}

fn camera() -> Exif {
    let mut exif = exif_with(&[
        (Tag::MAKE, Value::ascii("Canon")),
        (Tag::ORIENTATION, Value::Short(vec![6])),
        (
            Tag::X_RESOLUTION,
            Value::Rational(vec![Rational::new(72, 1)]),
        ),
        (
            Tag::EXPOSURE_TIME,
            Value::Rational(vec![Rational::new(1, 250)]),
        ),
        (Tag::EXIF_VERSION, Value::Undefined(b"0300".to_vec())),
        (Tag::GPS_LATITUDE_REF, Value::ascii("N")),
        (Tag::INTEROPERABILITY_INDEX, Value::ascii("R98")),
    ]);
    exif.set_thumbnail(Some(vec![0xFF, 0xD8, 1, 2, 3, 0xFF, 0xD9]));
    exif
}

#[test]
fn empty_model_is_a_valid_block() {
    let bytes = Exif::new().to_tiff().unwrap();
    assert_eq!(bytes.len(), Header::LEN + 6);
    let exif = read(&bytes);
    assert_eq!(exif.ifds().len(), 1);
    assert_eq!(exif.ifds()[0].kind(), IfdKind::Primary);
}

#[test]
fn round_trips_in_both_byte_orders() {
    for order in [ByteOrder::LittleEndian, ByteOrder::BigEndian] {
        let mut exif = camera();
        exif.set_byte_order(order);
        let bytes = exif.to_tiff().unwrap();
        assert_eq!(
            exif.encoded_len(&WriteOptions::DEFAULT).unwrap(),
            bytes.len()
        );
        assert_eq!(read(&bytes), exif);
        // Writing what was read gives the same bytes again.
        assert_eq!(read(&bytes).to_tiff().unwrap(), bytes);
    }
}

#[test]
fn byte_order_can_be_changed_on_write() {
    let exif = camera();
    let options = WriteOptions::DEFAULT.byte_order(ByteOrder::BigEndian);
    let back = read(&exif.to_tiff_with(&options).unwrap());
    assert_eq!(back.byte_order(), ByteOrder::BigEndian);
    assert_eq!(back.get::<u16>(Tag::ORIENTATION), Ok(Some(6)));
}

#[test]
fn missing_parents_are_created() {
    let exif = exif_with(&[(Tag::INTEROPERABILITY_INDEX, Value::ascii("R98"))]);
    let back = read(&exif.to_tiff().unwrap());
    let kinds: Vec<IfdKind> = back.ifds().iter().map(|i| i.kind()).collect();
    assert_eq!(kinds, [IfdKind::Primary, IfdKind::Exif, IfdKind::Interop]);
}

#[test]
fn tables_are_sorted_aligned_and_padded() {
    let bytes = camera().to_tiff().unwrap();
    let reader = Reader::new(&bytes, ByteOrder::LittleEndian);
    let ifd0 = RawIfd::parse(reader, Header::parse(&bytes).unwrap().ifd0_offset).unwrap();
    let ids: Vec<u16> = ifd0.entries().map(|e| e.tag).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted);
    assert_eq!(ifd0.offset() % 2, 0);
    for entry in ifd0.entries() {
        let len = entry.byte_len().unwrap();
        if len > 4 {
            assert_eq!(
                entry.data_offset().unwrap() % 2,
                0,
                "tag {:#06x}",
                entry.tag
            );
        } else {
            let padding = &entry.field[usize::try_from(len).unwrap()..];
            assert!(padding.iter().all(|&b| b == 0), "tag {:#06x}", entry.tag);
        }
    }
}

fn with_maker_note() -> (Exif, u32) {
    let mut exif = camera();
    exif.insert(Entry::new(
        Tag::MAKER_NOTE,
        Value::Undefined((0..40).collect()),
    ))
    .unwrap();
    let options = WriteOptions::DEFAULT.maker_note(MakerNotePolicy::Move);
    let back = read(&exif.to_tiff_with(&options).unwrap());
    let original = back.maker_note_offset().expect("MakerNote is out of line");
    (back, original)
}

#[test]
fn maker_note_keeps_its_offset_when_tags_are_added() {
    let (mut exif, original) = with_maker_note();
    // Lots of new data before the MakerNote in write order.
    exif.insert(Entry::new(
        Tag::IMAGE_DESCRIPTION,
        Value::ascii(&"x".repeat(300)),
    ))
    .unwrap();
    let back = read(&exif.to_tiff().unwrap());
    assert_eq!(back.maker_note_offset(), Some(original));
    assert_eq!(back.offset_schema(), None);
    assert_eq!(
        back.get::<String>(Tag::IMAGE_DESCRIPTION)
            .unwrap()
            .unwrap()
            .len(),
        300
    );
    assert_eq!(back, exif);
}

#[test]
fn moved_maker_note_records_offset_schema() {
    let (mut exif, original) = with_maker_note();
    exif.insert(Entry::new(
        Tag::IMAGE_DESCRIPTION,
        Value::ascii(&"x".repeat(300)),
    ))
    .unwrap();
    let options = WriteOptions::DEFAULT.maker_note(MakerNotePolicy::Move);
    let back = read(&exif.to_tiff_with(&options).unwrap());
    let moved = back.maker_note_offset().unwrap();
    assert_ne!(moved, original);
    let distance = i64::from(moved) - i64::from(original);
    assert_eq!(back.offset_schema().map(i64::from), Some(distance));

    // Moving it again accumulates the schema.
    let again = read(
        &back
            .to_tiff_with(&WriteOptions::DEFAULT.maker_note(MakerNotePolicy::Move))
            .unwrap(),
    );
    let total = i64::from(again.maker_note_offset().unwrap()) - i64::from(original);
    assert_eq!(again.offset_schema().map(i64::from), Some(total));
}

#[test]
fn anchoring_falls_back_or_fails() {
    let (mut exif, _) = with_maker_note();
    exif.maker_note_offset = Some(4); // inside the header: cannot anchor there
    let back = read(&exif.to_tiff().unwrap());
    assert!(
        back.offset_schema().is_some(),
        "Anchor falls back to moving"
    );
    let required = WriteOptions::DEFAULT.maker_note(MakerNotePolicy::RequireAnchored);
    assert!(matches!(
        exif.to_tiff_with(&required),
        Err(Error::Unsupported { .. })
    ));
    let dropped = WriteOptions::DEFAULT.maker_note(MakerNotePolicy::Drop);
    let back = read(&exif.to_tiff_with(&dropped).unwrap());
    assert!(back.entry(Tag::MAKER_NOTE).is_none());
    assert_eq!(back.offset_schema(), None);
}

/// A TIFF with one 16-byte strip at offset 100, as a TIFF file has.
fn tiff_with_strip() -> Vec<u8> {
    let mut data = vec![0u8; 116];
    data[..8].copy_from_slice(b"II*\0\x08\0\0\0");
    let entries: [(u16, u16, u32, u32); 3] =
        [(0x0100, 3, 1, 4), (0x0111, 4, 1, 100), (0x0117, 4, 1, 16)];
    let mut ifd = vec![];
    ifd.extend_from_slice(&3u16.to_le_bytes());
    for (tag, ty, count, value) in entries {
        ifd.extend_from_slice(&tag.to_le_bytes());
        ifd.extend_from_slice(&ty.to_le_bytes());
        ifd.extend_from_slice(&count.to_le_bytes());
        ifd.extend_from_slice(&value.to_le_bytes());
    }
    ifd.extend_from_slice(&0u32.to_le_bytes());
    data[8..8 + ifd.len()].copy_from_slice(&ifd);
    for (i, byte) in data[100..116].iter_mut().enumerate() {
        *byte = 0xA0 + u8::try_from(i).unwrap();
    }
    data
}

#[test]
fn image_data_needs_the_source_and_stays_in_place() {
    let source = tiff_with_strip();
    let mut exif = read(&source);
    assert!(matches!(exif.to_tiff(), Err(Error::Unsupported { .. })));

    exif.insert(Entry::new(Tag::ARTIST, Value::ascii(&"y".repeat(200))))
        .unwrap();
    let out = exif
        .to_tiff_with(&WriteOptions::DEFAULT.source(&source))
        .unwrap();
    assert_eq!(&out[100..116], &source[100..116], "strip copied in place");
    let back = read(&out);
    assert_eq!(back, exif);
    assert_eq!(back.get::<u32>(Tag::STRIP_OFFSETS), Ok(Some(100)));
}

#[test]
fn unwritable_entries_are_dropped() {
    let mut exif = camera();
    exif.ifd_mut(IfdKind::Primary).entries.push(Entry::new(
        Tag::new(Namespace::Main, 0xC000),
        Value::Unknown {
            type_id: 99,
            count: 1,
            field: [0; 4],
        },
    ));
    exif.ifd_mut(IfdKind::Primary).normalize();
    let back = read(&exif.to_tiff().unwrap());
    assert!(back.entry(Tag::new(Namespace::Main, 0xC000)).is_none());
    assert_eq!(back.get::<u16>(Tag::ORIENTATION), Ok(Some(6)));
}

#[test]
fn thumbnail_round_trips_and_can_be_removed() {
    let mut exif = camera();
    let back = read(&exif.to_tiff().unwrap());
    assert_eq!(back.thumbnail(), exif.thumbnail());
    exif.set_thumbnail(None);
    exif.remove_ifd(IfdKind::Thumbnail);
    let back = read(&exif.to_tiff().unwrap());
    assert!(back.thumbnail().is_none());
    assert!(back.ifd(IfdKind::Thumbnail).is_none());
}

#[test]
fn editing_rejects_managed_and_misplaced_tags() {
    let mut exif = camera();
    let pointer = Entry::new(Tag::EXIF_IFD_POINTER, Value::Long(vec![8]));
    assert!(matches!(
        exif.insert(pointer),
        Err(Error::InvalidEntry { .. })
    ));
    let gps_in_ifd0 = Entry::new(Tag::GPS_LATITUDE_REF, Value::ascii("S"));
    assert!(matches!(
        exif.ifd_mut(IfdKind::Primary).insert(gps_in_ifd0),
        Err(Error::InvalidEntry { .. })
    ));
    let replaced = exif
        .insert(Entry::new(Tag::MAKE, Value::ascii("Nikon")))
        .unwrap();
    assert_eq!(
        replaced.map(|e| e.into_value()),
        Some(Value::ascii("Canon"))
    );
    assert!(exif.remove(Tag::MAKE).is_some());
    assert!(exif.entry(Tag::MAKE).is_none());
}

#[test]
fn offset_schema_is_kept_when_anchored() {
    let (mut exif, _) = with_maker_note();
    exif.offset_schema = Some(-12);
    let back = read(&exif.to_tiff().unwrap());
    assert_eq!(back.offset_schema(), Some(-12));
}

/// A block written once, so its values have known source offsets.
fn written_camera() -> (Vec<u8>, Exif) {
    let mut exif = camera();
    exif.insert(Entry::new(
        Tag::IMAGE_DESCRIPTION,
        Value::ascii("SECRET-DESCRIPTION-0123456789"),
    ))
    .unwrap();
    exif.insert(Entry::new(
        Tag::GPS_LATITUDE,
        Value::Rational(vec![
            Rational::new(51, 1),
            Rational::new(30, 1),
            Rational::new(1234, 100),
        ]),
    ))
    .unwrap();
    let bytes = exif.to_tiff().unwrap();
    let back = read(&bytes);
    (bytes, back)
}

#[test]
fn unchanged_values_keep_their_offsets() {
    let (source, mut exif) = written_camera();
    let before: Vec<(Tag, Option<u32>)> =
        exif.entries().map(|(_, e)| (e.tag(), e.source)).collect();
    // Change one out-of-line value and add another, forcing new allocations.
    exif.insert(Entry::new(
        Tag::MAKE,
        Value::ascii("A much longer make than before"),
    ))
    .unwrap();
    exif.insert(Entry::new(Tag::ARTIST, Value::ascii(&"z".repeat(64))))
        .unwrap();
    let out = exif
        .to_tiff_with(&WriteOptions::DEFAULT.source(&source))
        .unwrap();
    let after = read(&out);
    for (tag, offset) in before {
        if tag == Tag::MAKE || offset.is_none() {
            continue;
        }
        let moved = after.entry(tag).and_then(|e| e.source);
        assert_eq!(moved, offset, "{tag} moved");
    }
    assert_eq!(after, exif);
}

#[test]
fn removed_values_do_not_leak_into_the_output() {
    let (source, mut exif) = written_camera();
    assert!(source.windows(6).any(|w| w == b"SECRET"));
    exif.remove(Tag::IMAGE_DESCRIPTION);
    exif.remove_ifd(IfdKind::Gps);
    let out = exif
        .to_tiff_with(&WriteOptions::DEFAULT.source(&source))
        .unwrap();
    assert!(
        !out.windows(6).any(|w| w == b"SECRET"),
        "removed text survived"
    );
    let latitude = 1234u32.to_le_bytes();
    assert!(
        !out.windows(4).any(|w| w == latitude),
        "removed GPS survived"
    );
    assert_eq!(read(&out), exif);
}

#[test]
fn dangling_data_references_are_dropped_with_the_source() {
    // A strip offset past the end of the source (found by fuzzing: written
    // back unchanged it pointed at unrelated bytes of a larger output).
    let mut source = tiff_with_strip();
    source.truncate(100);
    let (exif, _) = Exif::from_tiff(&source).unwrap();
    assert!(exif.entry(Tag::STRIP_OFFSETS).is_some());
    let out = exif
        .to_tiff_with(&WriteOptions::DEFAULT.source(&source))
        .unwrap();
    let back = read(&out);
    assert!(back.entry(Tag::STRIP_OFFSETS).is_none());
    assert!(back.entry(Tag::STRIP_BYTE_COUNTS).is_none());
    assert_eq!(back.get::<u16>(Tag::IMAGE_WIDTH), Ok(Some(4)));
}

#[test]
#[cfg(all(feature = "png", feature = "std"))]
fn an_oversized_source_block_fails_on_both_paths() {
    // A PNG whose eXIf chunk holds a TIFF block bigger than `max_metadata`.
    let crc = |kind: &[u8], data: &[u8]| {
        // CRC-32 (ISO 3309), bitwise.
        let mut c = 0xFFFF_FFFFu32;
        for &b in kind.iter().chain(data) {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    };
    let chunk = |kind: &[u8; 4], data: &[u8]| {
        let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc(kind, data).to_be_bytes());
        out
    };
    let mut tiff = b"II*\0\x08\0\0\0\0\0\0\0\0\0".to_vec();
    tiff.resize(4096, 0);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 0, 0, 0, 0]));
    png.extend(chunk(b"eXIf", &tiff));
    png.extend(chunk(b"IDAT", b"x"));
    png.extend(chunk(b"IEND", b""));
    let (exif, _) = Exif::read(&png).unwrap();
    let tight = WriteOptions::DEFAULT.limits(crate::Limits {
        max_metadata: 1024,
        ..crate::Limits::DEFAULT
    });
    let limit = |r: crate::Result<()>| {
        matches!(r, Err(crate::Error::LimitExceeded(crate::Limit::Metadata)))
    };
    assert!(limit(exif.write_into_with(&png, &tight).map(drop)));
    assert!(limit(exif.write_stream(
        std::io::Cursor::new(&png),
        &mut Vec::new(),
        &tight
    )));
    // Within the limit, both paths agree.
    let mut streamed = Vec::new();
    exif.write_stream(
        std::io::Cursor::new(&png),
        &mut streamed,
        &WriteOptions::DEFAULT,
    )
    .unwrap();
    assert_eq!(streamed, exif.write_into(&png).unwrap());
}

/// A `Read + Seek` that counts the bytes read through it.
#[cfg(feature = "std")]
struct Counting<'a> {
    inner: std::io::Cursor<&'a [u8]>,
    read: std::rc::Rc<core::cell::Cell<u64>>,
}

#[cfg(feature = "std")]
impl std::io::Read for Counting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = std::io::Read::read(&mut self.inner, buf)?;
        self.read.set(self.read.get() + n as u64);
        Ok(n)
    }
}

#[cfg(feature = "std")]
impl std::io::Seek for Counting<'_> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(&mut self.inner, pos)
    }
}

#[test]
#[cfg(all(feature = "tiff", feature = "std"))]
fn tiff_streams_read_metadata_not_image_data() {
    // IFD0 { ImageWidth, StripOffsets, StripByteCounts, Make } with an 8 MB
    // strip after it.
    const STRIP: u32 = 8 << 20;
    let mut tiff = b"II*\0\x08\0\0\0".to_vec();
    let strip_at: u32 = 8 + 2 + 4 * 12 + 4 + 8;
    tiff.extend_from_slice(&4u16.to_le_bytes());
    for (tag, ty, count, value) in [
        (0x0100u16, 4u16, 1u32, 64u32),
        (0x010F, 2, 6, strip_at - 8),
        (0x0111, 4, 1, strip_at),
        (0x0117, 4, 1, STRIP),
    ] {
        tiff.extend_from_slice(&tag.to_le_bytes());
        tiff.extend_from_slice(&ty.to_le_bytes());
        tiff.extend_from_slice(&count.to_le_bytes());
        tiff.extend_from_slice(&value.to_le_bytes());
    }
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff.extend_from_slice(b"Canon\0\0\0");
    tiff.extend((0..STRIP).map(|i| i as u8));
    let counter = std::rc::Rc::new(core::cell::Cell::new(0));
    let stream = || Counting {
        inner: std::io::Cursor::new(&tiff),
        read: counter.clone(),
    };

    let (exif, _) = Exif::read_from(stream()).unwrap();
    assert!(
        counter.get() < 64 * 1024,
        "read_from read {} bytes",
        counter.get()
    );
    assert_eq!(exif, Exif::read(&tiff).unwrap().0);

    let mut edited = exif.clone();
    edited.set(Tag::ARTIST, "exifkit").unwrap();
    counter.set(0);
    let mut streamed = Vec::new();
    edited
        .write_stream(stream(), &mut streamed, &WriteOptions::DEFAULT)
        .unwrap();
    // The strip is read once, to copy it through; the plan reads only
    // metadata.
    assert!(
        counter.get() < u64::from(STRIP) + 128 * 1024,
        "write_stream read {} bytes",
        counter.get()
    );
    assert_eq!(streamed, edited.write_into(&tiff).unwrap());
}

#[test]
fn raw_magic_needs_a_byte_order_that_can_carry_it() {
    // Found in review: RW2 and ORF `IIRS` exist only little-endian, so
    // writing them big-endian would give a header no reader recognizes.
    let mut tiff = b"IIU\0\x08\0\0\0\x01\0".to_vec();
    tiff.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
    tiff.extend_from_slice(&[0; 4]);
    let (exif, _) = Exif::read(&tiff).unwrap();
    let out = exif.write_into(&tiff).unwrap();
    assert_eq!(&out[..4], b"IIU\0");
    let big_endian = WriteOptions::DEFAULT.byte_order(crate::tiff::ByteOrder::BigEndian);
    assert!(matches!(
        exif.write_into_with(&tiff, &big_endian),
        Err(Error::Unsupported { .. })
    ));
    // ORF has a big-endian form, so it converts.
    let mut orf = tiff.clone();
    orf[2..4].copy_from_slice(b"RO");
    let (exif, _) = Exif::read(&orf).unwrap();
    let out = exif.write_into_with(&orf, &big_endian).unwrap();
    assert_eq!(&out[..4], b"MMOR");
    assert_eq!(Exif::read(&out).unwrap().0.entries().count(), 1);
}

#[test]
fn values_sharing_source_bytes_are_written_once() {
    // Found in review: Make and Model pointing at the same bytes, and
    // Software at a suffix of them, all kept in place by the writer.
    let mut tiff = b"II*\0\x08\0\0\0\x03\0".to_vec();
    let text_at: u32 = 8 + 2 + 3 * 12 + 4;
    for (tag, count, at) in [
        (0x010Fu16, 8u32, text_at),
        (0x0110, 8, text_at),
        (0x0131, 5, text_at + 3),
    ] {
        tiff.extend_from_slice(&tag.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&count.to_le_bytes());
        tiff.extend_from_slice(&at.to_le_bytes());
    }
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff.extend_from_slice(b"ACME Co\0");
    let (exif, _) = Exif::read(&tiff).unwrap();
    let out = exif
        .to_tiff_with(&WriteOptions::DEFAULT.source(&tiff))
        .unwrap();
    assert_eq!(Exif::from_tiff(&out).unwrap().0, exif);
    // Unchanged, they all stay where they were: the block is the same.
    assert_eq!(out, tiff);
    // An edit to one of them still moves only that one.
    let mut edited = exif.clone();
    edited.set(Tag::MODEL, "Widget 3000").unwrap();
    let out = edited
        .to_tiff_with(&WriteOptions::DEFAULT.source(&tiff))
        .unwrap();
    assert_eq!(Exif::from_tiff(&out).unwrap().0, edited);
}
