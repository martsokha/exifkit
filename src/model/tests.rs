use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::diagnostic::Severity;
use crate::tag::Orientation;
use crate::tiff::Type;
use crate::value::Rational;

/// Builds little-endian TIFF blocks. Children are written before parents,
/// so offsets are known when the parent is written; only IFD0 is patched in.
struct Tiff(Vec<u8>);

/// `(tag, type, count, field)`; `field` is the value-or-offset as a LE u32.
type E = (u16, u16, u32, u32);

const SHORT: u16 = 3;
const LONG: u16 = 4;
const ASCII: u16 = 2;
const RATIONAL: u16 = 5;
const UNDEFINED: u16 = 7;
const UTF8: u16 = 129;

impl Tiff {
    fn new() -> Self {
        Self(b"II\x2a\0\0\0\0\0".to_vec())
    }

    fn pos(&self) -> u32 {
        u32::try_from(self.0.len()).unwrap()
    }

    fn blob(&mut self, bytes: &[u8]) -> u32 {
        let at = self.pos();
        self.0.extend_from_slice(bytes);
        if self.0.len() % 2 == 1 {
            self.0.push(0);
        }
        at
    }

    fn ifd(&mut self, entries: &[E], next: u32) -> u32 {
        let at = self.pos();
        self.0
            .extend_from_slice(&u16::try_from(entries.len()).unwrap().to_le_bytes());
        for &(tag, ty, count, field) in entries {
            self.0.extend_from_slice(&tag.to_le_bytes());
            self.0.extend_from_slice(&ty.to_le_bytes());
            self.0.extend_from_slice(&count.to_le_bytes());
            self.0.extend_from_slice(&field.to_le_bytes());
        }
        self.0.extend_from_slice(&next.to_le_bytes());
        at
    }

    fn root(mut self, ifd0: u32) -> Vec<u8> {
        self.0[4..8].copy_from_slice(&ifd0.to_le_bytes());
        self.0
    }
}

fn inline(bytes: [u8; 4]) -> u32 {
    u32::from_le_bytes(bytes)
}

fn read(data: &[u8]) -> (Exif, Diagnostics) {
    Exif::from_tiff(data).expect("header is valid")
}

fn kinds(diagnostics: &Diagnostics) -> Vec<DiagnosticKind> {
    diagnostics.iter().map(|d| d.kind).collect()
}

/// IFD0 (Make, Orientation, Exif and GPS pointers) → Exif IFD and GPS IFD.
fn camera() -> Vec<u8> {
    let mut t = Tiff::new();
    let make = t.blob(b"Canon\0");
    let exposure = t.blob(&[1, 0, 0, 0, 250, 0, 0, 0]);
    let exif = t.ifd(
        &[
            (0x829A, RATIONAL, 1, exposure),
            (0x9000, UNDEFINED, 4, inline(*b"0300")),
        ],
        0,
    );
    let gps = t.ifd(&[(0x0001, ASCII, 2, inline(*b"N\0\0\0"))], 0);
    let ifd0 = t.ifd(
        &[
            (0x010F, ASCII, 6, make),
            (0x0112, SHORT, 1, 6),
            (0x8769, LONG, 1, exif),
            (0x8825, LONG, 1, gps),
        ],
        0,
    );
    t.root(ifd0)
}

#[test]
fn reads_a_well_formed_block() {
    let (exif, diagnostics) = read(&camera());
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(exif.byte_order(), ByteOrder::LittleEndian);
    let kinds: Vec<_> = exif.ifds().iter().map(Ifd::kind).collect();
    assert_eq!(kinds, [IfdKind::Primary, IfdKind::Exif, IfdKind::Gps]);

    assert_eq!(exif.get::<String>(Tag::MAKE), Ok(Some("Canon".into())));
    assert_eq!(
        exif.get::<Orientation>(Tag::ORIENTATION),
        Ok(Some(Orientation::Rotate90))
    );
    assert_eq!(
        exif.get::<Rational>(Tag::EXPOSURE_TIME),
        Ok(Some(Rational::new(1, 250)))
    );
    assert_eq!(
        exif.get::<String>(Tag::GPS_LATITUDE_REF),
        Ok(Some("N".into()))
    );
    assert_eq!(exif.get::<u16>(Tag::FLASH), Ok(None));

    // Pointer tags are structure, not entries.
    assert!(exif.entry(Tag::EXIF_IFD_POINTER).is_none());
    assert_eq!(exif.entries().count(), 5);

    let orientation = exif.entry(Tag::ORIENTATION).unwrap();
    assert_eq!(orientation.to_string(), "Orientation: Rotate 90 CW");
    let version = exif.entry(Tag::EXIF_VERSION).unwrap();
    assert_eq!(version.to_string(), "ExifVersion: 0300");
}

