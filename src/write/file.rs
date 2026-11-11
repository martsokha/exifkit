//! Writing Exif back into image files. See `docs/containers.md`.

use alloc::vec::Vec;

use super::WriteOptions;
use crate::container::{self, Encoding, Format};
use crate::error::{Error, Result};
use crate::model::Exif;

impl Exif {
    /// Writes the metadata into `original`, a JPEG, TIFF, PNG or WebP file,
    /// and returns the new file. Only the Exif segment or chunk changes; the
    /// image data and every other segment are copied byte for byte.
    ///
    /// The file's own Exif block is used as the
    /// [`source`](WriteOptions::source), so the new block is a minimal diff
    /// of the old one: unchanged values, the MakerNote and data the model
    /// does not understand keep their offsets.
    ///
    /// # Errors
    ///
    /// - [`Error::UnknownFormat`], or [`Error::Unsupported`] for HEIF and
    ///   JPEG XL (not writable yet) and formats whose feature is disabled.
    /// - [`Error::TooLarge`] if the block does not fit, notably over 65,527
    ///   bytes in a JPEG APP1 segment.
    /// - [`Error::Malformed`] if the file is too broken to rewrite safely
    ///   (for example a truncated segment).
    /// - Any error of [`Exif::to_tiff_with`].
    pub fn write_into(&self, original: &[u8]) -> Result<Vec<u8>> {
        self.write_into_with(original, &WriteOptions::DEFAULT)
    }

    /// Writes the metadata into `original` with the given options. If
    /// `options.source` is unset, the file's own Exif block is used.
    ///
    /// # Errors
    ///
    /// As [`Exif::write_into`].
    pub fn write_into_with(&self, original: &[u8], options: &WriteOptions<'_>) -> Result<Vec<u8>> {
        let (located, _) = container::locate(original)?;
        let mut options = *options;
        if options.source.is_none() {
            options.source = own_block(original, &located);
        }
        let tiff = self.to_tiff_with(&options)?;
        match located.format {
            // A TIFF file is its TIFF block; image data stayed in place.
            Format::Tiff => Ok(tiff),
            format => container::splice(original, format, Some(&tiff)),
        }
    }
}

/// The file's own TIFF block, as the reader saw it, when it is stored raw.
fn own_block<'a>(data: &'a [u8], located: &container::Located) -> Option<&'a [u8]> {
    if located.encoding != Encoding::Raw {
        return None;
    }
    let range = located.exif.clone()?;
    let start = usize::try_from(range.start).ok()?;
    let end = usize::try_from(range.end).ok()?.min(data.len());
    let block = data.get(start..end)?;
    // **quirk:** some PNG and WebP writers keep JPEG's `Exif\0\0` header.
    Some(block.strip_prefix(b"Exif\0\0").unwrap_or(block))
}

/// Removes the Exif data from a JPEG, PNG or WebP file: the Exif APP1
/// segments, the `eXIf` chunk (and legacy raw-profile text chunks), or the
/// `EXIF` chunk (clearing the `VP8X` flag). Everything else is copied byte
/// for byte. A file without Exif is returned unchanged.
///
/// # Errors
///
/// [`Error::UnknownFormat`], [`Error::Unsupported`] for TIFF (its metadata is
/// the file's structure), HEIF and JPEG XL, or [`Error::Malformed`] if the
/// file is too broken to rewrite safely.
pub fn remove_exif(original: &[u8]) -> Result<Vec<u8>> {
    let (located, _) = container::locate(original)?;
    match located.format {
        Format::Heif | Format::Jxl => Err(Error::Unsupported {
            what: "HEIF and JPEG XL are not writable yet",
        }),
        format => container::splice(original, format, None),
    }
}
