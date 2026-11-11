//! JPEG XL: Exif lives in an `Exif` box of the ISOBMFF-style container, or
//! Brotli-compressed in a `brob` box whose payload starts with `Exif`. A bare
//! codestream (`FF 0A`) has no boxes and so no Exif. See
//! `docs/containers.md#jpeg-xl`.

use alloc::borrow::Cow;
use alloc::vec::Vec;

use super::bmff::{self, BmffBox};
use super::pieces::Pieces;
use super::{Encoding, Format, Input, Located, truncated};
use crate::diagnostic::{Diagnostic, DiagnosticKind, Diagnostics};
use crate::error::{Error, Result};

/// The 12-byte signature box that starts the container form.
const SIGNATURE: [u8; 12] = *b"\0\0\0\x0CJXL \r\n\x87\n";
/// The start of a bare codestream.
const CODESTREAM: [u8; 2] = [0xFF, 0x0A];

/// What an Exif-carrying box holds.
enum Exif {
    /// An `Exif` box.
    Plain,
    /// A `brob` box wrapping an `Exif` box.
    Brotli,
}

fn exif_kind(input: &mut impl Input, b: &BmffBox) -> Result<Option<Exif>> {
    Ok(match &b.kind {
        b"Exif" => Some(Exif::Plain),
        b"brob" if b.len() >= 4 && input.array::<4>(b.data.start)? == Some(*b"Exif") => {
            Some(Exif::Brotli)
        }
        _ => None,
    })
}

fn is_codestream(b: &BmffBox) -> bool {
    &b.kind == b"jxlc" || &b.kind == b"jxlp"
}

/// Finds the Exif box. A bare codestream has none. A second Exif box is
/// reported as [`DiagnosticKind::DuplicateExif`].
pub(super) fn locate(input: &mut impl Input, diagnostics: &mut Diagnostics) -> Result<Located> {
    let located = |exif, encoding| Located {
        format: Format::Jxl,
        exif,
        encoding,
    };
    if input.array::<2>(0)? == Some(CODESTREAM) {
        return Ok(located(None, Encoding::Raw));
    }
    let len = input.len()?.unwrap_or(u64::MAX);
    let (boxes, broken) = bmff::boxes(input, 0..len)?;
    if let Some(at) = broken {
        truncated(diagnostics, at);
    }
    let mut found = None;
    for b in &boxes {
        let Some(kind) = exif_kind(input, b)? else {
            continue;
        };
        if found.is_some() {
            diagnostics.push(Diagnostic::new(DiagnosticKind::DuplicateExif).at(b.start));
            continue;
        }
        found = Some(match kind {
            Exif::Plain => match bmff::exif_tiff(input, b.data.clone())? {
                Some(tiff) => located(Some(tiff), Encoding::Raw),
                None => {
                    diagnostics
                        .push(Diagnostic::new(DiagnosticKind::InvalidExifOffset).at(b.start));
                    located(None, Encoding::Raw)
                }
            },
            // The compressed data after the 4-byte box type.
            Exif::Brotli => located(
                Some(b.data.start.saturating_add(4)..b.data.end),
                Encoding::Brotli,
            ),
        });
    }
    Ok(found.unwrap_or_else(|| located(None, Encoding::Raw)))
}

/// Decompresses a `brob` Exif payload and returns the TIFF block inside it.
pub(super) fn decode_brob(compressed: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut out = decompress(compressed, limit)?;
    // The decompressed `Exif` box payload: u32 offset, then the TIFF block.
    let len = u64::try_from(out.len()).unwrap_or(u64::MAX);
    let tiff = bmff::exif_tiff(&mut super::Slice(&out), 0..len)?.ok_or(Error::Malformed {
        offset: 0,
        reason: "Exif TIFF header offset out of bounds",
    })?;
    Ok(out.split_off(usize::try_from(tiff.start).unwrap_or(out.len())))
}

/// An `Exif` box holding `tiff`, with a zero TIFF header offset.
fn exif_box(tiff: &[u8]) -> Vec<u8> {
    let len = u64::try_from(tiff.len())
        .unwrap_or(u64::MAX)
        .saturating_add(4);
    let mut out = bmff::header(*b"Exif", len);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(tiff);
    out
}

/// The `ftyp` box of a JPEG XL container.
fn ftyp() -> Vec<u8> {
    let mut out = bmff::header(*b"ftyp", 12);
    out.extend_from_slice(b"jxl \0\0\0\0jxl ");
    out
}

