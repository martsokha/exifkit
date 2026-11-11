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
    tiff_of_payload(decompress(compressed, limit)?)
}

/// The TIFF block of a decompressed `Exif` box payload: a u32 offset, then
/// the TIFF block.
fn tiff_of_payload(mut out: Vec<u8>) -> Result<Vec<u8>> {
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
    // With JPEG reconstruction data that records an Exif segment, the Exif
    // payload must keep its size, or the original JPEG can no longer be
    // rebuilt (see `reconstructable`).
    let recorded = match boxes.iter().find(|b| &b.kind == b"jbrd") {
        Some(jbrd) => {
            let end = jbrd
                .data
                .end
                .min(jbrd.data.start.saturating_add(JBRD_HEADER_MAX));
            recorded(&super::pieces::read_range(input, jbrd.data.start..end)?)
        }
        None => Recorded::Nothing,
    };
    let existing = target.and_then(|t| Some((boxes.get(t)?, exif.get(t)?.as_ref()?)));
    let size = match (recorded, existing) {
        (Recorded::Exif(size), _) => Some(size),
        // Unreadable: keep the box's current size, the safe choice. Only
        // here does a `brob` box need decompressing.
        (Recorded::Unreadable, Some((b, kind))) => Some(payload_size(input, b, kind)?),
        _ => None,
    };
    let new_box = match size {
        // Even with no Exif box left (another tool dropped it), padding a
        // new one to the recorded size makes the reconstruction work again.
        Some(size) if tiff.is_some() || existing.is_some() => {
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

/// Enough of a `jbrd` box for its header: at most 16384 markers of 6 bits,
/// then for each APP marker a type and a 16-bit length.
const JBRD_HEADER_MAX: u64 = 64 * 1024;

/// What JPEG reconstruction data (a `jbrd` box) says about Exif.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recorded {
    /// No Exif APP segment: the Exif box is not part of the reconstruction
    /// and may change size freely (a transcode of a JPEG without Exif).
    Nothing,
    /// An Exif APP segment: `djxl` rebuilds the JPEG only if the Exif box
    /// payload has exactly this size.
    Exif(u64),
    /// The header could not be read.
    Unreadable,
}

/// Reads what JPEG reconstruction data records about Exif.
///
/// Reads the start of libjxl's `JPEGData` fields (`lib/jxl/jpeg/jpeg_data.cc`,
/// bits read LSB first): the gray flag, the marker list as 6-bit
/// `marker - 0xC0` values up to EOI, then each APP segment's type (a U32
/// with selectors 0, 1, 2 + 1 bit, 4 + 2 bits; Exif is 2) and 16-bit
/// `size - 1`. The segment size counts the marker byte, the two length
/// bytes and `Exif\0\0`; the box payload drops those 9 bytes and adds the
/// 4-byte offset, so it is the stored value minus 4 (libjxl's
/// `ExifBoxContentSize`).
fn recorded(jbrd: &[u8]) -> Recorded {
    let mut bits = Bits { data: jbrd, at: 0 };
    let parsed = (|| {
        bits.read(1)?; // is_gray
        let mut apps = 0usize;
        for _ in 0..16384 {
            let marker = bits.read(6)?.saturating_add(0xC0);
            if marker & 0xF0 == 0xE0 {
                apps = apps.saturating_add(1);
            }
            if marker == 0xD9 {
                for _ in 0..apps {
                    let kind = match bits.read(2)? {
                        0 => 0,
                        1 => 1,
                        2 => bits.read(1)?.saturating_add(2),
                        _ => bits.read(2)?.saturating_add(4),
                    };
                    let stored = bits.read(16)?;
                    if kind == 2 {
                        // Shorter than its own header: libjxl rejects it too.
                        return Some(match stored.checked_sub(4) {
                            Some(size) if stored >= 9 => Recorded::Exif(u64::from(size)),
                            _ => Recorded::Unreadable,
                        });
                    }
                }
                return Some(Recorded::Nothing);
            }
        }
        None
    })();
    parsed.unwrap_or(Recorded::Unreadable)
}

/// An LSB-first bit reader, as JPEG XL headers use.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: usize) -> Option<u32> {
        let mut value = 0u32;
        for i in 0..n {
            let byte = *self.data.get(self.at / 8)?;
            let bit = (byte >> (self.at % 8)) & 1;
            value |= u32::from(bit) << i;
            self.at = self.at.checked_add(1)?;
        }
        Some(value)
    }
}

