//! IFD identity. See `docs/format.md` §2.

/// Which IFD in the Exif graph an entry lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum IfdKind {
    /// IFD0: the primary image.
    Primary,
    /// IFD1: the thumbnail.
    Thumbnail,
    /// IFD2 and later in the IFD0 chain (multi-page TIFF). Holds the chain index.
    Chain(u16),
    /// The Exif IFD, pointed to by `ExifIFDPointer` (0x8769).
    Exif,
    /// The GPS IFD, pointed to by `GPSInfoIFDPointer` (0x8825).
    Gps,
    /// The Interoperability IFD, pointed to by `InteroperabilityIFDPointer` (0xA005).
    Interop,
    /// A SubIFD, pointed to by `SubIFDs` (0x014A). Holds its index.
    SubIfd(u16),
}
