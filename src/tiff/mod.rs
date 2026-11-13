//! The TIFF structure that carries Exif data. See `docs/format.md` §1.
//!
//! This is the zero-copy parse layer: [`Reader`] gives bounds-checked access to
//! a TIFF block, and [`RawIfd`] / [`RawEntry`] walk one directory without
//! allocating. The owned model in [`Exif`](crate::Exif) is built on top.

mod ifd;
mod reader;

use core::fmt;

pub use self::ifd::{DataError, RawEntry, RawIfd};
pub(crate) use self::reader::Paged;
pub use self::reader::Reader;
use crate::error::{Error, Result};

/// Byte order of a TIFF structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ByteOrder {
    /// `II`: little-endian (Intel).
    LittleEndian,
    /// `MM`: big-endian (Motorola).
    BigEndian,
}

impl ByteOrder {
    /// Decodes a `u16` stored in this byte order.
    #[must_use]
    pub const fn u16(self, bytes: [u8; 2]) -> u16 {
        match self {
            Self::LittleEndian => u16::from_le_bytes(bytes),
            Self::BigEndian => u16::from_be_bytes(bytes),
        }
    }

    /// Decodes a `u32` stored in this byte order.
    #[must_use]
    pub const fn u32(self, bytes: [u8; 4]) -> u32 {
        match self {
            Self::LittleEndian => u32::from_le_bytes(bytes),
            Self::BigEndian => u32::from_be_bytes(bytes),
        }
    }

    /// Decodes a `u64` stored in this byte order.
    #[must_use]
    pub const fn u64(self, bytes: [u8; 8]) -> u64 {
        match self {
            Self::LittleEndian => u64::from_le_bytes(bytes),
            Self::BigEndian => u64::from_be_bytes(bytes),
        }
    }
}

/// The field type of an IFD entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Type {
    /// 1: 8-bit unsigned integer.
    Byte,
    /// 2: 7-bit ASCII text, NUL-terminated.
    Ascii,
    /// 3: 16-bit unsigned integer.
    Short,
    /// 4: 32-bit unsigned integer.
    Long,
    /// 5: two `Long`s, numerator then denominator.
    Rational,
    /// 6: 8-bit signed integer.
    SByte,
    /// 7: 8-bit byte whose meaning depends on the tag.
    Undefined,
    /// 8: 16-bit signed integer.
    SShort,
    /// 9: 32-bit signed integer.
    SLong,
    /// 10: two `SLong`s, numerator then denominator.
    SRational,
    /// 11: IEEE 754 single-precision float.
    Float,
    /// 12: IEEE 754 double-precision float.
    Double,
    /// 13: offset of a child IFD (TIFF/EP, DNG).
    Ifd,
    /// 129: UTF-8 text, NUL-terminated (Exif 3.0).
    Utf8,
}

impl Type {
    /// Returns the type for a field-type id, or `None` if it is unknown.
    #[must_use]
    pub const fn from_u16(id: u16) -> Option<Self> {
        Some(match id {
            1 => Self::Byte,
            2 => Self::Ascii,
            3 => Self::Short,
            4 => Self::Long,
            5 => Self::Rational,
            6 => Self::SByte,
            7 => Self::Undefined,
            8 => Self::SShort,
            9 => Self::SLong,
            10 => Self::SRational,
            11 => Self::Float,
            12 => Self::Double,
            13 => Self::Ifd,
            129 => Self::Utf8,
            _ => return None,
        })
    }

    /// Returns the field-type id written to the file.
    #[must_use]
    pub const fn to_u16(self) -> u16 {
        match self {
            Self::Byte => 1,
            Self::Ascii => 2,
            Self::Short => 3,
            Self::Long => 4,
            Self::Rational => 5,
            Self::SByte => 6,
            Self::Undefined => 7,
            Self::SShort => 8,
            Self::SLong => 9,
            Self::SRational => 10,
            Self::Float => 11,
            Self::Double => 12,
            Self::Ifd => 13,
            Self::Utf8 => 129,
        }
    }

    /// Returns the size of one value of this type, in bytes.
    #[must_use]
    pub const fn size(self) -> u8 {
        match self {
            Self::Byte | Self::Ascii | Self::SByte | Self::Undefined | Self::Utf8 => 1,
            Self::Short | Self::SShort => 2,
            Self::Long | Self::SLong | Self::Float | Self::Ifd => 4,
            Self::Rational | Self::SRational | Self::Double => 8,
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Byte => "BYTE",
            Self::Ascii => "ASCII",
            Self::Short => "SHORT",
            Self::Long => "LONG",
            Self::Rational => "RATIONAL",
            Self::SByte => "SBYTE",
            Self::Undefined => "UNDEFINED",
            Self::SShort => "SSHORT",
            Self::SLong => "SLONG",
            Self::SRational => "SRATIONAL",
            Self::Float => "FLOAT",
            Self::Double => "DOUBLE",
            Self::Ifd => "IFD",
            Self::Utf8 => "UTF-8",
        })
    }
}