#[test]
fn big_endian() {
    // IFD0 at 8 with one SHORT entry: Orientation = 3.
    let data = [
        b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 3, 0, 0, 0, 0, 0, 0,
    ];
    let (exif, diagnostics) = read(&data);
    assert!(diagnostics.is_empty());
    assert_eq!(exif.get::<u16>(Tag::ORIENTATION), Ok(Some(3)));
}

#[test]
fn chain_cycle_terminates() {
    let mut t = Tiff::new();
    let ifd0 = t.pos();
    t.ifd(&[(0x0112, SHORT, 1, 1)], ifd0); // next → itself
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert_eq!(kinds(&diagnostics), [DiagnosticKind::Cycle]);
    assert_eq!(exif.ifds().len(), 1);
}

#[test]
fn pointer_cycle_terminates() {
    let mut t = Tiff::new();
    let exif_at = t.pos();
    // The Exif IFD's Interop pointer points back at the Exif IFD itself.
    t.ifd(&[(0xA005, LONG, 1, exif_at)], 0);
    let ifd0 = t.ifd(&[(0x8769, LONG, 1, exif_at)], 0);
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert_eq!(kinds(&diagnostics), [DiagnosticKind::Cycle]);
    assert!(exif.ifd(IfdKind::Exif).is_some());
    assert!(exif.ifd(IfdKind::Interop).is_none());
}

#[test]
fn deep_sub_ifd_nesting_is_capped() {
    // Each SubIFD points to a fresh SubIFD: depth grows until the limit.
    let mut t = Tiff::new();
    let mut child = t.ifd(&[], 0);
    for _ in 0..20 {
        child = t.ifd(&[(0x014A, LONG, 1, child)], 0);
    }
    let (exif, diagnostics) = read(&t.root(child));
    assert!(diagnostics.contains(DiagnosticKind::LimitExceeded(Limit::Depth)));
    assert_eq!(
        exif.ifds().len(),
        usize::from(Limits::DEFAULT.max_depth) + 1
    );
}

#[test]
fn out_of_bounds_value_is_dropped() {
    let mut t = Tiff::new();
    let ifd0 = t.ifd(&[(0x010F, ASCII, 100, 10_000), (0x0112, SHORT, 1, 1)], 0);
    let data = t.root(ifd0);
    let (exif, diagnostics) = read(&data);
    assert_eq!(kinds(&diagnostics), [DiagnosticKind::OutOfBounds]);
    assert_eq!(
        diagnostics.iter().next().unwrap().severity(),
        Severity::Error
    );
    assert!(exif.entry(Tag::MAKE).is_none());
    assert_eq!(exif.get::<u16>(Tag::ORIENTATION), Ok(Some(1)));

    let strict = Exif::from_tiff_with(&data, &ReadOptions::DEFAULT.strict(true));
    assert!(matches!(
        strict,
        Err(Error::Malformed { offset: 10_000, .. })
    ));
}

#[test]
fn huge_count_does_not_allocate() {
    let mut t = Tiff::new();
    let ifd0 = t.ifd(&[(0x9286, UNDEFINED, u32::MAX, 8)], 0);
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert_eq!(kinds(&diagnostics), [DiagnosticKind::OutOfBounds]);
    assert!(exif.ifd(IfdKind::Primary).unwrap().is_empty());
}

