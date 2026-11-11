//! Image containers: finding the Exif data in JPEG, TIFF, PNG and WebP.
//! See `docs/containers.md`.
//!
//! Each format's scanner works over an internal `Input` trait, so the same
//! code reads a byte slice in place or a `Read + Seek` stream without
//! loading the file.

#[cfg(feature = "jxl")]
mod bmff;
mod input;
#[cfg(feature = "jpeg")]
mod jpeg;
#[cfg(feature = "jxl")]
mod jxl;
// Each format uses a different part of the piece list.
#[cfg_attr(
    not(all(
        feature = "jpeg",
        feature = "png-legacy",
        feature = "webp",
        feature = "std"
    )),
    allow(dead_code)
)]
mod pieces;

#[cfg(feature = "std")]
pub(crate) use self::pieces::read_range;
#[cfg(feature = "png")]
mod png;
#[cfg(feature = "webp")]
mod webp;

use alloc::borrow::Cow;
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

/// Most bytes a decoded metadata block (compressed Exif or XMP, a PNG ICC
/// profile) may inflate to. Fixed rather than scaled with the file, so a
/// small compressed box in a large image cannot expand without bound.
pub(crate) const METADATA_LIMIT: usize = 64 * 1024 * 1024;

/// Whether this build can decode a payload stored this way.
#[cfg(feature = "std")]
pub(crate) const fn decodable(encoding: Encoding) -> bool {
    match encoding {
        Encoding::Raw => true,
        Encoding::Brotli => cfg!(feature = "jxl-brob"),
        Encoding::PngRawProfile { .. } => cfg!(feature = "png-legacy"),
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
        // TIFF, and raw formats with a TIFF structure under their own magic:
        // Olympus ORF (IIRO, MMOR, IIRS) and Panasonic RW2 (IIU\0).
        [b'I', b'I', 42, 0, ..]
        | [b'M', b'M', 0, 42, ..]
        | [b'I', b'I', b'R', b'O' | b'S', ..]
        | [b'M', b'M', b'O', b'R', ..]
        | [b'I', b'I', b'U', 0, ..] => Some(Format::Tiff),
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
    /// JPEG XL `brob` box wrapping an `Exif` box: Brotli-compressed.
    /// [`Located::exif`] is the compressed data (feature `jxl-brob`).
    Brotli,
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
        Format::Jxl => {
            #[cfg(feature = "jxl")]
            return jxl::locate(input, diagnostics);
            #[cfg(not(feature = "jxl"))]
            return Err(Error::Unsupported {
                what: "JPEG XL (enable the `jxl` feature)",
            });
        }
        Format::Heif => {
            let _ = diagnostics;
            Err(Error::Unsupported {
                what: "HEIF is not readable yet",
            })
        }
    }
}