/// Plans replacing the Exif box with one holding `tiff` (always as a plain
/// `Exif` box: a `brob` one is replaced too), or removing every Exif box when
/// `tiff` is `None`. A new box goes before the codestream. A bare codestream
/// is wrapped in a container (signature, `ftyp`, `Exif`, `jxlc`) to carry
/// Exif, as the format requires. Boxes do not reference each other, so
/// nothing else changes, except that a file with JPEG reconstruction data
/// keeps its Exif payload size (see [`reconstructable`]).
pub(super) fn plan(input: &mut impl Input, len: u64, tiff: Option<&[u8]>) -> Result<Pieces> {
    let mut out = Pieces::default();
    if input.array::<2>(0)? == Some(CODESTREAM) {
        if let Some(tiff) = tiff {
            out.bytes(SIGNATURE);
            out.bytes(ftyp());
            out.bytes(exif_box(tiff));
            out.bytes(bmff::header(*b"jxlc", len));
        }
        out.copy(0..len);
        return Ok(out);
    }
    if input.array::<12>(0)? != Some(SIGNATURE) {
        return Err(Error::Malformed {
            offset: 0,
            reason: "not a JPEG XL file",
        });
    }
    let (boxes, broken) = bmff::boxes(input, 0..len)?;
    if let Some(at) = broken {
        return Err(Error::Malformed {
            offset: at,
            reason: "truncated JPEG XL box",
        });
    }
    let mut exif = Vec::with_capacity(boxes.len());
    for b in &boxes {
        exif.push(exif_kind(input, b)?);
    }
    let target = exif.iter().position(Option::is_some);
    // With JPEG reconstruction data, the Exif payload must keep its size, or
    // the original JPEG can no longer be rebuilt (see `reconstructable`).
    let reconstruction = boxes.iter().any(|b| &b.kind == b"jbrd");
    let new_box = match (
        target.and_then(|t| Some((boxes.get(t)?, exif.get(t)?.as_ref()?))),
        reconstruction,
    ) {
        (Some((b, kind)), true) => {
            let size = payload_size(input, b, kind)?;
            Some(exif_box(&reconstructable(tiff, size)?))
        }
        _ => tiff.map(exif_box),
    };
    let insert_at = match (target, &new_box) {
        (None, Some(_)) => Some(boxes.iter().position(is_codestream).unwrap_or(boxes.len())),
        _ => None,
    };
    for (index, b) in boxes.iter().enumerate() {
        if Some(index) == insert_at
            && let Some(new) = &new_box
        {
            out.bytes(new.clone());
        }
        if Some(index) == target {
            if let Some(new) = &new_box {
                out.bytes(new.clone());
            }
            continue;
        }
        if exif.get(index).is_some_and(Option::is_some) {
            continue;
        }
        out.copy(b.start..b.end);
    }
    if insert_at == Some(boxes.len())
        && let Some(new) = &new_box
    {
        out.bytes(new.clone());
    }
    Ok(out)
}

/// The size of an Exif box's payload (the u32 offset and the TIFF block),
/// decompressed for a `brob` box.
fn payload_size(input: &mut impl Input, b: &BmffBox, kind: &Exif) -> Result<u64> {
    match kind {
        Exif::Plain => Ok(b.len()),
        Exif::Brotli => {
            let compressed =
                super::pieces::read_range(input, b.data.start.saturating_add(4)..b.data.end)?;
            let payload = decompress(&compressed, 64 * 1024 * 1024)?;
            Ok(u64::try_from(payload.len()).unwrap_or(u64::MAX))
        }
    }
}

/// The TIFF block to store in a file with JPEG reconstruction data (`jbrd`),
/// whose Exif payload was `size` bytes.
///
/// `djxl` rebuilds the original JPEG only if the Exif payload has exactly
/// the size the reconstruction data records (checked with libjxl: one byte
/// more or less and it falls back to decoding pixels). So the new block is
/// padded with zeros to that size, which a TIFF block allows, and the
/// rebuilt JPEG then carries the new Exif. Removing Exif writes an empty
/// TIFF block of that size instead of dropping the box.
///
/// # Errors
///
/// [`Error::TooLarge`] if the new block is bigger: storing it would lose
/// the lossless JPEG reconstruction.
fn reconstructable(tiff: Option<&[u8]>, size: u64) -> Result<Vec<u8>> {
    /// A little-endian TIFF header and an empty IFD0.
    const EMPTY: &[u8] = b"II*\0\x08\0\0\0\0\0\0\0\0\0";
    let max = usize::try_from(size.saturating_sub(4)).unwrap_or(usize::MAX);
    let tiff = tiff.unwrap_or(EMPTY);
    if tiff.len() > max {
        return Err(Error::TooLarge {
            size: tiff.len(),
            max,
        });
    }
    let mut padded = tiff.to_vec();
    padded.resize(max, 0);
    Ok(padded)
}

