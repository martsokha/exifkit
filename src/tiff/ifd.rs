//! Zero-copy parsing of one IFD.

use core::fmt;

use super::{Reader, Type};

/// Size of one IFD entry, in bytes.
const ENTRY_LEN: u32 = 12;

/// One IFD, parsed lazily from a [`Reader`].
#[derive(Debug, Clone, Copy)]
pub struct RawIfd<'a> {
    reader: Reader<'a>,
    offset: u32,
    declared: u16,
    available: u16,
}

impl<'a> RawIfd<'a> {
    /// Parses the IFD at `offset`. Returns `None` if even the entry count is
    /// out of bounds. Entries that run past the end of the data are dropped;
    /// see [`RawIfd::is_truncated`].
    #[must_use]
    pub fn parse(reader: Reader<'a>, offset: u32) -> Option<Self> {
        let declared = reader.u16(offset)?;
        let room = u64::try_from(reader.len())
            .unwrap_or(u64::MAX)
            .saturating_sub(u64::from(offset))
            .saturating_sub(2)
            / u64::from(ENTRY_LEN);
        let available = u16::try_from(room.min(u64::from(declared))).unwrap_or(declared);
        Some(Self {
            reader,
            offset,
            declared,
            available,
        })
    }

    /// Returns the TIFF-relative offset of this IFD.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Returns the entry count stored in the IFD.
    #[must_use]
    pub const fn declared_len(&self) -> u16 {
        self.declared
    }

    /// Returns how many entries are actually within bounds.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.available
    }

    /// Returns whether the IFD has no readable entries.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.available == 0
    }

    /// Returns whether some declared entries run past the end of the data.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.available < self.declared
    }

    /// Returns the entry at `index`, if it is within bounds.
    #[must_use]
    pub fn entry(&self, index: u16) -> Option<RawEntry<'a>> {
        if index >= self.available {
            return None;
        }
        let at = self
            .offset
            .checked_add(2)?
            .checked_add(u32::from(index).checked_mul(ENTRY_LEN)?)?;
        Some(RawEntry {
            reader: self.reader,
            offset: at,
            tag: self.reader.u16(at)?,
            type_id: self.reader.u16(at.checked_add(2)?)?,
            count: self.reader.u32(at.checked_add(4)?)?,
            field: self.reader.array(at.checked_add(8)?)?,
        })
    }

    /// Iterates over the entries that are within bounds.
    pub fn entries(&self) -> impl Iterator<Item = RawEntry<'a>> + use<'a> {
        let ifd = *self;
        (0..self.available).filter_map(move |i| ifd.entry(i))
    }

    /// Returns the offset of the next IFD in the chain, or `None` if it is
    /// zero, missing or the IFD is truncated.
    #[must_use]
    pub fn next(&self) -> Option<u32> {
        if self.is_truncated() {
            return None;
        }
        let at = self
            .offset
            .checked_add(2)?
            .checked_add(u32::from(self.declared).checked_mul(ENTRY_LEN)?)?;
        self.reader.u32(at).filter(|&next| next != 0)
    }
}

/// One 12-byte IFD entry.
#[derive(Debug, Clone, Copy)]
pub struct RawEntry<'a> {
    reader: Reader<'a>,
    offset: u32,
    /// Tag id.
    pub tag: u16,
    /// Field-type id.
    pub type_id: u16,
    /// Number of values.
    pub count: u32,
    /// The 4-byte value-or-offset field.
    pub field: [u8; 4],
}

/// Why an entry's data could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DataError {
    /// The field type is unknown, so the data size cannot be computed.
    UnknownType(u16),
    /// `count × size` overflows.
    TooLarge,
    /// The data lies outside the TIFF block.
    OutOfBounds {
        /// TIFF-relative offset the entry points to.
        offset: u32,
    },
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownType(id) => write!(f, "unknown field type {id}"),
            Self::TooLarge => f.write_str("value size overflows"),
            Self::OutOfBounds { offset } => write!(f, "value at offset {offset} is out of bounds"),
        }
    }
}