/// The 8-byte header at the start of a TIFF structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Header {
    /// Byte order of every multi-byte value in the structure.
    pub byte_order: ByteOrder,
    /// Offset of IFD0, relative to the start of the header.
    pub ifd0_offset: u32,
    /// The magic number: [`Header::TIFF`] (42), or a raw format's own value
    /// for a TIFF structure under another name.
    pub magic: u16,
}

impl Header {
    /// Size of the header, in bytes.
    pub const LEN: usize = 8;
    /// Olympus ORF (`IIRO` or `MMOR`).
    pub const ORF: u16 = 0x4F52;
    /// Olympus ORF from some models (`IIRS`).
    pub const ORF_RS: u16 = 0x5352;
    /// Panasonic RW2 and Leica RWL (`IIU\0`).
    pub const RW2: u16 = 0x55;
    /// Standard TIFF.
    pub const TIFF: u16 = 42;

    /// Whether a header with this byte order and magic is one readers
    /// recognize: TIFF in either order, ORF as `IIRO` or `MMOR`, and ORF
    /// `IIRS` and RW2 (`IIU\0`) little-endian only.
    #[must_use]
    pub const fn is_known(byte_order: ByteOrder, magic: u16) -> bool {
        matches!(
            (byte_order, magic),
            (_, Self::TIFF | Self::ORF) | (ByteOrder::LittleEndian, Self::ORF_RS | Self::RW2)
        )
    }

    /// Creates a standard TIFF header.
    #[must_use]
    pub const fn new(byte_order: ByteOrder, ifd0_offset: u32) -> Self {
        Self {
            byte_order,
            ifd0_offset,
            magic: Self::TIFF,
        }
    }

    /// Parses the header at the start of `data`.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] for BigTIFF (magic 43).
    /// - [`Error::InvalidHeader`] if `data` is shorter than 8 bytes, the
    ///   byte-order mark or magic number is wrong, or IFD0 would overlap
    ///   the header. Besides 42, the ORF and RW2 magic numbers are accepted
    ///   (their structure is TIFF).
    pub fn parse(data: &[u8]) -> Result<Self> {
        let Some(&[b0, b1, m0, m1, o0, o1, o2, o3]) = data.first_chunk::<8>() else {
            return Err(Error::InvalidHeader);
        };
        let byte_order = match [b0, b1] {
            [b'I', b'I'] => ByteOrder::LittleEndian,
            [b'M', b'M'] => ByteOrder::BigEndian,
            _ => return Err(Error::InvalidHeader),
        };
        let magic = byte_order.u16([m0, m1]);
        match magic {
            Self::TIFF | Self::ORF | Self::ORF_RS | Self::RW2 => {}
            43 => return Err(Error::Unsupported { what: "BigTIFF" }),
            _ => return Err(Error::InvalidHeader),
        }
        let ifd0_offset = byte_order.u32([o0, o1, o2, o3]);
        if (ifd0_offset as usize) < Self::LEN {
            return Err(Error::InvalidHeader);
        }
        Ok(Self {
            byte_order,
            ifd0_offset,
            magic,
        })
    }

    /// Encodes the header.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 8] {
        let (mark, magic, offset) = match self.byte_order {
            ByteOrder::LittleEndian => (
                *b"II",
                self.magic.to_le_bytes(),
                self.ifd0_offset.to_le_bytes(),
            ),
            ByteOrder::BigEndian => (
                *b"MM",
                self.magic.to_be_bytes(),
                self.ifd0_offset.to_be_bytes(),
            ),
        };
        [
            mark[0], mark[1], magic[0], magic[1], offset[0], offset[1], offset[2], offset[3],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips() {
        for byte_order in [ByteOrder::LittleEndian, ByteOrder::BigEndian] {
            for magic in [Header::TIFF, Header::ORF, Header::ORF_RS, Header::RW2] {
                let header = Header {
                    magic,
                    ..Header::new(byte_order, 8)
                };
                assert_eq!(Header::parse(&header.to_bytes()).ok(), Some(header));
            }
        }
    }

    #[test]
    fn header_rejects_bad_input() {
        assert!(matches!(Header::parse(b"II*\0"), Err(Error::InvalidHeader)));
        // Raw formats spell their magic as letters.
        assert_eq!(Header::parse(b"IIRO\x08\0\0\0").unwrap().magic, Header::ORF);
        assert_eq!(Header::parse(b"MMOR\0\0\0\x08").unwrap().magic, Header::ORF);
        assert_eq!(
            Header::parse(b"IIRS\x08\0\0\0").unwrap().magic,
            Header::ORF_RS
        );
        assert_eq!(
            Header::parse(b"IIU\0\x18\0\0\0").unwrap().magic,
            Header::RW2
        );
        assert!(matches!(
            Header::parse(b"XX*\0\x08\0\0\0"),
            Err(Error::InvalidHeader)
        ));
        assert!(matches!(
            Header::parse(b"II*\0\x04\0\0\0"),
            Err(Error::InvalidHeader)
        ));
        assert!(matches!(
            Header::parse(b"II+\0\x08\0\0\0"),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn type_ids_round_trip() {
        for id in 0..=u16::MAX {
            if let Some(ty) = Type::from_u16(id) {
                assert_eq!(ty.to_u16(), id);
            }
        }
    }
}
