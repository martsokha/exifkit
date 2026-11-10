//! Bounds-checked reads from a TIFF block.

use super::ByteOrder;

/// Bounds-checked, zero-copy access to a TIFF block.
///
/// Every read takes a TIFF-relative offset and returns `None` instead of
/// panicking when it would run past the end of the data.
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    data: &'a [u8],
    order: ByteOrder,
}

impl<'a> Reader<'a> {
    /// Creates a reader over a whole TIFF block (header included).
    #[must_use]
    pub const fn new(data: &'a [u8], order: ByteOrder) -> Self {
        Self { data, order }
    }

    /// Returns the byte order.
    #[must_use]
    pub const fn byte_order(&self) -> ByteOrder {
        self.order
    }

    /// Returns the length of the block, in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns whether the block is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns `len` bytes at `offset`, or `None` if they are out of bounds.
    #[must_use]
    pub fn bytes(&self, offset: u32, len: u32) -> Option<&'a [u8]> {
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(usize::try_from(len).ok()?)?;
        self.data.get(start..end)
    }

    /// Returns `N` bytes at `offset` as an array.
    #[must_use]
    pub fn array<const N: usize>(&self, offset: u32) -> Option<[u8; N]> {
        let len = u32::try_from(N).ok()?;
        self.bytes(offset, len)?.try_into().ok()
    }

    /// Reads a `u16` at `offset`.
    #[must_use]
    pub fn u16(&self, offset: u32) -> Option<u16> {
        self.array(offset).map(|b| self.order.u16(b))
    }

    /// Reads a `u32` at `offset`.
    #[must_use]
    pub fn u32(&self, offset: u32) -> Option<u32> {
        self.array(offset).map(|b| self.order.u32(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_bounds_checked() {
        let r = Reader::new(&[1, 0, 2, 0], ByteOrder::LittleEndian);
        assert_eq!(r.u16(0), Some(1));
        assert_eq!(r.u16(2), Some(2));
        assert_eq!(r.u16(3), None);
        assert_eq!(r.u32(1), None);
        assert_eq!(r.bytes(u32::MAX, u32::MAX), None);
        assert_eq!(r.bytes(4, 0), Some(&[][..]));
    }
}
