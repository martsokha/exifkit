//! Writing Exif back into image files. See `docs/containers.md`.

use alloc::borrow::Cow;
use alloc::vec::Vec;

use super::WriteOptions;
use super::plan::{Plan, Source};
use crate::container::{self, Format, Input, Located, Pieces, Slice};
use crate::diagnostic::Diagnostics;
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
    /// - If the file's own Exif block cannot serve as the source:
    ///   [`Error::LimitExceeded`] when it is over
    ///   [`Limits::max_metadata`](crate::Limits::max_metadata) (outside
    ///   TIFF), or [`Error::Unsupported`] when this build cannot decode it.
    ///   Writing without it would silently give a different file.
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
        let mut input = Slice(original);
        let located = container::locate_in(&mut input, &mut Diagnostics::default())?;
        if located.format == Format::Tiff {
            // A TIFF file is its own source, unless the caller gives one.
            let source = options.source.unwrap_or(original);
            return self
                .tiff_pieces(options, &source, magic(&mut input)?)?
                .to_vec(&mut Slice(source));
        }
        let own = source_in(&mut input, &located, options)?;
        let mut options = *options;
        options.source = options.source.or(own.as_deref());
        let tiff = self.to_tiff_with(&options)?;
        container::plan(&mut input, located.format, Some(&tiff))?.to_vec(&mut input)
    }

    /// Writes the metadata into the file read from `src`, writing the new
    /// file to `dst`. The result is the same as [`Exif::write_into_with`],
    /// but streamed: only headers, small metadata chunks and the Exif block
    /// are read into memory, and the image data is copied through in 64 KiB
    /// blocks.
    ///
    /// Memory use is bounded by the metadata, not the image: the file's own
    /// Exif block (the writer's source) is read only if this build can
    /// decode it, and at most [`Limits::max_metadata`](crate::Limits::max_metadata)
    /// of it (64 MiB by default, set through [`WriteOptions::limits`]); a
    /// Brotli-compressed JPEG XL block is decompressed as it is read, with
    /// its output capped the same way. A TIFF or TIFF-based raw file is its
    /// own Exif block: the writer reads its IFDs and values on demand and
    /// copies the strips and tiles through from `src`, never holding them.
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
        let located = container::locate_in(&mut input, &mut Diagnostics::default())?;
        if located.format == Format::Tiff {
            let magic = magic(&mut input)?;
            let pieces = match options.source {
                // As `write_into_with`, against the caller's source.
                Some(source) => {
                    let tiff = self
                        .tiff_pieces(options, &source, magic)?
                        .to_vec(&mut Slice(source))?;
                    dst.write_all(&tiff)?;
                    return Ok(());
                }
                // A TIFF file is its own source: plan against it read on
                // demand (IFDs and values only), then copy the strips and
                // tiles through from the stream.
                None => {
                    let len = container::Input::len(&mut input)?.unwrap_or(0);
                    let paged = crate::tiff::Paged::new(&mut input, len);
                    let pieces = self.tiff_pieces(options, &paged, magic);
                    if let Some(err) = paged.take_error() {
                        return Err(err);
                    }
                    pieces?
                }
            };
            pieces.emit(&mut input, &mut |bytes| Ok(dst.write_all(bytes)?))?;
            return Ok(());
        }
        let own = source_in(&mut input, &located, options)?;
        let mut options = *options;
        options.source = options.source.or(own.as_deref());
        let tiff = self.to_tiff_with(&options)?;
        let pieces = container::plan(&mut input, located.format, Some(&tiff))?;
        pieces.emit(&mut input, &mut |bytes| Ok(dst.write_all(bytes)?))?;
        Ok(())
    }
}

impl Exif {
    /// Plans a TIFF file rewritten against `source`, keeping `magic` (a raw
    /// format's own). The pieces copy image data from `source` by range.
    fn tiff_pieces(
        &self,
        options: &WriteOptions<'_>,
        source: &dyn Source,
        magic: u16,
    ) -> Result<Pieces> {
        Plan::with_source(self, options, Some(source))?
            .with_magic(magic)?
            .pieces()
    }
}

/// The magic number of the TIFF file behind `input`: 42, or a raw format's.
fn magic(input: &mut impl Input) -> Result<u16> {
    let mut header = [0u8; 8];
    Ok(if input.read_at(0, &mut header)? {
        crate::tiff::Header::parse(&header).map_or(crate::tiff::Header::TIFF, |h| h.magic)
    } else {
        crate::tiff::Header::TIFF
    })
}

/// The file's own Exif block, decoded as the reader decodes it, to serve as
/// the writer's source (`None` if the file has none or the caller gave a
/// source). Both write paths take it from here, so they agree.
///
/// Memory stays bounded by the metadata: a block this build cannot decode
/// fails before it is read; a raw block over
/// [`Limits::max_metadata`](crate::Limits::max_metadata) is refused; a
/// compressed one is decompressed as it is read, output capped the same way.
///
/// # Errors
///
/// When the block cannot serve, the write fails rather than silently giving
/// a different file: [`Error::Unsupported`] if this build cannot decode it,
/// [`Error::LimitExceeded`] past the cap, or the decoding error.
fn source_in(
    input: &mut impl Input,
    located: &Located,
    options: &WriteOptions<'_>,
) -> Result<Option<Vec<u8>>> {
    let (Some(range), None) = (&located.exif, options.source) else {
        return Ok(None);
    };
    let limits = &options.limits;
    let file_len = input
        .len()?
        .and_then(|len| usize::try_from(len).ok())
        .unwrap_or(usize::MAX);
    let budget = limits.metadata_budget(file_len);
    let mut diagnostics = Diagnostics::default();
    if !container::decodable(located.encoding) {
        // Fails with the reason (a disabled feature), before reading.
        container::tiff_payload(Cow::Borrowed(&[]), located.encoding, 0, &mut diagnostics)?;
    }
    #[cfg(feature = "jxl-brob")]
    if located.encoding == container::Encoding::Brotli {
        return container::decode_brob(input, range.clone(), budget).map(Some);
    }
    let max = u64::try_from(limits.max_metadata).unwrap_or(u64::MAX);
    if range.end.saturating_sub(range.start) > max {
        return Err(Error::LimitExceeded(crate::limits::Limit::Metadata));
    }
    let payload = container::read_range(input, range.clone())?;
    let (block, _) = container::tiff_payload(
        Cow::Owned(payload),
        located.encoding,
        budget,
        &mut diagnostics,
    )?;
    Ok(Some(block.into_owned()))
}

/// Removes the Exif data from a JPEG, PNG, WebP or JPEG XL file: the Exif
/// APP1 segments, the `eXIf` chunk (and legacy raw-profile text chunks), the
/// `EXIF` chunk (clearing the `VP8X` flag), or the `Exif` and Exif `brob`
/// boxes. Everything else is copied byte
/// for byte. A file without Exif is returned unchanged.
///
/// # Errors
///
/// [`Error::UnknownFormat`], [`Error::Unsupported`] for TIFF (its metadata is
/// the file's structure) and HEIF, or [`Error::Malformed`] if the
/// file is too broken to rewrite safely.
pub fn remove_exif(original: &[u8]) -> Result<Vec<u8>> {
    let (located, _) = container::locate(original)?;
    match located.format {
        Format::Heif => Err(Error::Unsupported {
            what: "HEIF is not writable yet",
        }),
        format => container::splice(original, format, None),
    }
}
