//! Turning a located Exif payload into a TIFF block: decoding compressed or
//! hex-encoded payloads, under a fixed cap.

#[cfg(feature = "jxl-brob")]
use core::ops::Range;

use super::Encoding;
#[cfg(feature = "jxl-brob")]
use super::Input;
use crate::diagnostic::{Diagnostic, DiagnosticKind, Diagnostics};
use crate::error::Result;

/// Most bytes a decoded metadata block may inflate to where no
/// [`Limits`](crate::Limits) are passed in (`xmp`, `icc_profile`, sizing a
/// JPEG XL Exif box): [`Limits::max_metadata`](crate::Limits::max_metadata)'s default.
#[cfg(any(all(feature = "png", feature = "inflate"), feature = "jxl"))]
pub(crate) const METADATA_LIMIT: usize = crate::limits::Limits::DEFAULT.max_metadata;

/// Decodes a Brotli-compressed JPEG XL Exif payload straight from `range` of
/// `input`: the compressed data is streamed into the decompressor, never held
/// whole. Returns the TIFF block.
#[cfg(feature = "jxl-brob")]
pub(crate) fn decode_brob(
    input: &mut impl Input,
    range: Range<u64>,
    limit: usize,
) -> Result<alloc::vec::Vec<u8>> {
    super::jxl::decode_brob(input, range, limit)
}

/// Whether this build can decode a payload stored this way.
pub(crate) const fn decodable(encoding: Encoding) -> bool {
    match encoding {
        Encoding::Raw => true,
        Encoding::Brotli => cfg!(feature = "jxl-brob"),
        Encoding::PngRawProfile { .. } => cfg!(feature = "png-legacy"),
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
                let len = u64::try_from(payload.len()).unwrap_or(u64::MAX);
                Cow::Owned(super::jxl::decode_brob(
                    &mut super::Slice(&payload),
                    0..len,
                    limit,
                )?)
            }
            #[cfg(not(feature = "jxl"))]
            {
                return Err(crate::error::Error::Unsupported {
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
                Cow::Owned(super::png::decode_raw_profile(text, compressed, limit)?)
            }
            #[cfg(not(feature = "png-legacy"))]
            {
                let _ = (compressed, text_offset, limit);
                return Err(crate::error::Error::Unsupported {
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
