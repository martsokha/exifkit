//! Bounds-checked reads from a TIFF block, held in memory or read on demand
//! from a stream.

use alloc::borrow::Cow;
#[cfg(feature = "std")]
use alloc::collections::BTreeMap;
#[cfg(feature = "std")]
use alloc::vec::Vec;
#[cfg(feature = "std")]
use core::cell::RefCell;
use core::fmt;

use super::ByteOrder;
#[cfg(feature = "std")]
use crate::container::Input;
#[cfg(feature = "std")]
use crate::error::{Error, Result};

/// Bounds-checked access to a TIFF block.
///
/// Every read takes a TIFF-relative offset and returns `None` instead of
/// panicking when it would run past the end of the data. Reads from a block
/// in memory borrow from it (no copy); reads from a stream return owned
/// bytes.
#[derive(Clone, Copy)]
pub struct Reader<'a> {
    data: Data<'a>,
    order: ByteOrder,
}

#[derive(Clone, Copy)]
enum Data<'a> {
    Slice(&'a [u8]),
    /// Streams are only read through the `std` APIs.
    #[cfg(feature = "std")]
    Paged(&'a Paged<'a>),
}

impl fmt::Debug for Reader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reader")
            .field("len", &self.len())
            .field("order", &self.order)
            .finish_non_exhaustive()
    }
}

impl<'a> Reader<'a> {
    /// Creates a reader over a whole TIFF block (header included).
    #[must_use]
    pub const fn new(data: &'a [u8], order: ByteOrder) -> Self {
        Self {
            data: Data::Slice(data),
            order,
        }
    }

    /// Creates a reader over a TIFF block read on demand from a stream.
    #[cfg(feature = "std")]
    pub(crate) const fn paged(source: &'a Paged<'a>, order: ByteOrder) -> Self {
        Self {
            data: Data::Paged(source),
            order,
        }
    }

    /// Returns the byte order.
    #[must_use]
    pub const fn byte_order(&self) -> ByteOrder {
        self.order
    }

    /// Returns the length of the block, in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        match self.data {
            Data::Slice(data) => data.len(),
            #[cfg(feature = "std")]
            Data::Paged(paged) => usize::try_from(paged.len).unwrap_or(usize::MAX),
        }
    }

    /// Returns whether the block is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns `len` bytes at `offset`, or `None` if they are out of bounds
    /// (or, for a stream, cannot be read: the caller then gets the I/O error).
    #[must_use]
    pub fn bytes(&self, offset: u32, len: u32) -> Option<Cow<'a, [u8]>> {
        match self.data {
            Data::Slice(data) => {
                let start = usize::try_from(offset).ok()?;
                let end = start.checked_add(usize::try_from(len).ok()?)?;
                data.get(start..end).map(Cow::Borrowed)
            }
            #[cfg(feature = "std")]
            Data::Paged(paged) => paged.get(u64::from(offset), u64::from(len)).map(Cow::Owned),
        }
    }

    /// Returns `N` bytes at `offset` as an array.
    #[must_use]
    pub fn array<const N: usize>(&self, offset: u32) -> Option<[u8; N]> {
        let len = u32::try_from(N).ok()?;
        (*self.bytes(offset, len)?).try_into().ok()
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

/// Size of a cached page.
#[cfg(feature = "std")]
const PAGE: u64 = 4096;
#[cfg(feature = "std")]
/// Most pages cached at once (1 MiB). The cache is dropped when full: IFD
/// walks read in clusters, so a simple reset is enough.
const MAX_PAGES: usize = 256;

/// A TIFF block read on demand from a stream, through a small page cache.
///
/// Only the bytes the parser or writer asks for are read: IFDs, values and
/// the thumbnail, never the image data in between. Reads larger than a page
/// bypass the cache. The first I/O error is kept for [`Paged::take_error`],
/// and the failed read reports out of bounds, so parsing stops cleanly.
#[cfg(feature = "std")]
pub(crate) struct Paged<'a> {
    input: RefCell<&'a mut dyn Input>,
    len: u64,
    pages: RefCell<BTreeMap<u64, Vec<u8>>>,
    error: RefCell<Option<Error>>,
}

#[cfg(feature = "std")]
impl<'a> Paged<'a> {
    /// Wraps the first `len` bytes of `input`.
    pub(crate) fn new(input: &'a mut dyn Input, len: u64) -> Self {
        Self {
            input: RefCell::new(input),
            len,
            pages: RefCell::new(BTreeMap::new()),
            error: RefCell::new(None),
        }
    }

    /// The length of the block.
    pub(crate) const fn len(&self) -> u64 {
        self.len
    }

    /// Returns the first I/O error a read hit, if any.
    pub(crate) fn take_error(&self) -> Option<Error> {
        self.error.borrow_mut().take()
    }

