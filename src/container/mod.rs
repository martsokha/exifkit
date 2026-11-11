//! Image containers: finding the Exif data in JPEG, TIFF, PNG and WebP.
//! See `docs/containers.md`.
//!
//! Each format's scanner works over an internal `Input` trait, so the same
//! code reads a byte slice in place or a `Read + Seek` stream without
//! loading the file.

mod input;
#[cfg(feature = "jpeg")]
mod jpeg;
#[cfg(feature = "png")]
mod png;
#[cfg(feature = "webp")]
mod webp;

use core::fmt;
use core::ops::Range;

#[cfg(feature = "std")]
pub(crate) use self::input::Stream;
pub(crate) use self::input::{Input, Slice};
use crate::diagnostic::{Diagnostic, DiagnosticKind, Diagnostics};
use crate::error::{Error, Result};

/// An image file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Format {
    /// JPEG (JFIF or Exif).
    Jpeg,
    /// TIFF, including TIFF-based raw files such as DNG.
    Tiff,
    /// PNG.
    Png,
    /// WebP.
    WebP,
    /// HEIF, HEIC or AVIF. Detected, not yet readable (milestone M3).
    Heif,
    /// JPEG XL. Detected, not yet readable (milestone M3).
    Jxl,
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Jpeg => "JPEG",
            Self::Tiff => "TIFF",
            Self::Png => "PNG",
            Self::WebP => "WebP",
            Self::Heif => "HEIF",
            Self::Jxl => "JPEG XL",
        })
    }
}

/// How many leading bytes [`detect`] looks at.
pub const DETECT_LEN: usize = 16;

/// Detects the format from the first bytes of a file (16 are enough).
#[must_use]
pub fn detect(prefix: &[u8]) -> Option<Format> {
    const HEIF_BRANDS: &[&[u8; 4]] = &[
        b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx", b"mif1", b"msf1", b"avif", b"avis",
    ];
    match prefix {
        [0xFF, 0xD8, 0xFF, ..] => Some(Format::Jpeg),
        [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..] => Some(Format::Tiff),
        [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n', ..] => Some(Format::Png),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some(Format::WebP),
        [
            0,
            0,
            0,
            0x0C,
            b'J',
            b'X',
            b'L',
            b' ',
            b'\r',
            b'\n',
            0x87,
            b'\n',
            ..,
        ]
        | [0xFF, 0x0A, ..] => Some(Format::Jxl),
        [_, _, _, _, b'f', b't', b'y', b'p', a, b, c, d, ..]
            if HEIF_BRANDS.contains(&&[*a, *b, *c, *d]) =>
        {
            Some(Format::Heif)
        }
        _ => None,
    }
}

/// How the Exif payload is stored in its container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Encoding {
    /// The bytes at [`Located::exif`] are the TIFF block itself.
    Raw,
    /// PNG legacy text chunk ("Raw profile type exif"): hex text, and
    /// zlib-compressed when `compressed`. [`Located::exif`] is the chunk data.
    PngRawProfile {
        /// Whether the text is zlib-compressed (`zTXt`, compressed `iTXt`).
        compressed: bool,
        /// Byte offset of the text within the chunk data (after keyword and flags).
        text_offset: u32,
    },
}

/// Where a file keeps its Exif data.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Located {
    /// The file format.
    pub format: Format,
    /// Byte range of the Exif payload in the file, if there is one.
    pub exif: Option<Range<u64>>,
    /// How the payload is stored.
    pub encoding: Encoding,
}

/// Finds the Exif data in a file held in memory.
///
/// Non-fatal problems (a second Exif block, a CRC mismatch, …) are returned
/// as [`Diagnostics`].
///
/// # Errors
///
/// [`Error::UnknownFormat`] if the format is not recognized, or
/// [`Error::Unsupported`] for a recognized format this build cannot read.
pub fn locate(data: &[u8]) -> Result<(Located, Diagnostics)> {
    let mut diagnostics = Diagnostics::default();
    let located = locate_in(&mut Slice(data), &mut diagnostics)?;
    Ok((located, diagnostics))
}

/// Finds the Exif data in any [`Input`].
pub(crate) fn locate_in(input: &mut impl Input, diagnostics: &mut Diagnostics) -> Result<Located> {
    let mut prefix = [0u8; DETECT_LEN];
    let prefix = match input.read_at(0, &mut prefix)? {
        true => &prefix[..],
        false => {
            // Short file: detect from what there is.
            let len = input.len()?.map_or(0, |n| usize::try_from(n).unwrap_or(0));
            let mut short = alloc::vec![0u8; len.min(DETECT_LEN)];
            if !input.read_at(0, &mut short)? {
                return Err(Error::UnknownFormat);
            }
            return finish_detect(input, diagnostics, detect(&short));
        }
    };
    finish_detect(input, diagnostics, detect(prefix))
}