/// The size of an Exif box's payload (the u32 offset and the TIFF block),
/// decompressed for a `brob` box.
fn payload_size(input: &mut impl Input, b: &BmffBox, kind: &Exif) -> Result<u64> {
    match kind {
        Exif::Plain => Ok(b.len()),
        Exif::Brotli => {
            let compressed =
                super::pieces::read_range(input, b.data.start.saturating_add(4)..b.data.end)?;
            let payload = decompress(&compressed, super::METADATA_LIMIT)?;
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
                return decompress(compressed, super::METADATA_LIMIT).map(|x| Some(Cow::Owned(x)));
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
    return decompress_from(compressed, limit);
    #[cfg(not(feature = "jxl-brob"))]
    {
        let _ = (compressed, limit);
        Err(Error::Unsupported {
            what: "Brotli-compressed JPEG XL box (enable the `jxl-brob` feature)",
        })
    }
}

/// Decompresses Brotli data read from `reader`, failing past `limit` output
/// bytes. Both sides are streamed: neither the compressed input nor more
/// than `limit + 1` output bytes are ever held whole.
#[cfg(feature = "jxl-brob")]
fn decompress_from(reader: impl std::io::Read, limit: usize) -> Result<Vec<u8>> {
    use std::io::Read;

    let limit = u64::try_from(limit).unwrap_or(u64::MAX);
    let mut out = Vec::new();
    brotli_decompressor::Decompressor::new(reader, 4096)
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

/// A byte range of an [`Input`] as [`std::io::Read`].
#[cfg(feature = "jxl-brob")]
struct RangeReader<'a, I> {
    input: &'a mut I,
    at: u64,
    end: u64,
}

#[cfg(feature = "jxl-brob")]
impl<I: Input> std::io::Read for RangeReader<'_, I> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = usize::try_from(self.end.saturating_sub(self.at)).unwrap_or(usize::MAX);
        let n = buf.len().min(left);
        let Some(chunk) = buf.get_mut(..n).filter(|c| !c.is_empty()) else {
            return Ok(0);
        };
        match self.input.read_at(self.at, chunk) {
            Ok(true) => {
                self.at = self.at.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
                Ok(n)
            }
            Ok(false) => Err(std::io::ErrorKind::UnexpectedEof.into()),
            Err(err) => Err(std::io::Error::other(err)),
        }
    }
}

/// [`decode_brob`] reading the compressed data from `range` of `input` as it
/// decompresses, so the compressed box is never held in memory.
#[cfg(feature = "jxl-brob")]
pub(super) fn decode_brob_in(
    input: &mut impl Input,
    range: core::ops::Range<u64>,
    limit: usize,
) -> Result<Vec<u8>> {
    let reader = RangeReader {
        input,
        at: range.start,
        end: range.end,
    };
    tiff_of_payload(decompress_from(reader, limit)?)
}

