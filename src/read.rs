//! Reading Exif from image files. See `docs/containers.md`.

use alloc::borrow::Cow;

use crate::container::{self, Slice};
use crate::diagnostic::Diagnostics;
use crate::error::{Error, Result};
use crate::model::{Exif, ReadOptions, check_strict};

impl Exif {
    /// Reads the Exif metadata of an image file held in memory, detecting
    /// its format (JPEG, TIFF, PNG or WebP), with the default [`ReadOptions`].
    ///
    /// # Errors
    ///
    /// [`Error::UnknownFormat`] or [`Error::Unsupported`] for a file this
    /// build cannot read, [`Error::NoExif`] if it has no Exif data, or any
    /// error of [`Exif::from_tiff`].
    pub fn read(data: &[u8]) -> Result<(Self, Diagnostics)> {
        Self::read_with(data, &ReadOptions::DEFAULT)
    }

    /// Reads the Exif metadata of an image file held in memory.
    ///
    /// # Errors
    ///
    /// As [`Exif::read`]; in strict mode also on the first diagnostic of
    /// [`Severity::Error`](crate::Severity::Error), including container ones.
    pub fn read_with(data: &[u8], options: &ReadOptions) -> Result<(Self, Diagnostics)> {
        let mut diagnostics = Diagnostics::default();
        let located = container::locate_in(&mut Slice(data), &mut diagnostics)?;
        let range = located.exif.clone().ok_or(Error::NoExif)?;
        let start = usize::try_from(range.start).unwrap_or(usize::MAX);
        let end = usize::try_from(range.end)
            .unwrap_or(usize::MAX)
            .min(data.len());
        let payload = data.get(start..end).unwrap_or_default();
        finish(
            Cow::Borrowed(payload),
            located.encoding,
            Some(range.start),
            data.len(),
            options,
            diagnostics,
        )
    }

    /// Reads the Exif metadata of an image file from a stream, reading only
    /// the parts that hold metadata (TIFF files are read whole).
    ///
    /// # Errors
    ///
    /// As [`Exif::read`], plus [`Error::Io`].
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    pub fn read_from<R: std::io::Read + std::io::Seek>(reader: R) -> Result<(Self, Diagnostics)> {
        Self::read_from_with(reader, &ReadOptions::DEFAULT)
    }

    /// Reads the Exif metadata of an image file from a stream.
    ///
    /// # Errors
    ///
    /// As [`Exif::read_with`], plus [`Error::Io`].
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    pub fn read_from_with<R: std::io::Read + std::io::Seek>(
        reader: R,
        options: &ReadOptions,
    ) -> Result<(Self, Diagnostics)> {
        use std::io::{Read, SeekFrom};

        let mut diagnostics = Diagnostics::default();
        let mut stream = container::Stream(reader);
        let located = container::locate_in(&mut stream, &mut diagnostics)?;
        let range = located.exif.clone().ok_or(Error::NoExif)?;
        // Budget from the whole file, as `read_with` does, so both paths agree.
        let total = container::Input::len(&mut stream)?.unwrap_or(range.end);
        let file_len = usize::try_from(total).unwrap_or(usize::MAX);
        // A payload this build cannot decode fails before any of it is read.
        if !container::decodable(located.encoding) {
            container::tiff_payload(Cow::Borrowed(&[]), located.encoding, 0, &mut diagnostics)?;
        }
        // A compressed JPEG XL block is decompressed straight from the
        // stream, so the compressed box is never held whole.
        #[cfg(feature = "jxl-brob")]
        if located.encoding == container::Encoding::Brotli {
            let limit = options.limits.metadata_budget(file_len);
            let tiff = container::decode_brob_in(&mut stream, range, limit)?;
            return finish(
                Cow::Owned(tiff),
                container::Encoding::Raw,
                None,
                file_len,
                options,
                diagnostics,
            );
        }
        let mut reader = stream.0;
        reader.seek(SeekFrom::Start(range.start))?;
        // `take` + `read_to_end` grows the buffer only as bytes arrive, so a
        // hostile length cannot force a large allocation.
        let mut payload = alloc::vec::Vec::new();
        reader
            .take(range.end.saturating_sub(range.start))
            .read_to_end(&mut payload)?;
        finish(
            Cow::Owned(payload),
            located.encoding,
            Some(range.start),
            file_len,
            options,
            diagnostics,
        )
    }
}

