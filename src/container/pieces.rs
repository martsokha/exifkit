//! A rewritten file as a list of pieces: byte ranges copied from the
//! original and new bytes. The container writers build one by walking the
//! structure through [`Input`], reading only headers and the small chunks
//! they must inspect, so the same plan serves an in-memory file and a
//! stream, and image data is never held in memory when streaming.

use alloc::vec::Vec;
use core::ops::Range;

use super::Input;
use crate::error::{Error, Result};

/// One piece of the output.
pub(crate) enum Piece {
    /// Bytes copied from the original.
    Copy(Range<u64>),
    /// Zero bytes.
    Zeros(u64),
    /// New bytes.
    Bytes(Vec<u8>),
}

/// Pieces and their total length.
#[derive(Default)]
pub(crate) struct Pieces {
    list: Vec<Piece>,
    len: u64,
}

impl Pieces {
    pub(crate) fn bytes(&mut self, bytes: impl Into<Vec<u8>>) {
        let bytes = bytes.into();
        self.len = self
            .len
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if !bytes.is_empty() {
            self.list.push(Piece::Bytes(bytes));
        }
    }

    pub(crate) fn copy(&mut self, range: Range<u64>) {
        self.len = self
            .len
            .saturating_add(range.end.saturating_sub(range.start));
        if range.start < range.end {
            // Merge with a directly preceding copy: fewer, larger reads.
            if let Some(Piece::Copy(last)) = self.list.last_mut()
                && last.end == range.start
            {
                last.end = range.end;
                return;
            }
            self.list.push(Piece::Copy(range));
        }
    }

    pub(crate) fn zeros(&mut self, len: u64) {
        if len > 0 {
            self.len = self.len.saturating_add(len);
            self.list.push(Piece::Zeros(len));
        }
    }

    /// Appends another list.
    pub(crate) fn extend(&mut self, other: Self) {
        for piece in other.list {
            match piece {
                Piece::Copy(range) => self.copy(range),
                Piece::Bytes(bytes) => self.bytes(bytes),
                Piece::Zeros(len) => self.zeros(len),
            }
        }
    }

    /// The total output length.
    pub(crate) const fn len(&self) -> u64 {
        self.len
    }

    /// Writes the pieces to `sink`, reading copied ranges from `input` in
    /// blocks of at most 64 KiB.
    pub(crate) fn emit(
        &self,
        input: &mut impl Input,
        sink: &mut impl FnMut(&[u8]) -> Result<()>,
    ) -> Result<()> {
        let mut buf = alloc::vec![0u8; 64 * 1024];
        for piece in &self.list {
            match piece {
                Piece::Bytes(bytes) => sink(bytes)?,
                Piece::Zeros(len) => {
                    let zero = [0u8; 4096];
                    let mut left = *len;
                    while left > 0 {
                        let n = usize::try_from(left.min(zero.len() as u64)).unwrap_or(zero.len());
                        sink(zero.get(..n).unwrap_or_default())?;
                        left = left.saturating_sub(n as u64);
                    }
                }
                Piece::Copy(range) => {
                    let mut at = range.start;
                    while at < range.end {
                        let n = usize::try_from(range.end.saturating_sub(at))
                            .unwrap_or(usize::MAX)
                            .min(buf.len());
                        let chunk = buf.get_mut(..n).unwrap_or_default();
                        if !input.read_at(at, chunk)? {
                            return Err(Error::Malformed {
                                offset: at,
                                reason: "the file changed or ended while it was copied",
                            });
                        }
                        sink(chunk)?;
                        at = at.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
                    }
                }
            }
        }
        Ok(())
    }

    /// Assembles the pieces in memory.
    pub(crate) fn to_vec(&self, input: &mut impl Input) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(usize::try_from(self.len).unwrap_or(0));
        self.emit(input, &mut |bytes| {
            out.extend_from_slice(bytes);
            Ok(())
        })?;
        Ok(out)
    }
}

/// Reads `range` into memory, failing if the input ends first. The buffer
/// grows in 64 KiB steps as bytes arrive, so a range claiming more than the
/// input holds fails before allocating it.
pub(crate) fn read_range(input: &mut impl Input, range: Range<u64>) -> Result<Vec<u8>> {
    const STEP: u64 = 64 * 1024;
    let len = range.end.saturating_sub(range.start);
    if usize::try_from(len).is_err() {
        return Err(Error::TooLarge {
            size: usize::MAX,
            max: usize::MAX,
        });
    }
    let mut out = Vec::new();
    let mut at = range.start;
    while at < range.end {
        let n = range.end.saturating_sub(at).min(STEP);
        let start = out.len();
        out.resize(start.saturating_add(usize::try_from(n).unwrap_or(0)), 0);
        if !input.read_at(at, out.get_mut(start..).unwrap_or_default())? {
            return Err(Error::Malformed {
                offset: at,
                reason: "truncated chunk",
            });
        }
        at = at.saturating_add(n);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::Slice;

    #[test]
    fn read_range_reads_in_steps_and_refuses_past_the_end() {
        // A length that cannot fit in memory is refused before any read.
        #[cfg(target_pointer_width = "32")]
        assert!(matches!(
            read_range(&mut Slice(&[]), 0..u64::MAX),
            Err(Error::TooLarge { .. })
        ));
        let data: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        assert_eq!(
            read_range(&mut Slice(&data), 10..150_000).unwrap(),
            &data[10..150_000]
        );
        assert_eq!(
            read_range(&mut Slice(&data), 5..5).unwrap(),
            Vec::<u8>::new()
        );
        // Claiming far more than the input holds fails without allocating it.
        assert!(matches!(
            read_range(&mut Slice(&data), 0..u64::MAX),
            Err(Error::Malformed { .. })
        ));
    }
}