#[cfg(test)]
mod tests {
    use alloc::vec;
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
    fn streamed_brotli_matches_in_memory() {
        let data = [b"junk", BROTLI_EXIF, b"more"].concat();
        let range = 4..4 + BROTLI_EXIF.len() as u64;
        assert_eq!(
            decode_brob_in(&mut Slice(&data), range.clone(), 1 << 20).unwrap(),
            TIFF
        );
        // The output cap holds when streaming too.
        assert!(matches!(
            decode_brob_in(&mut Slice(&data), range, 8),
            Err(Error::LimitExceeded(_))
        ));
        // A range past the input is an error, not a hang or a panic.
        assert!(decode_brob_in(&mut Slice(&data), 4..1 << 40, 1 << 20).is_err());
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
        let original = [TIFF, &[0u8; 10]].concat();
        let jbrd = bx(b"jbrd", &jbrd_header(&[2], 4 + original.len() as u32));
        let codestream = bx(b"jxlc", &[0xFF, 0x0A]);
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
    fn recorded_size_needs_no_decompression() {
        // Found in review: removing a `brob` Exif that the reconstruction
        // records must not need `jxl-brob`; the size comes from `jbrd`.
        let jbrd = bx(b"jbrd", &jbrd_header(&[2], 4 + TIFF.len() as u32));
        let brob = bx(b"brob", &[b"Exif", BROTLI_EXIF].concat());
        let codestream = bx(b"jxlc", &[0xFF, 0x0A]);
        let data = jxl(&[jbrd.clone(), brob, codestream.clone()]);
        let out = splice(&data, None).unwrap();
        assert_eq!(out, jxl(&[jbrd.clone(), exif(0, TIFF), codestream.clone()]));
        // With the Exif box gone (another tool dropped it), a new one is
        // padded to the recorded size, so the reconstruction works again.
        let data = jxl(&[jbrd.clone(), codestream.clone()]);
        let small = &TIFF[..8];
        let padded = [small, &[0u8; 6]].concat();
        assert_eq!(
            splice(&data, Some(small)).unwrap(),
            jxl(&[jbrd, exif(0, &padded), codestream])
        );
    }

    /// Packs `(value, bits)` fields LSB first, as JPEG XL headers are.
    fn pack(fields: &[(u32, usize)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut at = 0usize;
        for &(value, n) in fields {
            for i in 0..n {
                if at / 8 == out.len() {
                    out.push(0);
                }
                out[at / 8] |= (((value >> i) & 1) as u8) << (at % 8);
                at += 1;
            }
        }
        out
    }

    /// A `jbrd` header: SOI, the given APP segment types, then EOI. An Exif
    /// segment records an Exif box payload of `exif` bytes; others store 99.
    fn jbrd_header(apps: &[u32], exif: u32) -> Vec<u8> {
        let mut fields = vec![(0, 1), (0xD8 - 0xC0, 6)];
        fields.extend(apps.iter().map(|_| (0xE1 - 0xC0, 6)));
        fields.push((0xD9 - 0xC0, 6));
        for &kind in apps {
            match kind {
                0 | 1 => fields.push((kind, 2)),
                2 | 3 => fields.extend([(2, 2), (kind - 2, 1)]),
                _ => fields.extend([(3, 2), (kind - 4, 2)]),
            }
            fields.push((if kind == 2 { exif + 4 } else { 99 }, 16));
        }
        pack(&fields)
    }

    #[test]
    fn reads_what_reconstruction_records_about_exif() {
        assert_eq!(recorded(&jbrd_header(&[2], 95)), Recorded::Exif(95));
        assert_eq!(
            recorded(&jbrd_header(&[1, 3, 2], 2468)),
            Recorded::Exif(2468)
        );
        assert_eq!(recorded(&jbrd_header(&[], 0)), Recorded::Nothing);
        assert_eq!(recorded(&jbrd_header(&[0, 1, 3], 0)), Recorded::Nothing);
        assert_eq!(recorded(&[]), Recorded::Unreadable);
        assert_eq!(
            recorded(&jbrd_header(&[1, 2], 95)[..2]),
            Recorded::Unreadable
        );
        // A segment shorter than its own header, as libjxl rejects.
        assert_eq!(recorded(&jbrd_header(&[2], 0)), Recorded::Unreadable);
    }

    #[test]
    fn exif_added_to_a_transcode_without_exif_can_grow() {
        // Found in review: Exif added after transcoding is not part of the
        // reconstruction, so a later, bigger edit must not be refused.
        let jbrd = bx(b"jbrd", &jbrd_header(&[1], 0));
        let codestream = bx(b"jxlc", &[0xFF, 0x0A]);
        let data = jxl(&[jbrd.clone(), exif(0, TIFF), codestream.clone()]);
        let bigger = [TIFF, &[0u8; 11]].concat();
        assert_eq!(
            splice(&data, Some(&bigger)).unwrap(),
            jxl(&[jbrd, exif(0, &bigger), codestream])
        );
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
