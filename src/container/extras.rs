//! The other metadata a file carries beside Exif: the raw XMP packet and the
//! ICC profile. Read only; writing Exif never touches them.

use alloc::borrow::Cow;

use super::{Format, detect};
use crate::error::{Error, Result};

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
        Format::Jpeg => Ok(super::jpeg::xmp(data)?.map(Cow::Borrowed)),
        #[cfg(feature = "tiff")]
        Format::Tiff => Ok(tiff_tag(data, 0x02BC)),
        #[cfg(feature = "png")]
        Format::Png => super::png::xmp(data),
        #[cfg(feature = "webp")]
        Format::WebP => Ok(super::webp::chunk_data(data, b"XMP ")?.map(Cow::Borrowed)),
        #[cfg(feature = "jxl")]
        Format::Jxl => super::jxl::xmp(data),
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
        Format::Jpeg => Ok(super::jpeg::icc(data)?.map(Cow::Owned)),
        #[cfg(feature = "tiff")]
        Format::Tiff => Ok(tiff_tag(data, 0x8773)),
        #[cfg(feature = "png")]
        Format::Png => Ok(super::png::icc(data)?.map(Cow::Owned)),
        #[cfg(feature = "webp")]
        Format::WebP => Ok(super::webp::chunk_data(data, b"ICCP")?.map(Cow::Borrowed)),
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
fn tiff_tag(data: &[u8], tag: u16) -> Option<Cow<'_, [u8]>> {
    let header = crate::tiff::Header::parse(data).ok()?;
    let reader = crate::tiff::Reader::new(data, header.byte_order);
    let ifd0 = crate::tiff::RawIfd::parse(reader, header.ifd0_offset)?;
    ifd0.entries().find(|e| e.tag == tag)?.data().ok()
}

#[cfg(test)]
#[cfg(feature = "tiff")]
mod tests {
    use super::*;

    #[test]
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
