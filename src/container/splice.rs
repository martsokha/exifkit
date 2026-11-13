//! Writing a new Exif payload into a file, or removing it: dispatch to each
//! format's plan of copied ranges and new bytes.

use super::pieces::Pieces;
use super::{Format, Input, Slice};
use crate::error::{Error, Result};

/// Plans putting a new TIFF block into a JPEG, PNG or WebP file, or
/// removing the Exif data when `tiff` is `None`. Only the Exif segment or
/// chunk (and the container fields that depend on it) changes; everything
/// else is a range copied from the original.
pub(crate) fn plan(input: &mut impl Input, format: Format, tiff: Option<&[u8]>) -> Result<Pieces> {
    let _ = (&input, tiff);
    match format {
        #[cfg(feature = "jpeg")]
        Format::Jpeg => {
            let len = length(input)?;
            super::jpeg::plan(input, len, tiff)
        }
        #[cfg(feature = "png")]
        Format::Png => {
            let len = length(input)?;
            super::png::plan(input, len, tiff)
        }
        #[cfg(feature = "webp")]
        Format::WebP => {
            let len = length(input)?;
            super::webp::plan(input, len, tiff)
        }
        #[cfg(feature = "jxl")]
        Format::Jxl => {
            let len = length(input)?;
            super::jxl::plan(input, len, tiff)
        }
        Format::Tiff => Err(Error::Unsupported {
            what: "removing Exif from a TIFF file (its metadata is the file's structure)",
        }),
        _ => Err(Error::Unsupported {
            what: "writing this format (not supported, or its feature is disabled)",
        }),
    }
}

/// The input's length, which a plan needs up front.
#[cfg(any(feature = "jpeg", feature = "png", feature = "webp", feature = "jxl"))]
fn length(input: &mut impl Input) -> Result<u64> {
    input.len()?.ok_or(Error::Unsupported {
        what: "stream of unknown length",
    })
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
