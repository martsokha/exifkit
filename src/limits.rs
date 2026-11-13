//! Resource caps applied while parsing. See `docs/architecture.md#safety-model`.

use core::fmt;

/// Resource caps applied while parsing untrusted input.
///
/// The defaults are generous enough for any real camera file and keep memory
/// use linear in the input size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum nesting depth of IFD pointers (IFD0 → Exif → Interop is 3).
    pub max_depth: u8,
    /// Maximum number of IFDs visited in total.
    pub max_ifds: u16,
    /// Maximum number of entries in a single IFD.
    pub max_entries: u16,
    /// Maximum number of SubIFDs one IFD references, across all of its
    /// SubIFDs tags (a duplicate tag adds to the same IFD's list).
    pub max_sub_ifds: u16,
    /// Maximum number of bytes allocated while building the model, or `None`
    /// to derive it from the input (4 × input length + 64 KiB).
    pub max_alloc: Option<usize>,
    /// Maximum size of a decoded metadata block: decompressed Exif or XMP
    /// (JPEG XL `brob`, legacy PNG text) and inflated PNG chunks, the file's
    /// own Exif block read as the writer's source, and any single value or
    /// thumbnail in the TIFF structure. Fixed rather than scaled with the
    /// file, so neither a small compressed box nor one bogus entry in a
    /// large image can make memory use grow with the image. Default 64 MiB.
    pub max_metadata: usize,
}

impl Limits {
    /// The default limits.
    pub const DEFAULT: Self = Self {
        max_depth: 8,
        max_ifds: 256,
        max_entries: 4096,
        max_sub_ifds: 64,
        max_alloc: None,
        max_metadata: 64 * 1024 * 1024,
    };

    /// The cap for decoding a metadata block of a file of `input_len` bytes:
    /// the allocation budget, but never more than [`Limits::max_metadata`].
    #[must_use]
    pub const fn metadata_budget(&self, input_len: usize) -> usize {
        let budget = self.alloc_budget(input_len);
        if budget < self.max_metadata {
            budget
        } else {
            self.max_metadata
        }
    }

    /// Returns the allocation budget for an input of `input_len` bytes.
    #[must_use]
    pub const fn alloc_budget(&self, input_len: usize) -> usize {
        match self.max_alloc {
            Some(max) => max,
            None => input_len.saturating_mul(4).saturating_add(64 * 1024),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Identifies which [`Limits`] field was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Limit {
    /// [`Limits::max_depth`].
    Depth,
    /// [`Limits::max_ifds`].
    Ifds,
    /// [`Limits::max_entries`].
    Entries,
    /// [`Limits::max_sub_ifds`].
    SubIfds,
    /// [`Limits::max_alloc`].
    Alloc,
    /// [`Limits::max_metadata`].
    Metadata,
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Depth => "ifd nesting depth",
            Self::Ifds => "number of ifds",
            Self::Entries => "entries per ifd",
            Self::SubIfds => "sub-ifds per ifd",
            Self::Alloc => "allocation budget",
            Self::Metadata => "metadata block size",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_budget_takes_the_smaller_cap() {
        let limits = Limits::DEFAULT;
        assert_eq!(limits.metadata_budget(0), 64 * 1024);
        assert_eq!(limits.metadata_budget(usize::MAX), 64 * 1024 * 1024);
        let tight = Limits {
            max_metadata: 1000,
            ..Limits::DEFAULT
        };
        assert_eq!(tight.metadata_budget(1 << 20), 1000);
    }

    #[test]
    fn alloc_budget_saturates() {
        assert_eq!(Limits::DEFAULT.alloc_budget(usize::MAX), usize::MAX);
        assert_eq!(Limits::DEFAULT.alloc_budget(0), 64 * 1024);
    }
}
