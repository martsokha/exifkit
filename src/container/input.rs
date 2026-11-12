//! Random-access input shared by the container scanners.

use crate::error::Result;

/// Random-access reads, implemented for byte slices and (with `std`) for
/// `Read + Seek` streams.
pub(crate) trait Input {
    /// Fills `buf` with the bytes at `offset`. Returns `Ok(false)` if the
    /// input ends first.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<bool>;

    /// The total length, if known.
    fn len(&mut self) -> Result<Option<u64>>;

    /// Reads `N` bytes at `offset`, or `None` past the end.
    #[cfg_attr(
        not(any(feature = "jpeg", feature = "png", feature = "webp")),
        allow(dead_code)
    )]
    fn array<const N: usize>(&mut self, offset: u64) -> Result<Option<[u8; N]>>
    where
        Self: Sized,
    {
        let mut buf = [0u8; N];
        Ok(self.read_at(offset, &mut buf)?.then_some(buf))
    }
}

/// A byte slice as [`Input`].
pub(crate) struct Slice<'a>(pub(crate) &'a [u8]);

impl Input for Slice<'_> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<bool> {
        let source = usize::try_from(offset)
            .ok()
            .and_then(|start| self.0.get(start..start.checked_add(buf.len())?));
        Ok(source.is_some_and(|source| {
            buf.copy_from_slice(source);
            true
        }))
    }

    fn len(&mut self) -> Result<Option<u64>> {
        Ok(u64::try_from(self.0.len()).ok())
    }
}

/// A `Read + Seek` stream as [`Input`].
#[cfg(feature = "std")]
pub(crate) struct Stream<R>(pub(crate) R);

#[cfg(feature = "std")]
impl<R: std::io::Read + std::io::Seek> Input for Stream<R> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<bool> {
        use std::io::{ErrorKind, SeekFrom};
        self.0.seek(SeekFrom::Start(offset))?;
        match self.0.read_exact(buf) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == ErrorKind::UnexpectedEof => Ok(false),
            Err(err) => Err(err.into()),
        }
    }

    fn len(&mut self) -> Result<Option<u64>> {
        Ok(Some(self.0.seek(std::io::SeekFrom::End(0))?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_reads_are_bounds_checked() {
        let mut input = Slice(&[1, 2, 3]);
        assert_eq!(input.array::<2>(1).unwrap(), Some([2, 3]));
        assert_eq!(input.array::<2>(2).unwrap(), None);
        assert_eq!(input.array::<1>(u64::MAX).unwrap(), None);
        assert_eq!(input.len().unwrap(), Some(3));
    }
}
