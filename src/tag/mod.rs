//! Tag identity and the static tag table. See `docs/format.md` §6.
//!
//! The table lives in `data/tags.toml`. `cargo xtask codegen` turns it into
//! `generated.rs` and `enums.rs`, which are committed and must not be edited.

mod enums;
mod generated;

use core::fmt;

pub use self::enums::*;
use self::generated::{NAMES, TAGS};
use crate::ifd::IfdKind;
use crate::tiff::Type;

/// A tag id space.
///
/// IFD0, IFD1, the Exif IFD and SubIFDs share one id space ([`Main`]). The GPS
/// and Interoperability IFDs reuse low ids, so each needs its own.
///
/// [`Main`]: Namespace::Main
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Namespace {
    /// Tags of IFD0, IFD1, the Exif IFD and SubIFDs.
    Main,
    /// Tags of the GPS IFD.
    Gps,
    /// Tags of the Interoperability IFD.
    Interop,
}

/// A tag: an id within a [`Namespace`].
///
/// Known tags are available as associated constants, such as
/// [`Tag::ORIENTATION`]. Unknown tags are just as valid: build them with
/// [`Tag::new`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Tag {
    ns: Namespace,
    id: u16,
}

impl Tag {
    /// Creates a tag from its namespace and id.
    #[must_use]
    pub const fn new(ns: Namespace, id: u16) -> Self {
        Self { ns, id }
    }

    /// Returns the namespace.
    #[must_use]
    pub const fn namespace(self) -> Namespace {
        self.ns
    }

    /// Returns the numeric id.
    #[must_use]
    pub const fn id(self) -> u16 {
        self.id
    }

    /// Returns the table entry for this tag, or `None` if it is unknown.
    #[must_use]
    pub fn info(self) -> Option<&'static TagInfo> {
        TAGS.binary_search_by_key(&self, |info| info.tag)
            .ok()
            .and_then(|i| TAGS.get(i))
    }

    /// Returns the spec name, such as `"DateTimeDigitized"`.
    #[must_use]
    pub fn name(self) -> Option<&'static str> {
        self.info().map(|info| info.name)
    }

    /// Looks a tag up by its spec name or its ExifTool alias.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        NAMES
            .binary_search_by_key(&name, |&(n, _)| n)
            .ok()
            .and_then(|i| NAMES.get(i))
            .and_then(|&(_, index)| TAGS.get(usize::from(index)))
            .map(|info| info.tag)
    }

    /// Returns every known tag, ordered by namespace and id.
    pub fn all() -> impl ExactSizeIterator<Item = &'static TagInfo> {
        TAGS.iter()
    }
}

impl fmt::Debug for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => write!(f, "{name}"),
            None => write!(f, "{:?}(0x{:04X})", self.ns, self.id),
        }
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// How many values a tag holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    /// Any number of values.
    Any,
    /// Exactly this many values.
    Fixed(u32),
    /// One of these counts.
    OneOf(&'static [u32]),
}

impl Count {
    /// Returns whether `count` is allowed.
    #[must_use]
    pub fn allows(self, count: u32) -> bool {
        match self {
            Self::Any => true,
            Self::Fixed(n) => n == count,
            Self::OneOf(ns) => ns.contains(&count),
        }
    }
}

/// What the writer does with a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Role {
    /// An ordinary value, written as the model holds it.
    Value,
    /// The offset of a child IFD. Computed by the writer.
    Pointer(IfdKind),
    /// The offset of image or thumbnail data. Computed by the writer.
    DataOffset,
    /// The length of image or thumbnail data. Computed by the writer.
    DataLength,
    /// The vendor MakerNote blob. See `docs/writing.md`.
    MakerNote,
    /// Another value the writer computes, such as `OffsetSchema`.
    Computed,
}

/// Which document defines a tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Origin {
    /// TIFF 6.0 or its technical notes.
    Tiff,
    /// TIFF/EP (ISO 12234-2).
    TiffEp,
    /// CIPA DC-008 (Exif).
    Exif,
    /// Adobe DNG.
    Dng,
    /// Adobe, outside DNG (for example XMP in TIFF).
    Adobe,
    /// Microsoft (Windows Photo tags, `OffsetSchema`, `Padding`).
    Microsoft,
    /// Any other source.
    Other,
}

/// An Exif version, such as 2.32 or 3.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Major version.
    pub major: u8,
    /// Minor version, as written: 2.32 is `{ major: 2, minor: 32 }`.
    pub minor: u8,
}

impl Version {
    /// Creates a version.
    #[must_use]
    pub const fn new(major: u8, minor: u8) -> Self {
        Self { major, minor }
    }
}

