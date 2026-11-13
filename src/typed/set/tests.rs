use alloc::string::ToString;
use alloc::vec;

use super::*;
use crate::tag::{FileSource, Orientation};
use crate::typed::datetime::{Offset, SubSec};

fn exif_version(exif: &Exif) -> Option<Vec<u8>> {
    match exif.ifd(IfdKind::Exif)?.get(Tag::EXIF_VERSION)?.value() {
        Value::Undefined(v) => Some(v.clone()),
        _ => None,
    }
}

fn invalid(result: Result<Option<Value>>) -> Option<ValueError> {
    match result {
        Err(Error::InvalidValue { error, .. }) => Some(error),
        _ => None,
    }
}

#[test]
fn integers_take_the_tags_type() {
    let mut exif = Exif::new();
    exif.set(Tag::ORIENTATION, Orientation::Rotate180).unwrap();
    assert_eq!(
        exif.entry(Tag::ORIENTATION).unwrap().value(),
        &Value::Short(vec![3])
    );
    // The table's preferred type first: ImageWidth prefers LONG.
    exif.set(Tag::IMAGE_WIDTH, 640u32).unwrap();
    assert_eq!(
        exif.entry(Tag::IMAGE_WIDTH).unwrap().value(),
        &Value::Long(vec![640])
    );
    // A preferred type the value does not fit is passed over: SubjectArea is
    // SHORT only, so a big value has nowhere to go.
    assert_eq!(
        invalid(exif.set(Tag::SUBJECT_AREA, [70_000u32, 1])),
        Some(ValueError::OutOfRange)
    );
    // A rational-only tag takes n/1.
    exif.set(Tag::X_RESOLUTION, 300u16).unwrap();
    assert_eq!(
        exif.entry(Tag::X_RESOLUTION).unwrap().value(),
        &Value::Rational(vec![Rational::new(300, 1)])
    );
    // A single-byte UNDEFINED enum.
    exif.set(Tag::FILE_SOURCE, FileSource::Dsc).unwrap();
    assert_eq!(
        exif.entry(Tag::FILE_SOURCE).unwrap().value(),
        &Value::Undefined(vec![3])
    );
    assert_eq!(
        exif.get::<FileSource>(Tag::FILE_SOURCE).unwrap(),
        Some(FileSource::Dsc)
    );
    // Too big for SHORT, and SHORT is all Orientation allows.
    assert_eq!(
        invalid(exif.set(Tag::ORIENTATION, 70_000u32)),
        Some(ValueError::OutOfRange)
    );
}

#[test]
fn values_are_checked_against_the_table() {
    let mut exif = Exif::new();
    // Not a defined enumeration value.
    assert_eq!(
        invalid(exif.set(Tag::ORIENTATION, 42u16)),
        Some(ValueError::OutOfRange)
    );
    assert_eq!(
        invalid(exif.set(Tag::FILE_SOURCE, 9u8)),
        Some(ValueError::OutOfRange)
    );
    // GPSLatitude is three rationals.
    assert_eq!(
        invalid(exif.set(Tag::GPS_LATITUDE, Rational::new(1, 1))),
        Some(ValueError::CountNotAllowed { found: 1 })
    );
    exif.set(
        Tag::GPS_LATITUDE,
        [
            Rational::new(50, 1),
            Rational::new(27, 1),
            Rational::new(0, 1),
        ],
    )
    .unwrap();
    // A float never becomes a rational implicitly.
    assert!(matches!(
        invalid(exif.set(Tag::EXPOSURE_TIME, 0.004f64)),
        Some(ValueError::WrongType { .. })
    ));
    // A raw Value is checked too.
    assert!(matches!(
        invalid(exif.set(Tag::ORIENTATION, Value::Long(vec![1]))),
        Some(ValueError::WrongType { .. })
    ));
    // Computed tags are refused, as by insert.
    assert!(matches!(
        exif.set(Tag::STRIP_OFFSETS, 8u32),
        Err(Error::InvalidEntry { .. })
    ));
    // Failures leave the model alone.
    assert!(exif.entry(Tag::ORIENTATION).is_none());
    // Unknown tags are stored unchecked.
    let unknown = Tag::new(crate::tag::Namespace::Main, 0xC0DE);
    exif.set(unknown, 7u16).unwrap();
    assert_eq!(exif.entry(unknown).unwrap().value(), &Value::Short(vec![7]));
}

#[test]
fn text_picks_ascii_or_utf8_and_raises_the_version() {
    let mut exif = Exif::new();
    exif.set(Tag::ARTIST, "Jane Doe").unwrap();
    assert_eq!(
        exif.entry(Tag::ARTIST).unwrap().value(),
        &Value::ascii("Jane Doe")
    );
    assert_eq!(exif_version(&exif), None, "ASCII needs no version");
    exif.ifd_mut(IfdKind::Exif)
        .insert(Entry::new(
            Tag::EXIF_VERSION,
            Value::Undefined(b"0232".to_vec()),
        ))
        .unwrap();
    let previous = exif.set(Tag::ARTIST, "Олена").unwrap();
    assert_eq!(previous, Some(Value::ascii("Jane Doe")));
    assert_eq!(
        exif.entry(Tag::ARTIST).unwrap().value(),
        &Value::utf8("Олена")
    );
    assert_eq!(exif_version(&exif).as_deref(), Some(&b"0300"[..]));
    // Serial numbers are ASCII only, even in Exif 3.1.
    assert!(matches!(
        invalid(exif.set(Tag::BODY_SERIAL_NUMBER, "№1")),
        Some(ValueError::WrongType { .. })
    ));
    // NUL would end the string early.
    assert_eq!(
        invalid(exif.set(Tag::ARTIST, "a\0b")),
        Some(ValueError::Invalid)
    );
    // An UNDEFINED tag takes the bytes as they are.
    exif.set(Tag::EXIF_VERSION, "0310").unwrap();
    assert_eq!(exif_version(&exif).as_deref(), Some(&b"0310"[..]));
    // A later version is never lowered.
    exif.set(Tag::IMAGE_DESCRIPTION, "Café").unwrap();
    assert_eq!(exif_version(&exif).as_deref(), Some(&b"0310"[..]));
}