/// Decodes the payload and builds the model. `payload_start` is where the
/// payload sits in the file, or `None` if it was already decoded (and so has
/// no file position for diagnostics).
fn finish(
    payload: Cow<'_, [u8]>,
    encoding: container::Encoding,
    payload_start: Option<u64>,
    file_len: usize,
    options: &ReadOptions,
    mut diagnostics: Diagnostics,
) -> Result<(Exif, Diagnostics)> {
    let budget = options.limits.metadata_budget(file_len);
    let (tiff, tiff_start) = container::tiff_payload(payload, encoding, budget, &mut diagnostics)?;
    let (exif, mut more) = Exif::from_tiff_lenient(&tiff, options)?;
    // IFD offsets are relative to the TIFF block; report them relative to the
    // file, like container offsets (decoded PNG text has no file position).
    if let (Some(payload_start), Some(start)) = (payload_start, tiff_start) {
        more.rebase(payload_start.saturating_add(start));
    }
    diagnostics.extend(more);
    check_strict(&diagnostics, options)?;
    Ok((exif, diagnostics))
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::diagnostic::DiagnosticKind;
    use crate::tag::Tag;

    /// A TIFF block with IFD0 = { Orientation = 6 }.
    const TIFF: &[u8] = &[
        b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
    ];

    fn jpeg_with(app1: &[u8]) -> Vec<u8> {
        let mut out = alloc::vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend_from_slice(&u16::try_from(app1.len() + 2).unwrap().to_be_bytes());
        out.extend_from_slice(app1);
        out.extend_from_slice(&[0xFF, 0xDA, 0, 2, 0xFF, 0xD9]);
        out
    }

    #[test]
    fn reads_every_container() {
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(TIFF);
        let jpeg = jpeg_with(&app1);

        let mut webp = b"RIFF\0\0\0\0WEBPEXIF".to_vec();
        webp.extend_from_slice(&u32::try_from(TIFF.len()).unwrap().to_le_bytes());
        webp.extend_from_slice(TIFF);
        let riff_len = u32::try_from(webp.len() - 8).unwrap();
        webp[4..8].copy_from_slice(&riff_len.to_le_bytes());

        for data in [&jpeg[..], TIFF, &webp] {
            let (exif, diagnostics) = Exif::read(data).unwrap();
            assert_eq!(exif.get::<u16>(Tag::ORIENTATION), Ok(Some(6)));
            assert!(diagnostics.is_empty(), "{diagnostics:?}");
            #[cfg(feature = "std")]
            assert_eq!(
                Exif::read_from(std::io::Cursor::new(data)).unwrap(),
                (exif, diagnostics)
            );
        }
    }

    #[test]
    fn no_exif_and_unknown_format() {
        let jfif = [
            0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xDA, 0, 2, 0xFF, 0xD9,
        ];
        assert!(matches!(Exif::read(&jfif), Err(Error::NoExif)));
        assert!(matches!(
            Exif::read(b"GIF89a\0\0\0\0\0\0"),
            Err(Error::UnknownFormat)
        ));
        assert!(matches!(Exif::read(b""), Err(Error::UnknownFormat)));
    }

    #[test]
    fn ifd_offsets_are_file_relative() {
        // Replace Orientation with a Make (ASCII) whose 100 bytes would sit
        // at TIFF offset 10_000, far out of bounds.
        let mut tiff = TIFF.to_vec();
        tiff[10..22].copy_from_slice(&[0x0F, 0x01, 2, 0, 100, 0, 0, 0, 0x10, 0x27, 0, 0]);
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let jpeg = jpeg_with(&app1);
        let tiff_start: u64 = 2 + 4 + 6; // SOI, APP1 marker and length, "Exif\0\0"
        let (_, from_tiff) = Exif::from_tiff(&tiff).unwrap();
        let (_, from_file) = Exif::read(&jpeg).unwrap();
        let first = |d: &Diagnostics| d.iter().next().and_then(|d| d.offset);
        assert_eq!(first(&from_tiff), Some(10_000));
        assert_eq!(first(&from_file), Some(10_000 + tiff_start));
        let strict = Exif::read_with(&jpeg, &ReadOptions::DEFAULT.strict(true));
        assert!(
            matches!(strict, Err(Error::Malformed { offset, .. }) if offset == 10_000 + tiff_start)
        );
    }

    #[test]
    fn container_diagnostics_reach_strict_mode() {
        // APP1 declares more bytes than the file has.
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(TIFF);
        let mut data = jpeg_with(&app1);
        data.truncate(4 + 2 + app1.len() - 1);
        let (_, diagnostics) = Exif::read(&data).unwrap();
        assert!(diagnostics.contains(DiagnosticKind::TruncatedContainer));
        let strict = Exif::read_with(&data, &ReadOptions::DEFAULT.strict(true));
        assert!(matches!(strict, Err(Error::Malformed { .. })));
    }

    /// A JPEG XL container whose Exif is a `brob` box: `brotli -q 11` of a
    /// zero offset and an empty little-endian TIFF block.
    #[cfg(feature = "jxl")]
    fn jxl_with_brob() -> Vec<u8> {
        let mut out = b"\0\0\0\x0CJXL \r\n\x87\n\0\0\0\x14ftypjxl \0\0\0\0jxl ".to_vec();
        let compressed: &[u8] = &[
            0xa1, 0x88, 0x00, 0xc0, 0x3f, 0x0d, 0x90, 0xa4, 0x82, 0x30, 0x0c, 0x0a, 0x04, 0x80,
            0xba, 0x01,
        ];
        out.extend_from_slice(&u32::try_from(12 + compressed.len()).unwrap().to_be_bytes());
        out.extend_from_slice(b"brobExif");
        out.extend_from_slice(compressed);
        out.extend_from_slice(b"\0\0\0\x0Ajxlc\xFF\x0A");
        out
    }

    #[test]
    #[cfg(all(feature = "jxl-brob", feature = "std"))]
    fn streamed_brotli_exif_reads_as_in_memory() {
        let data = jxl_with_brob();
        let (in_memory, _) = Exif::read(&data).unwrap();
        let (streamed, _) = Exif::read_from(std::io::Cursor::new(&data)).unwrap();
        assert_eq!(streamed, in_memory);
        // `max_metadata` bounds the decompressed block on both paths.
        let tight = ReadOptions::DEFAULT.limits(crate::Limits {
            max_metadata: 8,
            ..crate::Limits::DEFAULT
        });
        assert!(matches!(
            Exif::read_with(&data, &tight),
            Err(Error::LimitExceeded(_))
        ));
        assert!(matches!(
            Exif::read_from_with(std::io::Cursor::new(&data), &tight),
            Err(Error::LimitExceeded(_))
        ));
    }

    #[test]
    #[cfg(all(feature = "jxl", feature = "std", not(feature = "jxl-brob")))]
    fn undecodable_exif_fails_before_reading_it() {
        let data = jxl_with_brob();
        assert!(matches!(
            Exif::read_from(std::io::Cursor::new(&data)),
            Err(Error::Unsupported { .. })
        ));
    }
}