#[test]
fn unknown_type_is_kept_and_does_not_abort() {
    let mut t = Tiff::new();
    let ifd0 = t.ifd(&[(0x0112, SHORT, 1, 1), (0xC000, 99, 3, 0xDEAD_BEEF)], 0);
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert_eq!(kinds(&diagnostics), [DiagnosticKind::UnknownType(99)]);
    assert_eq!(exif.get::<u16>(Tag::ORIENTATION), Ok(Some(1)));
    let unknown = exif.entry(Tag::new(Namespace::Main, 0xC000)).unwrap();
    assert!(matches!(
        unknown.value(),
        Value::Unknown {
            type_id: 99,
            count: 3,
            ..
        }
    ));
}

#[test]
fn invalid_first_entry_means_a_corrupt_ifd() {
    let mut t = Tiff::new();
    // Not an IFD: the "first entry" has type 0x1B00.
    let gps = t.ifd(
        &[
            (0x0000, 0x1B00, 1, 0),
            (0x0001, ASCII, 2, inline(*b"N\0\0\0")),
        ],
        0,
    );
    let ifd0 = t.ifd(&[(0x0112, SHORT, 1, 1), (0x8825, LONG, 1, gps)], 0);
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert_eq!(kinds(&diagnostics), [DiagnosticKind::CorruptIfd]);
    assert!(exif.ifd(IfdKind::Gps).is_none());
    assert_eq!(exif.get::<u16>(Tag::ORIENTATION), Ok(Some(1)));
}

#[test]
fn zero_padding_is_skipped_silently() {
    let mut t = Tiff::new();
    let ifd0 = t.ifd(&[(0x0112, SHORT, 1, 1), (0, 0, 0, 0), (0, 0, 0, 0)], 0);
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(exif.ifd(IfdKind::Primary).unwrap().len(), 1);
}

#[test]
fn too_many_bad_entries_stop_the_ifd() {
    let mut t = Tiff::new();
    let mut entries = vec![(0x0112, SHORT, 1, 1)];
    entries.extend((1..=20).map(|i| (0xC000 + i, 99, 1, 0)));
    let ifd0 = t.ifd(&entries, 0);
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert!(diagnostics.contains(DiagnosticKind::CorruptIfd));
    let read = exif.ifd(IfdKind::Primary).unwrap().len();
    assert_eq!(read, 1 + MAX_BAD_ENTRIES + 1);
}

#[test]
fn utf8_type_is_supported() {
    let mut t = Tiff::new();
    let text = t.blob("Ünïcode\0".as_bytes());
    let exif_ifd = t.ifd(&[(0xA40E, UTF8, 10, text)], 0);
    let ifd0 = t.ifd(&[(0x8769, LONG, 1, exif_ifd)], 0);
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let value = exif.get::<String>(Tag::DEVELOPMENT_TYPE_DESCRIPTION);
    assert_eq!(value, Ok(Some("Ünïcode".into())));
}

#[test]
fn thumbnail_is_extracted() {
    let mut t = Tiff::new();
    let jpeg = t.blob(&[0xFF, 0xD8, 0xFF, 0xD9]);
    let ifd1 = t.ifd(&[(0x0201, LONG, 1, jpeg), (0x0202, LONG, 1, 4)], 0);
    let ifd0 = t.ifd(&[(0x0112, SHORT, 1, 1)], ifd1);
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(exif.thumbnail(), Some(&[0xFF, 0xD8, 0xFF, 0xD9][..]));
    assert!(exif.ifd(IfdKind::Thumbnail).unwrap().is_empty());
}

#[test]
fn bad_thumbnail_keeps_its_tags() {
    let mut t = Tiff::new();
    let ifd1 = t.ifd(&[(0x0201, LONG, 1, 5000), (0x0202, LONG, 1, 4)], 0);
    let ifd0 = t.ifd(&[], ifd1);
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert_eq!(kinds(&diagnostics), [DiagnosticKind::InvalidThumbnail]);
    assert_eq!(exif.thumbnail(), None);
    assert_eq!(exif.ifd(IfdKind::Thumbnail).unwrap().len(), 2);
}

