//! Writing Exif back into image files. See `docs/containers.md`.

use alloc::borrow::Cow;
use alloc::vec::Vec;

use super::WriteOptions;
use crate::container::{self, Format};
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
        let (located, _) = container::locate(original)?;
        let mut options = *options;
        // The file's own block is the source; if it cannot serve (too big,
        // or not decodable here), fail rather than write a different file.
        let own = match (&located.exif, options.source) {
            (Some(range), None) => {
                check_size(&located, range, &options.limits)?;
                let at = |o: u64| usize::try_from(o).unwrap_or(usize::MAX);
                let payload = original
                    .get(at(range.start)..at(range.end).min(original.len()))
                    .unwrap_or_default();
                Some(own_block(
                    Cow::Borrowed(payload),
                    located.encoding,
                    original.len(),
                    &options.limits,
                )?)
            }
            _ => None,
        };
        if let Some(own) = &own {
            options.source = Some(own);
        }
        match located.format {
            // A TIFF file is its TIFF block; image data stays in place. A raw
            // format (ORF, RW2) keeps its own magic number.
            Format::Tiff => {
                let magic = crate::tiff::Header::parse(original)
                    .map_or(crate::tiff::Header::TIFF, |h| h.magic);
                super::plan::Plan::new(self, &options)?
                    .with_magic(magic)?
                    .serialize(options.source)
            }
            format => container::splice(original, format, Some(&self.to_tiff_with(&options)?)),
        }
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
        let mut diagnostics = crate::diagnostic::Diagnostics::default();
        let located = container::locate_in(&mut input, &mut diagnostics)?;
        // A TIFF file is its own source: plan against it read on demand, then
        // copy the image data through from the stream. Only the IFDs and
        // values are read, never the strips or tiles.
        if located.format == Format::Tiff && options.source.is_none() {
            let len = container::Input::len(&mut input)?.unwrap_or(0);
            let pieces = {
                let paged = crate::tiff::Paged::new(&mut input, len);
                let header = crate::tiff::Header::parse(&paged.header()?)?;
                let plan = super::plan::Plan::with_source(self, options, Some(&paged))?
                    .with_magic(header.magic)?;
                if let Some(err) = paged.take_error() {
                    return Err(err);
                }
                plan.pieces()?
            };
            pieces.emit(&mut input, &mut |bytes| Ok(dst.write_all(bytes)?))?;
            return Ok(());
        }
        // The file's own block, read from the stream, is the source, as in
        // `write_into_with`: if it cannot serve, the write fails. A block this
        // build cannot decode fails before it is read, and outside TIFF (whose
        // block is the whole file) no more than `max_metadata` is read. A
        // compressed block is decompressed as it is read, so only its capped
        // output is ever held.
        let file_len = container::Input::len(&mut input)?
            .and_then(|len| usize::try_from(len).ok())
            .unwrap_or(usize::MAX);
        let own = match (&located.exif, options.source) {
            (Some(range), None) => {
                if !container::decodable(located.encoding) {
                    container::tiff_payload(
                        Cow::Borrowed(&[]),
                        located.encoding,
                        0,
                        &mut diagnostics,
                    )?;
                }
                #[cfg(feature = "jxl-brob")]
                let decoded = if located.encoding == container::Encoding::Brotli {
                    let limit = options.limits.metadata_budget(file_len);
                    Some(container::decode_brob_in(&mut input, range.clone(), limit)?)
                } else {
                    None
                };
                #[cfg(not(feature = "jxl-brob"))]
                let decoded: Option<Vec<u8>> = None;
                Some(match decoded {
                    Some(tiff) => own_block(
                        Cow::Owned(tiff),
                        container::Encoding::Raw,
                        file_len,
                        &options.limits,
                    )?,
                    None => {
                        check_size(&located, range, &options.limits)?;
                        let payload = container::read_range(&mut input, range.clone())?;
                        own_block(
                            Cow::Owned(payload),
                            located.encoding,
                            file_len,
                            &options.limits,
                        )?
                    }
                })
            }
            _ => None,
        };
        let mut options = *options;
        if let Some(own) = &own {
            options.source = Some(own);
        }
        if located.format == Format::Tiff {
            // With a caller-given source, as `write_into_with` does.
            let mut magic = [0u8; 8];
            let magic = if container::Input::read_at(&mut input, 0, &mut magic)? {
                crate::tiff::Header::parse(&magic).map_or(crate::tiff::Header::TIFF, |h| h.magic)
            } else {
                crate::tiff::Header::TIFF
            };
            let tiff = super::plan::Plan::new(self, &options)?
                .with_magic(magic)?
                .serialize(options.source)?;
            dst.write_all(&tiff)?;
            return Ok(());
        }
        let tiff = self.to_tiff_with(&options)?;
        let pieces = container::plan(&mut input, located.format, Some(&tiff))?;
        pieces.emit(&mut input, &mut |bytes| Ok(dst.write_all(bytes)?))?;
        Ok(())
    }
}

/// The file's own TIFF block, decoded as the reader decodes it (a
/// compressed JPEG XL `brob` box or a legacy PNG text chunk included), to
/// serve as the writer's source.
///
/// # Errors
///
/// The decoding error ([`Error::Unsupported`] for a disabled feature,
/// [`Error::LimitExceeded`] past [`Limits::max_metadata`](crate::Limits::max_metadata)).
fn own_block<'a>(
    payload: Cow<'a, [u8]>,
    encoding: container::Encoding,
    file_len: usize,
    limits: &crate::limits::Limits,
) -> Result<Cow<'a, [u8]>> {
    let mut diagnostics = crate::diagnostic::Diagnostics::default();
    container::tiff_payload(
        payload,
        encoding,
        limits.metadata_budget(file_len),
        &mut diagnostics,
    )
    .map(|(block, _)| block)
}

/// Refuses an Exif block bigger than [`Limits::max_metadata`] as a source,
/// except in TIFF, where the block is the whole file.
///
/// [`Limits::max_metadata`]: crate::Limits::max_metadata
fn check_size(
    located: &container::Located,
    range: &core::ops::Range<u64>,
    limits: &crate::limits::Limits,
) -> Result<()> {
    let max = u64::try_from(limits.max_metadata).unwrap_or(u64::MAX);
    if located.format != Format::Tiff && range.end.saturating_sub(range.start) > max {
        return Err(Error::LimitExceeded(crate::limits::Limit::Metadata));
    }
    Ok(())
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