    /// Reads `len` bytes at `offset`, or `None` if out of bounds or failed.
    pub(crate) fn get(&self, offset: u64, len: u64) -> Option<Vec<u8>> {
        let end = offset.checked_add(len)?;
        if end > self.len {
            return None;
        }
        let size = usize::try_from(len).ok()?;
        if len > PAGE {
            let mut out = alloc::vec![0u8; size];
            return self.read(offset, &mut out).then_some(out);
        }
        let mut out = Vec::with_capacity(size);
        let mut at = offset;
        while at < end {
            let page = at / PAGE;
            let start = usize::try_from(at - page * PAGE).ok()?;
            let take = usize::try_from(end.min((page + 1) * PAGE) - at).ok()?;
            let mut pages = self.pages.borrow_mut();
            if !pages.contains_key(&page) {
                if pages.len() >= MAX_PAGES {
                    pages.clear();
                }
                let page_start = page * PAGE;
                let page_len =
                    usize::try_from(self.len.min(page_start + PAGE) - page_start).ok()?;
                let mut bytes = alloc::vec![0u8; page_len];
                if !self.read(page_start, &mut bytes) {
                    return None;
                }
                pages.insert(page, bytes);
            }
            out.extend_from_slice(pages.get(&page)?.get(start..start.checked_add(take)?)?);
            at = at.checked_add(u64::try_from(take).ok()?)?;
        }
        Some(out)
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> bool {
        match self.input.borrow_mut().read_at(offset, buf) {
            Ok(true) => true,
            Ok(false) => {
                self.keep(Error::Malformed {
                    offset,
                    reason: "the file ended early while it was read",
                });
                false
            }
            Err(err) => {
                self.keep(err);
                false
            }
        }
    }

    fn keep(&self, err: Error) {
        let mut error = self.error.borrow_mut();
        if error.is_none() {
            *error = Some(err);
        }
    }

    /// Reads the TIFF header at the start of the block.
    ///
    /// # Errors
    ///
    /// The I/O error, if reading failed; [`Error::InvalidHeader`] if the
    /// block is shorter than a header.
    pub(crate) fn header(&self) -> Result<[u8; 8]> {
        match self.get(0, 8) {
            Some(bytes) => bytes.try_into().map_err(|_| Error::InvalidHeader),
            None => Err(self.take_error().unwrap_or(Error::InvalidHeader)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "std")]
    use crate::container::Slice;

    #[test]
    fn reads_are_bounds_checked() {
        let r = Reader::new(&[1, 0, 2, 0], ByteOrder::LittleEndian);
        assert_eq!(r.u16(0), Some(1));
        assert_eq!(r.u16(2), Some(2));
        assert_eq!(r.u16(3), None);
        assert_eq!(r.u32(1), None);
        assert_eq!(r.bytes(u32::MAX, u32::MAX), None);
        assert_eq!(r.bytes(4, 0).as_deref(), Some(&[][..]));
    }

    #[cfg(feature = "std")]
    /// An input whose reads fail.
    struct Broken;

    #[cfg(feature = "std")]
    impl Input for Broken {
        fn read_at(&mut self, _: u64, _: &mut [u8]) -> Result<bool> {
            Err(Error::Unsupported {
                what: "broken input",
            })
        }

        fn len(&mut self) -> Result<Option<u64>> {
            Ok(Some(100))
        }
    }

    #[test]
    #[cfg(feature = "std")]
    fn a_failed_header_read_reports_the_failure() {
        // Found in review: not `InvalidHeader`, which would hide it.
        let mut input = Broken;
        let paged = Paged::new(&mut input, 100);
        assert!(matches!(paged.header(), Err(Error::Unsupported { .. })));
        // A block shorter than a header is still `InvalidHeader`.
        let mut short = Slice(&[b'I', b'I', 42, 0]);
        let paged = Paged::new(&mut short, 4);
        assert!(matches!(paged.header(), Err(Error::InvalidHeader)));
    }

    #[test]
    #[cfg(feature = "std")]
    fn paged_reads_match_slice_reads() {
        let data: Vec<u8> = (0..20_000u32).map(|i| (i * 7) as u8).collect();
        let mut input = Slice(&data);
        let paged = Paged::new(&mut input, data.len() as u64);
        let a = Reader::new(&data, ByteOrder::BigEndian);
        let b = Reader::paged(&paged, ByteOrder::BigEndian);
        for (offset, len) in [
            (0, 8),
            (4090, 12),
            (4095, 2),
            (8000, 9000),
            (19_990, 10),
            (19_995, 10),
            (0, 0),
        ] {
            assert_eq!(a.bytes(offset, len), b.bytes(offset, len), "{offset}+{len}");
        }
        assert_eq!(a.u32(4094), b.u32(4094));
        assert_eq!(b.len(), data.len());
        assert!(paged.take_error().is_none());
    }
}