#[test]
fn unsorted_and_duplicate_entries() {
    let mut t = Tiff::new();
    let ifd0 = t.ifd(
        &[
            (0x0112, SHORT, 1, 1),
            (0x0100, SHORT, 1, 64),
            (0x0112, SHORT, 1, 8),
        ],
        0,
    );
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert_eq!(
        kinds(&diagnostics),
        [
            DiagnosticKind::UnsortedEntries,
            DiagnosticKind::DuplicateTag
        ]
    );
    let ifd0 = exif.ifd(IfdKind::Primary).unwrap();
    let ids: Vec<u16> = ifd0.entries().iter().map(|e| e.tag().id()).collect();
    assert_eq!(ids, [0x0100, 0x0112]);
    assert_eq!(exif.get::<u16>(Tag::ORIENTATION), Ok(Some(1)));
    assert_eq!(ifd0.duplicates().len(), 1);
}

#[test]
fn misplaced_tag_is_still_found() {
    let mut t = Tiff::new();
    let exposure = t.blob(&[1, 0, 0, 0, 60, 0, 0, 0]);
    let ifd0 = t.ifd(&[(0x829A, RATIONAL, 1, exposure)], 0);
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert_eq!(
        kinds(&diagnostics),
        [DiagnosticKind::MisplacedTag {
            expected: IfdKind::Exif
        }]
    );
    assert_eq!(
        exif.get::<Rational>(Tag::EXPOSURE_TIME),
        Ok(Some(Rational::new(1, 60)))
    );
}

#[test]
fn type_and_count_mismatches_keep_the_value() {
    let mut t = Tiff::new();
    let ifd0 = t.ifd(
        &[
            (0x0112, LONG, 1, 6),
            (0x0132, ASCII, 2, inline(*b"x\0\0\0")),
        ],
        0,
    );
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert_eq!(
        kinds(&diagnostics),
        [
            DiagnosticKind::TypeMismatch { found: Type::Long },
            DiagnosticKind::CountMismatch { found: 2 },
        ]
    );
    // A LONG orientation still converts losslessly.
    assert_eq!(
        exif.get::<Orientation>(Tag::ORIENTATION),
        Ok(Some(Orientation::Rotate90))
    );
}