/// Turns a located payload into a TIFF block: decodes PNG raw profiles and
/// strips an `Exif\0\0` header that some writers add. Also returns where
/// the TIFF block starts within the payload, or `None` if the block was
/// decoded and so has no position in the file.
pub(crate) fn tiff_payload<'a>(
    payload: alloc::borrow::Cow<'a, [u8]>,
    encoding: Encoding,
    limit: usize,
    diagnostics: &mut Diagnostics,
) -> Result<(alloc::borrow::Cow<'a, [u8]>, Option<u64>)> {
    use alloc::borrow::Cow;

    let decoded = encoding != Encoding::Raw;
    let payload = match encoding {
        Encoding::Raw => payload,
        Encoding::Brotli => {
            #[cfg(feature = "jxl")]
            {
                Cow::Owned(jxl::decode_brob(&payload, limit)?)
            }
            #[cfg(not(feature = "jxl"))]
            {
                return Err(Error::Unsupported {
                    what: "JPEG XL (enable the `jxl` feature)",
                });
            }
        }
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
    let position = |skipped: u64| (!decoded).then_some(skipped);
    if let Some(rest) = payload.strip_prefix(b"Exif\0\0") {
        diagnostics.push(Diagnostic::new(DiagnosticKind::NonStandardExifHeader));
        let stripped = match payload {
            Cow::Borrowed(all) => Cow::Borrowed(all.get(6..).unwrap_or_default()),
            Cow::Owned(_) => Cow::Owned(rest.to_vec()),
        };
        return Ok((stripped, position(6)));
    }
    Ok((payload, position(0)))
}

/// The XMP packet of a JPEG, TIFF, PNG or WebP file, as stored: the bytes
/// of the XML, which exifkit does not parse. Returns `None` if there is none.
///
/// Where it is read from: the JPEG APP1 XMP segment (the standard packet;
/// Extended XMP segments are not merged in), TIFF tag 0x02BC in IFD0, the
/// PNG `iTXt` chunk `XML:com.adobe.xmp`, or the WebP `XMP ` chunk. Writing
/// Exif never touches it.
///
/// XMP often repeats Exif fields (`exif:` and `tiff:` properties). After
/// editing Exif, the two may disagree; which one wins is up to the reader.
///
/// # Errors
///
/// [`Error::UnknownFormat`], [`Error::Unsupported`] (HEIF, JPEG XL, a
/// disabled feature, or compressed PNG XMP without `inflate`), or
/// [`Error::Malformed`] if the container cannot be walked.
pub fn xmp(data: &[u8]) -> Result<Option<Cow<'_, [u8]>>> {
    match detect(data).ok_or(Error::UnknownFormat)? {
        #[cfg(feature = "jpeg")]
        Format::Jpeg => Ok(jpeg::xmp(data)?.map(Cow::Borrowed)),
        #[cfg(feature = "tiff")]
        Format::Tiff => Ok(tiff_tag(data, 0x02BC).map(Cow::Borrowed)),
        #[cfg(feature = "png")]
        Format::Png => png::xmp(data),
        #[cfg(feature = "webp")]
        Format::WebP => Ok(webp::chunk_data(data, b"XMP ")?.map(Cow::Borrowed)),
        #[cfg(feature = "jxl")]
        Format::Jxl => jxl::xmp(data),
        format => Err(unreadable(format)),
    }
}

/// The ICC color profile of a JPEG, TIFF, PNG or WebP file, as stored.
/// Returns `None` if there is none.
///
/// Where it is read from: the JPEG APP2 `ICC_PROFILE` segments (joined in
/// sequence order), TIFF tag 0x8773 in IFD0, the PNG `iCCP` chunk
/// (inflated, so it needs the `inflate` feature), or the WebP `ICCP` chunk.
/// Writing Exif never touches it.
///
/// # Errors
///
/// As [`xmp`], plus [`Error::Malformed`] for JPEG profile segments that
/// are missing or inconsistent, and [`Error::Unsupported`] for a PNG
/// profile without the `inflate` feature.
pub fn icc_profile(data: &[u8]) -> Result<Option<Cow<'_, [u8]>>> {
    match detect(data).ok_or(Error::UnknownFormat)? {
        #[cfg(feature = "jpeg")]
        Format::Jpeg => Ok(jpeg::icc(data)?.map(Cow::Owned)),
        #[cfg(feature = "tiff")]
        Format::Tiff => Ok(tiff_tag(data, 0x8773).map(Cow::Borrowed)),
        #[cfg(feature = "png")]
        Format::Png => Ok(png::icc(data)?.map(Cow::Owned)),
        #[cfg(feature = "webp")]
        Format::WebP => Ok(webp::chunk_data(data, b"ICCP")?.map(Cow::Borrowed)),
        // JPEG XL keeps its color encoding in the codestream header, which
        // would need a codestream decoder to reach.
        Format::Jxl => Err(Error::Unsupported {
            what: "the ICC profile of a JPEG XL file (it is in the codestream)",
        }),
        format => Err(unreadable(format)),
    }
}

/// The error for a format whose extras cannot be read.
fn unreadable(format: Format) -> Error {
    Error::Unsupported {
        what: match format {
            Format::Heif => "HEIF is not readable yet",
            _ => "this format (its feature is disabled)",
        },
    }
}