#[test]
fn exif_3_tags_raise_the_version() {
    let mut exif = Exif::new();
    exif.set(Tag::DEVELOPMENT_TYPE_DESCRIPTION, "raw").unwrap();
    // UTF-8 is the only type this tag allows, ASCII text or not.
    assert_eq!(
        exif.entry(Tag::DEVELOPMENT_TYPE_DESCRIPTION)
            .unwrap()
            .value(),
        &Value::utf8("raw")
    );
    assert_eq!(exif_version(&exif).as_deref(), Some(&b"0300"[..]));
}

#[test]
fn date_times_write_all_three_tags() {
    let mut exif = Exif::new();
    let mut taken = DateTime::parse("2026:01:30 12:34:56").unwrap().unwrap();
    taken.subsec = SubSec::parse("120").unwrap();
    taken.offset = Offset::from_minutes(9 * 60);
    exif.set_date_time_original(taken).unwrap();
    assert_eq!(exif.date_time_original().unwrap(), Some(taken));
    assert_eq!(
        exif.entry(Tag::OFFSET_TIME_ORIGINAL).unwrap().value(),
        &Value::ascii("+09:00")
    );
    // Dropping the parts removes their tags, so nothing stale remains.
    let plain = DateTime {
        subsec: None,
        offset: None,
        ..taken
    };
    exif.set_date_time_original(plain).unwrap();
    assert_eq!(exif.date_time_original().unwrap(), Some(plain));
    assert!(exif.entry(Tag::SUB_SEC_TIME_ORIGINAL).is_none());
    assert!(exif.entry(Tag::OFFSET_TIME_ORIGINAL).is_none());
    // An impossible date changes nothing.
    let bad = DateTime { month: 13, ..taken };
    assert!(exif.set_date_time_original(bad).is_err());
    assert_eq!(exif.date_time_original().unwrap(), Some(plain));
}

#[test]
fn encoded_text_and_copyright_round_trip() {
    let mut exif = Exif::new();
    for order in [ByteOrder::LittleEndian, ByteOrder::BigEndian] {
        exif.set_byte_order(order);
        for text in ["hello", "привіт 👋"] {
            exif.set(Tag::USER_COMMENT, EncodedText::new(text, order))
                .unwrap();
            let back = exif.user_comment().unwrap().unwrap();
            assert_eq!(back.text().as_deref(), Some(text));
        }
    }
    // Plain text gets the ASCII prefix, so the getter reads it back.
    exif.set(Tag::USER_COMMENT, "hi").unwrap();
    assert_eq!(
        exif.user_comment().unwrap().unwrap().text().as_deref(),
        Some("hi")
    );
    assert!(matches!(
        invalid(exif.set(Tag::USER_COMMENT, "привіт")),
        Some(ValueError::WrongType { .. })
    ));
    // ExifVersion still takes the bytes as they are.
    exif.set(Tag::EXIF_VERSION, "0300").unwrap();
    let copyright = Copyright {
        photographer: None,
        editor: Some("Editor".to_string()),
    };
    exif.set(Tag::COPYRIGHT, copyright.clone()).unwrap();
    assert_eq!(
        exif.entry(Tag::COPYRIGHT).unwrap().value(),
        &Value::Ascii(b" \0Editor\0".to_vec())
    );
    assert_eq!(exif.copyright().unwrap(), Some(copyright));
    // "©" needs UTF-8, which Exif 3.0 allows for Copyright.
    let copyright = Copyright {
        photographer: Some("© 2026 Олена".to_string()),
        editor: Some("Editor".to_string()),
    };
    exif.set(Tag::COPYRIGHT, copyright.clone()).unwrap();
    assert!(matches!(
        exif.entry(Tag::COPYRIGHT).unwrap().value(),
        Value::Utf8(_)
    ));
    assert_eq!(exif.copyright().unwrap(), Some(copyright));
    assert_eq!(exif_version(&exif).as_deref(), Some(&b"0300"[..]));
}

#[test]
fn set_values_survive_writing() {
    let mut exif = Exif::new();
    exif.set(Tag::ARTIST, "Олена").unwrap();
    exif.set(Tag::ORIENTATION, Orientation::Rotate180).unwrap();
    exif.set(Tag::X_RESOLUTION, 72u32).unwrap();
    exif.set(Tag::SUBJECT_AREA, [10u16, 20, 30]).unwrap();
    let (back, diagnostics) = Exif::from_tiff(&exif.to_tiff().unwrap()).unwrap();
    assert_eq!(back, exif);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