/// The XMP packet: the first `xml ` box. A Brotli-compressed one (`brob`
/// wrapping `xml `) is decompressed with `jxl-brob`.
pub(super) fn xmp(data: &[u8]) -> Result<Option<Cow<'_, [u8]>>> {
    if data.starts_with(&CODESTREAM) {
        return Ok(None);
    }
    let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
    let (boxes, _) = bmff::boxes(&mut super::Slice(data), 0..len)?;
    for b in &boxes {
        let payload = b.payload(data);
        match (&b.kind, payload.strip_prefix(b"xml ")) {
            (b"xml ", _) => return Ok(Some(Cow::Borrowed(payload))),
            (b"brob", Some(compressed)) => {
                return decompress(compressed, 64 * 1024 * 1024).map(|x| Some(Cow::Owned(x)));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// Decompresses a `brob` payload (after its box type), failing past `limit`
/// output bytes. Decompression is streamed, so the cap holds against a
/// Brotli bomb.
fn decompress(compressed: &[u8], limit: usize) -> Result<Vec<u8>> {
    #[cfg(feature = "jxl-brob")]
    {
        use std::io::Read;

        let limit = u64::try_from(limit).unwrap_or(u64::MAX);
        let mut out = Vec::new();
        brotli_decompressor::Decompressor::new(compressed, 4096)
            .take(limit.saturating_add(1))
            .read_to_end(&mut out)
            .map_err(|_| Error::Malformed {
                offset: 0,
                reason: "invalid Brotli data in JPEG XL brob box",
            })?;
        if u64::try_from(out.len()).unwrap_or(u64::MAX) > limit {
            return Err(Error::LimitExceeded(crate::limits::Limit::Alloc));
        }
        Ok(out)
    }
    #[cfg(not(feature = "jxl-brob"))]
    {
        let _ = (compressed, limit);
        Err(Error::Unsupported {
            what: "Brotli-compressed JPEG XL box (enable the `jxl-brob` feature)",
        })
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::container::Slice;

    const TIFF: &[u8] = b"II*\0\x08\0\0\0\0\0\0\0\0\0";
    /// `brotli -q 11` of `00000000` + [`TIFF`]: a compressed Exif payload.
    const BROTLI_EXIF: &[u8] = &[
        0xa1, 0x88, 0x00, 0xc0, 0x3f, 0x0d, 0x90, 0xa4, 0x82, 0x30, 0x0c, 0x0a, 0x04, 0x80, 0xba,
        0x01,
    ];

    fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = bmff::header(*kind, payload.len() as u64);
        out.extend_from_slice(payload);
        out
    }

    fn exif(offset: u32, rest: &[u8]) -> Vec<u8> {
        bx(b"Exif", &[&offset.to_be_bytes()[..], rest].concat())
    }

    fn jxl(boxes: &[Vec<u8>]) -> Vec<u8> {
        let mut out = SIGNATURE.to_vec();
        out.extend(ftyp());
        for b in boxes {
            out.extend_from_slice(b);
        }
        out
    }

    fn find(data: &[u8]) -> (Located, Diagnostics) {
        let mut diagnostics = Diagnostics::default();
        let located = locate(&mut Slice(data), &mut diagnostics).unwrap();
        (located, diagnostics)
    }

    fn splice(data: &[u8], tiff: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut input = Slice(data);
        plan(&mut input, data.len() as u64, tiff)?.to_vec(&mut input)
    }

    fn tiff_at(data: &[u8], located: &Located) -> Vec<u8> {
        let range = located.exif.clone().unwrap();
        data[range.start as usize..range.end as usize].to_vec()
    }

    #[test]
    fn locates_exif_after_its_offset() {
        let codestream = bx(b"jxlc", &[0xFF, 0x0A, 1, 2]);
        let data = jxl(&[exif(0, TIFF), codestream.clone()]);
        let (located, diagnostics) = find(&data);
        assert_eq!(tiff_at(&data, &located), TIFF);
        assert!(diagnostics.is_empty());
        // An offset skips a header some writers add.
        let data = jxl(&[exif(6, &[b"Exif\0\0", TIFF].concat()), codestream.clone()]);
        assert_eq!(tiff_at(&data, &find(&data).0), TIFF);
        // An offset past the end is reported, not followed.
        let data = jxl(&[exif(99, TIFF), codestream.clone()]);
        let (located, diagnostics) = find(&data);
        assert_eq!(located.exif, None);
        assert!(diagnostics.contains(DiagnosticKind::InvalidExifOffset));
        // A second Exif box is reported.
        let data = jxl(&[exif(0, TIFF), exif(0, TIFF), codestream]);
        assert!(find(&data).1.contains(DiagnosticKind::DuplicateExif));
        // A bare codestream has no Exif.
        assert_eq!(find(&[0xFF, 0x0A, 0, 0]).0.exif, None);
    }

    #[test]
    fn locates_and_decodes_brotli_exif() {
        let brob = bx(b"brob", &[b"Exif", BROTLI_EXIF].concat());
        let data = jxl(&[brob, bx(b"jxlc", &[0xFF, 0x0A])]);
        let (located, _) = find(&data);
        assert_eq!(located.encoding, Encoding::Brotli);
        let compressed = tiff_at(&data, &located);
        #[cfg(feature = "jxl-brob")]
        assert_eq!(decode_brob(&compressed, 1 << 20).unwrap(), TIFF);
        #[cfg(not(feature = "jxl-brob"))]
        assert!(matches!(
            decode_brob(&compressed, 1 << 20),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    #[cfg(feature = "jxl-brob")]
    fn brotli_output_is_capped() {
        assert!(matches!(
            decode_brob(BROTLI_EXIF, 8),
            Err(Error::LimitExceeded(_))
        ));
        assert!(matches!(
            decode_brob(b"not brotli at all", 1 << 20),
            Err(Error::Malformed { .. })
        ));
    }

    #[test]
    fn replaces_inserts_and_removes() {
        let codestream = bx(b"jxlc", &[0xFF, 0x0A, 1, 2]);
        let xml = bx(b"xml ", b"<x/>");
        let other = b"MM\0*\0\0\0\x08\0\0\0\0\0\0";
        let data = jxl(&[
            xml.clone(),
            exif(6, &[b"Exif\0\0", TIFF].concat()),
            codestream.clone(),
        ]);
        assert_eq!(
            splice(&data, Some(other)).unwrap(),
            jxl(&[xml.clone(), exif(0, other), codestream.clone()])
        );
        // No Exif yet: before the codestream.
        let data = jxl(&[xml.clone(), codestream.clone()]);
        assert_eq!(
            splice(&data, Some(TIFF)).unwrap(),
            jxl(&[xml.clone(), exif(0, TIFF), codestream.clone()])
        );
        // Removing drops every Exif box, compressed or not.
        let brob = bx(b"brob", &[b"Exif", BROTLI_EXIF].concat());
        let data = jxl(&[exif(0, TIFF), xml.clone(), brob, codestream.clone()]);
        assert_eq!(splice(&data, None).unwrap(), jxl(&[xml, codestream]));
    }

    #[test]
    fn bare_codestream_is_wrapped() {
        let codestream = [0xFF, 0x0A, 9, 8, 7];
        let out = splice(&codestream, Some(TIFF)).unwrap();
        assert_eq!(out, jxl(&[exif(0, TIFF), bx(b"jxlc", &codestream)]));
        assert_eq!(tiff_at(&out, &find(&out).0), TIFF);
        // Nothing to remove from a bare codestream.
        assert_eq!(splice(&codestream, None).unwrap(), codestream);
    }

    #[test]
    fn reconstruction_data_pins_the_exif_size() {
        let jbrd = bx(b"jbrd", &[1, 2, 3]);
        let codestream = bx(b"jxlc", &[0xFF, 0x0A]);
        let original = [TIFF, &[0u8; 10]].concat();
        let data = jxl(&[jbrd.clone(), exif(0, &original), codestream.clone()]);
        // Smaller: padded back to the original size.
        let out = splice(&data, Some(TIFF)).unwrap();
        assert_eq!(
            out,
            jxl(&[jbrd.clone(), exif(0, &original), codestream.clone()])
        );
        // Bigger: refused rather than losing the reconstruction.
        let bigger = [TIFF, &[0u8; 11]].concat();
        assert!(matches!(
            splice(&data, Some(&bigger)),
            Err(Error::TooLarge { max: 24, .. })
        ));
        // Removing writes an empty TIFF block of the same size.
        let out = splice(&data, None).unwrap();
        assert_eq!(out.len(), data.len());
        let tiff = tiff_at(&out, &find(&out).0);
        let (exif, _) = crate::Exif::from_tiff(&tiff).unwrap();
        assert_eq!(exif.entries().count(), 0);
    }

    #[test]
    fn broken_files_are_refused() {
        let mut data = jxl(&[exif(0, TIFF)]);
        data.truncate(data.len() - 3);
        assert!(matches!(
            splice(&data, Some(TIFF)),
            Err(Error::Malformed { .. })
        ));
        assert!(find(&data).1.contains(DiagnosticKind::TruncatedContainer));
    }

    #[test]
    fn extracts_xmp() {
        let data = jxl(&[bx(b"xml ", b"<x/>"), bx(b"jxlc", &[0xFF, 0x0A])]);
        assert_eq!(xmp(&data).unwrap().as_deref(), Some(&b"<x/>"[..]));
        assert_eq!(xmp(&[0xFF, 0x0A]).unwrap(), None);
    }
}