/// The bytes of a tag in IFD0 of a TIFF file.
#[cfg(feature = "tiff")]
fn tiff_tag(data: &[u8], tag: u16) -> Option<&[u8]> {
    let header = crate::tiff::Header::parse(data).ok()?;
    let reader = crate::tiff::Reader::new(data, header.byte_order);
    let ifd0 = crate::tiff::RawIfd::parse(reader, header.ifd0_offset)?;
    ifd0.entries().find(|e| e.tag == tag)?.data().ok()
}

/// Plans putting a new TIFF block into a JPEG, PNG or WebP file, or
/// removing the Exif data when `tiff` is `None`. Only the Exif segment or
/// chunk (and the container fields that depend on it) changes; everything
/// else is a range copied from the original.
pub(crate) fn plan(
    input: &mut impl Input,
    format: Format,
    tiff: Option<&[u8]>,
) -> Result<pieces::Pieces> {
    let _ = tiff;
    #[cfg_attr(
        not(any(feature = "jpeg", feature = "png", feature = "webp", feature = "jxl")),
        allow(unused_variables)
    )]
    let len = input.len()?.ok_or(Error::Unsupported {
        what: "stream of unknown length",
    })?;
    match format {
        #[cfg(feature = "jpeg")]
        Format::Jpeg => jpeg::plan(input, len, tiff),
        #[cfg(feature = "png")]
        Format::Png => png::plan(input, len, tiff),
        #[cfg(feature = "webp")]
        Format::WebP => webp::plan(input, len, tiff),
        #[cfg(feature = "jxl")]
        Format::Jxl => jxl::plan(input, len, tiff),
        Format::Tiff => Err(Error::Unsupported {
            what: "removing Exif from a TIFF file (its metadata is the file's structure)",
        }),
        #[allow(unreachable_patterns)]
        _ => Err(Error::Unsupported {
            what: "writing this format (not supported, or its feature is disabled)",
        }),
    }
}

/// [`plan`] applied to a file held in memory.
pub(crate) fn splice(
    data: &[u8],
    format: Format,
    tiff: Option<&[u8]>,
) -> Result<alloc::vec::Vec<u8>> {
    let mut input = Slice(data);
    plan(&mut input, format, tiff)?.to_vec(&mut input)
}

/// Reports that the container ends before a structure it declares.
#[cfg_attr(
    not(any(feature = "jpeg", feature = "png", feature = "webp")),
    allow(dead_code)
)]
pub(crate) fn truncated(diagnostics: &mut Diagnostics, offset: u64) {
    diagnostics.push(Diagnostic::new(DiagnosticKind::TruncatedContainer).at(offset));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_formats() {
        assert_eq!(detect(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Format::Jpeg));
        assert_eq!(detect(b"II*\0\x08\0\0\0"), Some(Format::Tiff));
        assert_eq!(detect(b"MM\0*\0\0\0\x08"), Some(Format::Tiff));
        assert_eq!(detect(b"IIRO\x08\0\0\0"), Some(Format::Tiff));
        assert_eq!(detect(b"MMOR\0\0\0\x08"), Some(Format::Tiff));
        assert_eq!(detect(b"IIRS\x08\0\0\0"), Some(Format::Tiff));
        assert_eq!(detect(b"IIU\0\x18\0\0\0"), Some(Format::Tiff));
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

    #[test]
    #[cfg(feature = "tiff")]
    fn tiff_extras_come_from_ifd0() {
        // IFD0 with XMLPacket (BYTE) and InterColorProfile (UNDEFINED).
        let mut tiff = b"II*\0\x08\0\0\0\x02\0".to_vec();
        tiff.extend_from_slice(&[0xBC, 0x02, 1, 0, 6, 0, 0, 0, 38, 0, 0, 0]);
        tiff.extend_from_slice(&[0x73, 0x87, 7, 0, 5, 0, 0, 0, 44, 0, 0, 0]);
        tiff.extend_from_slice(&[0; 4]);
        tiff.extend_from_slice(b"<x:x/>icc!!");
        assert_eq!(xmp(&tiff).unwrap().as_deref(), Some(&b"<x:x/>"[..]));
        assert_eq!(icc_profile(&tiff).unwrap().as_deref(), Some(&b"icc!!"[..]));
        assert!(matches!(xmp(b"GIF89a"), Err(Error::UnknownFormat)));
    }
}