/// Whether a string tag is ASCII, UTF-8 or either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringKind {
    /// ASCII only.
    Ascii,
    /// ASCII or, since Exif 3.0, UTF-8.
    AsciiOrUtf8,
    /// UTF-8 only.
    Utf8,
}

/// Static description of a known tag.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct TagInfo {
    /// The tag.
    pub tag: Tag,
    /// Spec name.
    pub name: &'static str,
    /// ExifTool name, when it differs from the spec name.
    pub alias: Option<&'static str>,
    /// The IFD the tag belongs in.
    pub ifd: IfdKind,
    /// Allowed field types, preferred type first.
    pub types: &'static [Type],
    /// Allowed number of values.
    pub count: Count,
    /// What the writer does with the tag.
    pub role: Role,
    /// Which document defines the tag.
    pub origin: Origin,
    /// Exif version that introduced the tag, when known and 2.3 or later.
    pub since: Option<Version>,
    /// Whether the tag can identify a person (removed by `Strip::Personal`).
    pub personal: bool,
    /// Named values, for tags that hold an enumeration.
    pub values: &'static [(u16, &'static str)],
}

impl TagInfo {
    /// Returns the string kind, or `None` if the tag is not a string.
    #[must_use]
    pub fn string_kind(&self) -> Option<StringKind> {
        let ascii = self.types.contains(&Type::Ascii);
        let utf8 = self.types.contains(&Type::Utf8);
        match (ascii, utf8) {
            (true, true) => Some(StringKind::AsciiOrUtf8),
            (true, false) => Some(StringKind::Ascii),
            (false, true) => Some(StringKind::Utf8),
            (false, false) => None,
        }
    }

    /// Returns the description of a named value, if the tag defines it.
    #[must_use]
    pub fn describe(&self, value: u16) -> Option<&'static str> {
        self.values
            .iter()
            .find(|&&(v, _)| v == value)
            .map(|&(_, desc)| desc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_unique() {
        assert!(TAGS.windows(2).all(|w| w[0].tag < w[1].tag));
        assert!(NAMES.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn lookup_by_tag_and_name() {
        for info in Tag::all() {
            assert_eq!(info.tag.info().map(|i| i.name), Some(info.name));
            assert_eq!(Tag::from_name(info.name), Some(info.tag));
            if let Some(alias) = info.alias {
                assert_eq!(Tag::from_name(alias), Some(info.tag));
            }
        }
        assert_eq!(Tag::from_name("CreateDate"), Some(Tag::DATE_TIME_DIGITIZED));
        assert_eq!(
            Tag::new(Namespace::Main, 0xFFFF).info().map(|i| i.name),
            None
        );
    }

    #[test]
    fn namespaces_keep_overlapping_ids_apart() {
        assert_eq!(Tag::GPS_LATITUDE_REF.id(), Tag::INTEROPERABILITY_INDEX.id());
        assert_ne!(Tag::GPS_LATITUDE_REF, Tag::INTEROPERABILITY_INDEX);
    }

    #[test]
    fn enums_round_trip() {
        assert_eq!(Orientation::from_u16(6), Orientation::Rotate90);
        assert_eq!(u16::from(Orientation::Rotate90), 6);
        assert_eq!(Orientation::from_u16(99), Orientation::Other(99));
        assert_eq!(LightSource::from_u16(34), LightSource::WarmWhiteLed);
    }

    #[test]
    fn string_kinds() {
        let kind = |tag: Tag| tag.info().and_then(TagInfo::string_kind);
        assert_eq!(kind(Tag::DATE_TIME), Some(StringKind::Ascii));
        assert_eq!(kind(Tag::ARTIST), Some(StringKind::AsciiOrUtf8));
        assert_eq!(
            kind(Tag::DEVELOPMENT_TYPE_DESCRIPTION),
            Some(StringKind::Utf8)
        );
        assert_eq!(kind(Tag::ORIENTATION), None);
        // DC-008-2026 Table 18: serial numbers are ASCII only.
        assert_eq!(kind(Tag::BODY_SERIAL_NUMBER), Some(StringKind::Ascii));
        assert_eq!(kind(Tag::LENS_SERIAL_NUMBER), Some(StringKind::Ascii));
    }

    #[test]
    fn spec_corrections() {
        // Table 18 says RATIONAL; the definition in section 4.6.6.7.41 says SHORT.
        let gain = Tag::GAIN_CONTROL.info().map(|i| i.types);
        assert_eq!(gain, Some(&[Type::Short][..]));
        // Exif 3.0 redefined GPSAltitudeRef and added the sea-level values.
        assert_eq!(AltitudeRef::from_u16(2), AltitudeRef::AboveSeaLevel);
        assert_eq!(
            Tag::GPS_ALTITUDE_REF.info().and_then(|i| i.describe(1)),
            Some("Negative ellipsoidal height (below the ellipsoidal surface)")
        );
    }
}
