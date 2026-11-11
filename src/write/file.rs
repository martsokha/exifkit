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
            // A TIFF file is its TIFF block; image data stayed in place. A raw
            // format (ORF, RW2) keeps its own magic number.
            Format::Tiff => Ok(keep_magic(original, tiff)),
            format => container::splice(original, format, Some(&tiff)),
        }
    }

    /// Writes the metadata into the file read from `src`, writing the new
    /// file to `dst`. The result is the same as [`Exif::write_into_with`],
    /// but JPEG, PNG and WebP files are streamed: only their headers, small
    /// metadata chunks and the Exif block are read into memory, and the
    /// image data is copied through in 64 KiB blocks. A TIFF file is its
    /// own Exif block, so it is read whole.
    ///
    /// `dst` must not be the file `src` reads from. Write to a temporary
    /// file and rename it over the original, so a failure leaves the
    /// original intact:
    ///
    /// ```no_run
    /// # fn main() -> exifkit::Result<()> {
    /// use std::fs::File;
    /// use std::io::BufWriter;
    ///
    /// let (exif, _) = exifkit::Exif::read_from(File::open("photo.jpg")?)?;
    /// let tmp = "photo.jpg.tmp";
    /// let mut dst = BufWriter::new(File::create(tmp)?);
    /// exif.write_stream(File::open("photo.jpg")?, &mut dst, &exifkit::WriteOptions::DEFAULT)?;
    /// dst.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    /// std::fs::rename(tmp, "photo.jpg")?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Exif::write_into`], plus [`Error::Io`] from either stream. On
    /// an error, `dst` may hold a partial file.
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    pub fn write_stream<R, W>(&self, src: R, mut dst: W, options: &WriteOptions<'_>) -> Result<()>
    where
        R: std::io::Read + std::io::Seek,
        W: std::io::Write,
    {
        let mut input = container::Stream(src);
        let mut diagnostics = crate::diagnostic::Diagnostics::default();
        let located = container::locate_in(&mut input, &mut diagnostics)?;
        // The file's own block, read from the stream, is the source.
        let own = match (&located.exif, located.encoding) {
            (Some(range), Encoding::Raw) if options.source.is_none() => {
                Some(container::read_range(&mut input, range.clone())?)
            }
            _ => None,
        };
        let mut options = *options;
        if let Some(own) = &own {
            options.source = Some(own.strip_prefix(b"Exif\0\0").unwrap_or(own));
        }
        let tiff = self.to_tiff_with(&options)?;
        if located.format == Format::Tiff {
            let mut magic = [0u8; 4];
            let tiff = if container::Input::read_at(&mut input, 0, &mut magic)? {
                keep_magic(&magic, tiff)
            } else {
                tiff
            };
            dst.write_all(&tiff)?;
            return Ok(());
        }
        let pieces = container::plan(&mut input, located.format, Some(&tiff))?;
        pieces.emit(&mut input, &mut |bytes| Ok(dst.write_all(bytes)?))?;
        Ok(())
    }
}

/// Puts the original file's magic number into a rewritten TIFF file, when
/// both have the same byte order (the writer always writes 42).
fn keep_magic(original: &[u8], mut tiff: Vec<u8>) -> Vec<u8> {
    if let (Some(from), Some(to)) = (original.get(..4), tiff.get_mut(..4))
        && from.get(..2) == to.get(..2)
    {
        to.copy_from_slice(from);
    }
    tiff
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