#[test]
fn shared_pointer_is_not_a_duplicate() {
    let mut t = Tiff::new();
    let exif_ifd = t.ifd(&[(0x9000, UNDEFINED, 4, inline(*b"0232"))], 0);
    let ifd1 = t.ifd(&[(0x8769, LONG, 1, exif_ifd)], 0);
    let ifd0 = t.ifd(&[(0x8769, LONG, 1, exif_ifd)], ifd1);
    let (_, diagnostics) = read(&t.root(ifd0));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn allocation_budget_stops_the_walk() {
    let mut t = Tiff::new();
    let blob = t.blob(&[0; 512]);
    let ifd0 = t.ifd(&[(0x9286, UNDEFINED, 512, blob), (0x0112, SHORT, 1, 1)], 0);
    let mut limits = Limits::DEFAULT;
    limits.max_alloc = Some(256);
    let options = ReadOptions::DEFAULT.limits(limits);
    let (_, diagnostics) = Exif::from_tiff_with(&t.root(ifd0), &options).unwrap();
    assert!(diagnostics.contains(DiagnosticKind::LimitExceeded(Limit::Alloc)));
    let strict = Exif::from_tiff_with(&camera(), &options.strict(true));
    assert!(matches!(strict, Err(Error::LimitExceeded(Limit::Alloc))));
}

#[test]
fn invalid_ifd0_is_lenient_by_default() {
    let data = Tiff::new().root(1000); // IFD0 past the end of the data
    let (exif, diagnostics) = read(&data);
    assert_eq!(kinds(&diagnostics), [DiagnosticKind::InvalidIfdOffset]);
    assert!(exif.ifds().is_empty());
    assert!(Exif::from_tiff_with(&data, &ReadOptions::DEFAULT.strict(true)).is_err());
    // An IFD0 overlapping the header is a broken header, not a broken IFD.
    assert!(matches!(
        Exif::from_tiff(&Tiff::new().root(4)),
        Err(Error::InvalidHeader)
    ));
}

#[test]
fn invalid_header_is_an_error() {
    assert!(matches!(
        Exif::from_tiff(b"nope"),
        Err(Error::InvalidHeader)
    ));
    assert!(matches!(Exif::from_tiff(&[]), Err(Error::InvalidHeader)));
}

#[test]
fn diagnostics_display() {
    let mut t = Tiff::new();
    let ifd0 = t.ifd(&[(0x010F, ASCII, 100, 10_000)], 0);
    let (_, diagnostics) = read(&t.root(ifd0));
    let text: Vec<String> = diagnostics.iter().map(ToString::to_string).collect();
    assert_eq!(
        text,
        vec!["value out of bounds, tag Make, in Primary, at offset 10000"]
    );
}

/// A cheap, deterministic stand-in for fuzzing: every single-byte mutation
/// and every truncation of a realistic block must read without panicking,
/// and strict mode must agree with the lenient diagnostics.
#[test]
fn survives_every_single_byte_mutation_and_truncation() {
    let original = camera();
    let check = |data: &[u8]| {
        if let Ok((exif, diagnostics)) = Exif::from_tiff(data) {
            for (_, entry) in exif.entries() {
                let _ = entry.to_string();
            }
            for diagnostic in &diagnostics {
                let _ = diagnostic.to_string();
            }
            let strict = Exif::from_tiff_with(data, &ReadOptions::DEFAULT.strict(true));
            assert_eq!(strict.is_ok(), diagnostics.first_error().is_none());
        }
    };
    for len in 0..original.len() {
        check(&original[..len]);
    }
    let mut data = original.clone();
    for i in 0..data.len() {
        for byte in [0x00, 0x01, 0x07, 0x0D, 0x81, 0xFE, 0xFF] {
            data[i] = byte;
            check(&data);
        }
        data[i] = original[i];
    }
}

#[test]
fn sub_ifds_are_numbered_canonically() {
    // A SubIFDs list whose first two pointers are invalid: the reader used to
    // hand out numbers 0 and 1 to them and call the real one SubIfd(2).
    let mut t = Tiff::new();
    let sub = t.ifd(&[(0x0100, SHORT, 1, 16)], 0);
    let mut offsets = vec![0xFFu8, 0xFF, 0, 0, 0xFF, 0xFF, 0, 0];
    offsets.extend_from_slice(&sub.to_le_bytes());
    let list = t.blob(&offsets);
    let ifd0 = t.ifd(&[(0x014A, LONG, 3, list)], 0);
    let (exif, _) = read(&t.root(ifd0));
    let subs: Vec<IfdKind> = exif
        .ifds()
        .iter()
        .map(Ifd::kind)
        .filter(|k| matches!(k, IfdKind::SubIfd(_)))
        .collect();
    assert_eq!(subs, [IfdKind::SubIfd(0)]);
    let back = read(&exif.to_tiff().unwrap()).0;
    assert_eq!(back, exif);
}

#[test]
fn only_the_kept_maker_note_is_tracked() {
    // An inline MakerNote, then a long duplicate that is not kept. The
    // tracked offset must not come from the duplicate (found by fuzzing:
    // it made the writer invent an OffsetSchema).
    let mut t = Tiff::new();
    let long = t.blob(&[1; 10]);
    let exif_ifd = t.ifd(
        &[
            (0x927C, UNDEFINED, 4, inline(*b"abcd")),
            (0x927C, UNDEFINED, 10, long),
        ],
        0,
    );
    let ifd0 = t.ifd(&[(0x8769, LONG, 1, exif_ifd)], 0);
    let (exif, _) = read(&t.root(ifd0));
    assert_eq!(exif.maker_note_offset(), None);
    let once = exif.to_tiff().unwrap();
    let back = read(&once).0;
    assert_eq!(back.offset_schema(), None);
    assert_eq!(back.to_tiff().unwrap(), once);
}

#[test]
fn tracked_maker_note_follows_sub_ifd_renumbering() {
    // A MakerNote in a SubIFD whose number changes when SubIFDs are
    // renumbered (the first pointer is invalid). Found by fuzzing: the
    // tracking pointed at the old number, so a moved MakerNote got no
    // OffsetSchema on the first write but one on the second.
    let mut t = Tiff::new();
    let note = t.blob(&[7; 9]);
    let sub = t.ifd(&[(0x927C, UNDEFINED, 9, note)], 0);
    let mut offsets = vec![0xFFu8, 0xFF, 0, 0];
    offsets.extend_from_slice(&sub.to_le_bytes());
    let list = t.blob(&offsets);
    let ifd0 = t.ifd(&[(0x014A, LONG, 2, list)], 0);
    let (exif, _) = read(&t.root(ifd0));
    let options = crate::WriteOptions::DEFAULT.maker_note(crate::MakerNotePolicy::Move);
    let once = exif.to_tiff_with(&options).unwrap();
    let back = read(&once).0;
    assert!(back.offset_schema().is_some(), "the move is recorded");
    assert_eq!(
        back.to_tiff_with(&options).unwrap(),
        once,
        "and writing again is stable"
    );
}

#[test]
fn sub_ifd_limit_is_per_parent_across_duplicate_tags() {
    // Found by fuzzing: two SubIFDs tags each under the limit gave one IFD
    // more children than the limit; the writer merges them into one tag,
    // which then read back truncated.
    let mut t = Tiff::new();
    let children: Vec<u32> = (1..=3)
        .map(|w| t.ifd(&[(0x0100, SHORT, 1, w)], 0))
        .collect();
    let first = t.blob(&[children[0], children[1]].map(u32::to_le_bytes).concat());
    let ifd0 = t.ifd(
        &[(0x014A, LONG, 2, first), (0x014A, LONG, 1, children[2])],
        0,
    );
    let mut limits = Limits::DEFAULT;
    limits.max_sub_ifds = 2;
    let options = ReadOptions::DEFAULT.limits(limits);
    let (exif, diagnostics) = Exif::from_tiff_with(&t.root(ifd0), &options).unwrap();
    assert!(diagnostics.contains(DiagnosticKind::LimitExceeded(Limit::SubIfds)));
    let subs = exif
        .ifds()
        .iter()
        .filter(|i| matches!(i.kind(), IfdKind::SubIfd(_)))
        .count();
    assert_eq!(subs, 2);
    let (back, _) = Exif::from_tiff_with(&exif.to_tiff().unwrap(), &options).unwrap();
    assert_eq!(back, exif);
}

#[test]
fn duplicate_sub_ifd_pointers_number_by_discovery() {
    // Two SubIFDs tags in IFD0; the second one's children must not tie with
    // the first one's (found by fuzzing: the writer merges both lists).
    let mut t = Tiff::new();
    let a = t.ifd(&[(0x0100, SHORT, 1, 1)], 0);
    let b = t.ifd(&[(0x0100, SHORT, 1, 2)], 0);
    let c = t.ifd(&[(0x0100, SHORT, 1, 3)], 0);
    let ifd0 = t.ifd(
        &[
            (0x014A, LONG, 1, a),
            (0x014A, LONG, 1, b),
            (0x014A, LONG, 1, c),
        ],
        0,
    );
    let (exif, _) = read(&t.root(ifd0));
    let widths: Vec<u16> = (0..3)
        .filter_map(|n| exif.ifd(IfdKind::SubIfd(n))?.get(Tag::IMAGE_WIDTH))
        .filter_map(|e| u16::from_value(e.value()).ok())
        .collect();
    assert_eq!(widths, [1, 2, 3]);
    let back = read(&exif.to_tiff().unwrap()).0;
    assert_eq!(back, exif);
}

#[test]
fn malformed_offset_schema_is_dropped() {
    // Several SLONGs instead of one (found by fuzzing: kept as an entry, it
    // was silently lost on write, since the writer owns OffsetSchema).
    let mut t = Tiff::new();
    let values = t.blob(&[1, 0, 0, 0, 2, 0, 0, 0]);
    let exif_ifd = t.ifd(&[(0xEA1D, 9, 2, values)], 0);
    let ifd0 = t.ifd(&[(0x8769, LONG, 1, exif_ifd)], 0);
    let (exif, diagnostics) = read(&t.root(ifd0));
    assert!(diagnostics.contains(DiagnosticKind::CountMismatch { found: 2 }));
    assert_eq!(exif.offset_schema(), None);
    assert!(exif.ifd(IfdKind::Exif).unwrap().is_empty());
}