impl<'a> RawEntry<'a> {
    /// Returns the TIFF-relative offset of this entry.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Returns the field type, or `None` if it is unknown.
    #[must_use]
    pub const fn ty(&self) -> Option<Type> {
        Type::from_u16(self.type_id)
    }

    /// Returns the size of the value data, in bytes.
    ///
    /// # Errors
    ///
    /// Fails for an unknown type or when the size overflows `u32`.
    pub fn byte_len(&self) -> Result<u32, DataError> {
        let ty = self.ty().ok_or(DataError::UnknownType(self.type_id))?;
        u32::from(ty.size())
            .checked_mul(self.count)
            .ok_or(DataError::TooLarge)
    }

    /// Returns where the value data lives: inside the entry when it fits in
    /// 4 bytes, otherwise at the offset stored in the field.
    ///
    /// # Errors
    ///
    /// Fails like [`RawEntry::byte_len`].
    pub fn data_offset(&self) -> Result<u32, DataError> {
        if self.byte_len()? <= 4 {
            self.offset.checked_add(8).ok_or(DataError::TooLarge)
        } else {
            Ok(self.reader.byte_order().u32(self.field))
        }
    }

    /// Returns the value data, borrowed from the TIFF block.
    ///
    /// # Errors
    ///
    /// Fails like [`RawEntry::byte_len`], or with
    /// [`DataError::OutOfBounds`] when the data is not inside the block.
    pub fn data(&self) -> Result<&'a [u8], DataError> {
        let len = self.byte_len()?;
        let offset = self.data_offset()?;
        self.reader
            .bytes(offset, len)
            .ok_or(DataError::OutOfBounds { offset })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiff::ByteOrder;

    /// Header + IFD at 8 with one SHORT entry (Orientation = 6) and one
    /// out-of-line ASCII entry, then next = 0.
    const TIFF: &[u8] = &[
        b'I', b'I', 42, 0, 8, 0, 0, 0, //
        2, 0, //
        0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, //
        0x0F, 0x01, 2, 0, 6, 0, 0, 0, 38, 0, 0, 0, //
        0, 0, 0, 0, //
        b'C', b'a', b'n', b'o', b'n', 0,
    ];

    #[test]
    fn parses_entries_and_data() {
        let reader = Reader::new(TIFF, ByteOrder::LittleEndian);
        let ifd = RawIfd::parse(reader, 8).unwrap();
        assert_eq!(ifd.len(), 2);
        assert!(!ifd.is_truncated());
        assert_eq!(ifd.next(), None);
        let entries: alloc::vec::Vec<_> = ifd.entries().collect();
        assert_eq!(entries[0].tag, 0x0112);
        assert_eq!(entries[0].data(), Ok(&[6, 0][..]));
        assert_eq!(entries[1].data(), Ok(&b"Canon\0"[..]));
    }

    #[test]
    fn truncated_ifd_keeps_what_fits() {
        let short = &TIFF[..30];
        let ifd = RawIfd::parse(Reader::new(short, ByteOrder::LittleEndian), 8).unwrap();
        assert_eq!(ifd.declared_len(), 2);
        assert_eq!(ifd.len(), 1);
        assert!(ifd.is_truncated());
        assert_eq!(ifd.next(), None);
    }

    #[test]
    fn huge_counts_do_not_allocate_or_panic() {
        let mut data = TIFF.to_vec();
        data[14..18].copy_from_slice(&u32::MAX.to_le_bytes()); // count of entry 0
        let ifd = RawIfd::parse(Reader::new(&data, ByteOrder::LittleEndian), 8).unwrap();
        let entry = ifd.entry(0).unwrap();
        assert_eq!(entry.byte_len(), Err(DataError::TooLarge));
        assert!(entry.data().is_err());
    }
}