fn finish_detect(
    input: &mut impl Input,
    diagnostics: &mut Diagnostics,
    format: Option<Format>,
) -> Result<Located> {
    let format = format.ok_or(Error::UnknownFormat)?;
    #[cfg_attr(
        not(any(feature = "tiff", feature = "jpeg", feature = "webp")),
        allow(unused_variables)
    )]
    let raw = |exif: Option<Range<u64>>| Located {
        format,
        exif,
        encoding: Encoding::Raw,
    };
    match format {
        Format::Tiff => {
            #[cfg(feature = "tiff")]
            {
                let len = input.len()?.ok_or(Error::Unsupported {
                    what: "TIFF stream of unknown length",
                })?;
                Ok(raw(Some(0..len)))
            }
            #[cfg(not(feature = "tiff"))]
            {
                let _ = input;
                Err(Error::Unsupported {
                    what: "TIFF (enable the `tiff` feature)",
                })
            }
        }
        Format::Jpeg => {
            #[cfg(feature = "jpeg")]
            return jpeg::locate(input, diagnostics).map(raw);
            #[cfg(not(feature = "jpeg"))]
            return Err(Error::Unsupported {
                what: "JPEG (enable the `jpeg` feature)",
            });
        }
        Format::Png => {
            #[cfg(feature = "png")]
            return png::locate(input, diagnostics);
            #[cfg(not(feature = "png"))]
            return Err(Error::Unsupported {
                what: "PNG (enable the `png` feature)",
            });
        }
        Format::WebP => {
            #[cfg(feature = "webp")]
            return webp::locate(input, diagnostics).map(raw);
            #[cfg(not(feature = "webp"))]
            return Err(Error::Unsupported {
                what: "WebP (enable the `webp` feature)",
            });
        }
        Format::Heif | Format::Jxl => {
            let _ = diagnostics;
            Err(Error::Unsupported {
                what: "HEIF and JPEG XL are not readable yet",
            })
        }
    }
}

/// Turns a located payload into a TIFF block: decodes PNG raw profiles and
/// strips an `Exif\0\0` header that some writers add.
pub(crate) fn tiff_payload<'a>(
    payload: alloc::borrow::Cow<'a, [u8]>,
    encoding: Encoding,
    limit: usize,
    diagnostics: &mut Diagnostics,
) -> Result<alloc::borrow::Cow<'a, [u8]>> {
    use alloc::borrow::Cow;

    let payload = match encoding {
        Encoding::Raw => payload,
        Encoding::PngRawProfile {
            compressed,
            text_offset,
        } => {
            #[cfg(feature = "png-legacy")]
            {
                let text = payload
                    .get(usize::try_from(text_offset).unwrap_or(usize::MAX)..)
                    .unwrap_or_default();
                Cow::Owned(png::decode_raw_profile(text, compressed, limit)?)
            }
            #[cfg(not(feature = "png-legacy"))]
            {
                let _ = (compressed, text_offset, limit);
                return Err(Error::Unsupported {
                    what: "legacy PNG Exif text chunk (enable the `png-legacy` feature)",
                });
            }
        }
    };
    // **quirk:** some PNG and WebP writers keep JPEG's `Exif\0\0` header.
    if let Some(rest) = payload.strip_prefix(b"Exif\0\0") {
        diagnostics.push(Diagnostic::new(DiagnosticKind::NonStandardExifHeader));
        return Ok(match payload {
            Cow::Borrowed(all) => Cow::Borrowed(all.get(6..).unwrap_or_default()),
            Cow::Owned(_) => Cow::Owned(rest.to_vec()),
        });
    }
    Ok(payload)
}

/// Reports that the container ends before a structure it declares.
#[cfg_attr(
    not(any(feature = "jpeg", feature = "png", feature = "webp")),
    allow(dead_code)
)]
pub(crate) fn truncated(diagnostics: &mut Diagnostics, offset: u64) {
    diagnostics.push(Diagnostic::new(DiagnosticKind::TruncatedContainer).at_file(offset));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_formats() {
        assert_eq!(detect(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Format::Jpeg));
        assert_eq!(detect(b"II*\0\x08\0\0\0"), Some(Format::Tiff));
        assert_eq!(detect(b"MM\0*\0\0\0\x08"), Some(Format::Tiff));
        assert_eq!(detect(b"\x89PNG\r\n\x1a\n\0\0"), Some(Format::Png));
        assert_eq!(detect(b"RIFF\0\0\0\0WEBPVP8X"), Some(Format::WebP));
        assert_eq!(detect(b"\0\0\0\x18ftypheic\0\0\0\0"), Some(Format::Heif));
        assert_eq!(detect(b"\0\0\0\x1cftypavif\0\0\0\0"), Some(Format::Heif));
        assert_eq!(detect(b"\0\0\0\x0cJXL \r\n\x87\n"), Some(Format::Jxl));
        assert_eq!(detect(b"GIF89a"), None);
        assert_eq!(detect(b""), None);
    }

    #[test]
    fn heif_and_jxl_are_reported_as_unsupported() {
        let heic = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic";
        assert!(matches!(locate(heic), Err(Error::Unsupported { .. })));
        assert!(matches!(
            locate(b"GIF89a......."),
            Err(Error::UnknownFormat)
        ));
    }
}
