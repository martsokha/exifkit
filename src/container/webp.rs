//! WebP: Exif lives in the `EXIF` chunk of the RIFF container.

use alloc::vec::Vec;
use core::ops::Range;

use super::{Input, truncated};
use crate::diagnostic::{Diagnostic, DiagnosticKind, Diagnostics};
use crate::error::{Error, Result};

/// Finds the payload of the first `EXIF` chunk.
pub(super) fn locate(
    input: &mut impl Input,
    diagnostics: &mut Diagnostics,
) -> Result<Option<Range<u64>>> {
    let Some(
        [
            b'R',
            b'I',
            b'F',
            b'F',
            s0,
            s1,
            s2,
            s3,
            b'W',
            b'E',
            b'B',
            b'P',
        ],
    ) = input.array::<12>(0)?
    else {
        return Ok(None);
    };
    // The RIFF size counts from offset 8. Trust the smaller of it and the file.
    let mut end = 8u64.saturating_add(u64::from(u32::from_le_bytes([s0, s1, s2, s3])));
    if let Some(len) = input.len()?
        && len < end
    {
        truncated(diagnostics, len);
        end = len;
    }
    let mut at: u64 = 12;
    let mut found: Option<Range<u64>> = None;
    while at < end {
        let Some([f0, f1, f2, f3, l0, l1, l2, l3]) = input.array::<8>(at)? else {
            truncated(diagnostics, at);
            break;
        };
        let len = u64::from(u32::from_le_bytes([l0, l1, l2, l3]));
        let data = at.saturating_add(8);
        let data_end = data.saturating_add(len);
        if &[f0, f1, f2, f3] == b"EXIF" {
            let range = data..data_end.min(end);
            if data_end > end {
                truncated(diagnostics, at);
            }
            if found.is_none() {
                found = Some(range);
            } else {
                diagnostics.push(Diagnostic::new(DiagnosticKind::DuplicateExif).at(at));
            }
        }
        // Chunks are padded to an even length.
        at = data_end.saturating_add(len & 1);
    }
    Ok(found)
}

/// `VP8X` flag bits.
const FLAG_EXIF: u8 = 0x08;
const FLAG_ALPHA: u8 = 0x10;

/// A RIFF chunk as byte offsets: `start..end` covers the header, data and
/// padding byte.
struct Span {
    fourcc: [u8; 4],
    start: usize,
    data: Range<usize>,
    end: usize,
}

fn malformed(offset: usize, reason: &'static str) -> Error {
    Error::Malformed {
        offset: u64::try_from(offset).unwrap_or(u64::MAX),
        reason,
    }
}

/// Splits a WebP into its chunks. Returns them and where the bytes after
/// the RIFF payload (copied verbatim) start.
fn spans(data: &[u8]) -> Result<(Vec<Span>, usize)> {
    let Some(
        &[
            b'R',
            b'I',
            b'F',
            b'F',
            s0,
            s1,
            s2,
            s3,
            b'W',
            b'E',
            b'B',
            b'P',
        ],
    ) = data.get(..12)
    else {
        return Err(malformed(0, "not a WebP"));
    };
    let riff_end = usize::try_from(u32::from_le_bytes([s0, s1, s2, s3]))
        .unwrap_or(usize::MAX)
        .saturating_add(8);
    if riff_end > data.len() {
        return Err(malformed(4, "truncated WebP"));
    }
    let mut spans = Vec::new();
    let mut at = 12usize;
    while at < riff_end {
        let Some(&[f0, f1, f2, f3, l0, l1, l2, l3]) = data.get(at..at.saturating_add(8)) else {
            return Err(malformed(at, "truncated WebP chunk"));
        };
        let len = usize::try_from(u32::from_le_bytes([l0, l1, l2, l3])).unwrap_or(usize::MAX);
        let data_start = at.saturating_add(8);
        let data_end = data_start.saturating_add(len);
        let end = data_end.saturating_add(len & 1).min(riff_end);
        if data_end > riff_end {
            return Err(malformed(at, "truncated WebP chunk"));
        }
        spans.push(Span {
            fourcc: [f0, f1, f2, f3],
            start: at,
            data: data_start..data_end,
            end,
        });
        at = end.max(data_end);
    }
    Ok((spans, riff_end))
}

/// Canvas size and alpha of a simple-format image, from its bitstream.
fn canvas(data: &[u8], span: &Span) -> Option<(u32, u32, bool)> {
    let bits = data.get(span.data.clone())?;
    match &span.fourcc {
        // Lossy: 3-byte frame tag (bit 0 clear on a key frame), start code
        // 9D 01 2A, then 14-bit width and height.
        b"VP8 " => {
            let &[tag, _, _, 0x9D, 0x01, 0x2A, w0, w1, h0, h1, ..] = bits else {
                return None;
            };
            if tag & 1 != 0 {
                return None;
            }
            let width = u32::from(u16::from_le_bytes([w0, w1]) & 0x3FFF);
            let height = u32::from(u16::from_le_bytes([h0, h1]) & 0x3FFF);
            Some((width, height, false))
        }
        // Lossless: signature 0x2F, then width-1 and height-1 (14 bits each)
        // and the alpha hint.
        b"VP8L" => {
            let &[0x2F, b0, b1, b2, b3, ..] = bits else {
                return None;
            };
            let packed = u32::from_le_bytes([b0, b1, b2, b3]);
            let width = (packed & 0x3FFF).saturating_add(1);
            let height = ((packed >> 14) & 0x3FFF).saturating_add(1);
            Some((width, height, packed >> 28 & 1 == 1))
        }
        _ => None,
    }
}

fn chunk(fourcc: &[u8; 4], payload: &[u8]) -> Result<Vec<u8>> {
    let len = u32::try_from(payload.len()).map_err(|_| Error::TooLarge {
        size: payload.len(),
        max: u32::MAX as usize,
    })?;
    let mut out = Vec::with_capacity(payload.len().saturating_add(9));
    out.extend_from_slice(fourcc);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
    Ok(out)
}

/// Appends a chunk at an even offset. Only the file's last chunk can lack
/// its padding byte (some writers drop it), so pad before writing after it.
fn push_chunk(body: &mut Vec<u8>, chunk: &[u8]) {
    if body.len() % 2 == 1 {
        body.push(0);
    }
    body.extend_from_slice(chunk);
}

/// Replaces the `EXIF` chunk with one holding `tiff`, or removes it when
/// `tiff` is `None`, keeping the `VP8X` EXIF flag in step. A simple-format
/// file (a bare `VP8 ` or `VP8L` chunk) gains a `VP8X` header with the
/// canvas size read from the bitstream, since only the extended format can
/// carry Exif. A new `EXIF` chunk goes before `XMP `, or last. Everything
/// else is copied verbatim.
pub(super) fn splice(data: &[u8], tiff: Option<&[u8]>) -> Result<Vec<u8>> {
    let (spans, rest) = spans(data)?;
    let new_chunk = tiff.map(|t| chunk(b"EXIF", t)).transpose()?;
    let target = spans.iter().position(|s| &s.fourcc == b"EXIF");
    let has_vp8x = spans.iter().any(|s| &s.fourcc == b"VP8X");

    // A simple file has to become extended to carry Exif.
    let new_vp8x = match (&new_chunk, has_vp8x) {
        (Some(_), false) => {
            // A simple file starts with its bitstream chunk. Anything else
            // without VP8X is not a layout we can extend safely.
            let (width, height, alpha) =
                spans
                    .first()
                    .and_then(|s| canvas(data, s))
                    .ok_or(Error::Unsupported {
                        what: "WebP without VP8X whose first chunk is not an image bitstream with a canvas size",
                    })?;
            let mut payload = alloc::vec![0u8; 10];
            let flags = FLAG_EXIF | if alpha { FLAG_ALPHA } else { 0 };
            let w = width.saturating_sub(1).to_le_bytes();
            let h = height.saturating_sub(1).to_le_bytes();
            payload.copy_from_slice(&[flags, 0, 0, 0, w[0], w[1], w[2], h[0], h[1], h[2]]);
            Some(chunk(b"VP8X", &payload)?)
        }
        _ => None,
    };
    let insert_at = match (target, &new_chunk) {
        (None, Some(_)) => Some(
            spans
                .iter()
                .position(|s| &s.fourcc == b"XMP ")
                .unwrap_or(spans.len()),
        ),
        _ => None,
    };

    let mut body = Vec::with_capacity(
        data.len()
            .saturating_add(new_chunk.as_ref().map_or(0, Vec::len)),
    );
    if let Some(vp8x) = &new_vp8x {
        body.extend_from_slice(vp8x);
    }
    for (index, span) in spans.iter().enumerate() {
        if Some(index) == insert_at
            && let Some(new) = &new_chunk
        {
            push_chunk(&mut body, new);
        }
        if Some(index) == target {
            if let Some(new) = &new_chunk {
                push_chunk(&mut body, new);
            }
            continue;
        }
        if &span.fourcc == b"EXIF" {
            continue;
        }
        let original = data.get(span.start..span.end).unwrap_or_default();
        if &span.fourcc == b"VP8X" {
            let mut vp8x = original.to_vec();
            if let Some(flags) = vp8x.get_mut(8) {
                if new_chunk.is_some() {
                    *flags |= FLAG_EXIF;
                } else {
                    *flags &= !FLAG_EXIF;
                }
            }
            body.extend_from_slice(&vp8x);
        } else {
            body.extend_from_slice(original);
        }
    }
    if insert_at == Some(spans.len())
        && let Some(new) = &new_chunk
    {
        push_chunk(&mut body, new);
    }

    let riff_size = u32::try_from(body.len().saturating_add(4)).map_err(|_| Error::TooLarge {
        size: body.len(),
        max: u32::MAX as usize,
    })?;
    let mut out = Vec::with_capacity(body.len().saturating_add(12));
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&body);
    out.extend_from_slice(data.get(rest..).unwrap_or_default());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::container::Slice;

    fn chunk(fourcc: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = fourcc.to_vec();
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    fn webp(chunks: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = chunks.iter().flatten().copied().collect();
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&u32::try_from(body.len() + 4).unwrap().to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend(body);
        out
    }

    fn find(data: &[u8]) -> (Option<Range<u64>>, Diagnostics) {
        let mut diagnostics = Diagnostics::default();
        let found = locate(&mut Slice(data), &mut diagnostics).unwrap();
        (found, diagnostics)
    }

    #[test]
    fn finds_exif_after_odd_sized_chunks() {
        let data = webp(&[
            chunk(b"VP8X", &[0; 10]),
            chunk(b"VP8L", &[1, 2, 3]),
            chunk(b"EXIF", b"II*\0\x08\0\0\0"),
        ]);
        let (found, diagnostics) = find(&data);
        let range = found.unwrap();
        assert_eq!(
            &data[range.start as usize..range.end as usize],
            b"II*\0\x08\0\0\0"
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn riff_size_beyond_file_is_truncation() {
        let mut data = webp(&[chunk(b"EXIF", b"II*\0\x08\0\0\0")]);
        data[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        let (found, diagnostics) = find(&data);
        assert!(found.is_some());
        assert!(diagnostics.contains(DiagnosticKind::TruncatedContainer));
    }

    #[test]
    fn huge_chunk_length_terminates() {
        let mut data = webp(&[chunk(b"VP8 ", &[0; 4])]);
        data[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(find(&data).0, None);
    }
}

#[cfg(test)]
mod splice_tests {
    use alloc::vec::Vec;

    use super::*;

    const TIFF: &[u8] = b"II*\0\x08\0\0\0\0\0\0\0\0\0";

    fn raw(fourcc: &[u8; 4], data: &[u8]) -> Vec<u8> {
        chunk(fourcc, data).unwrap()
    }

    fn webp(chunks: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = chunks.iter().flatten().copied().collect();
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&u32::try_from(body.len() + 4).unwrap().to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend(body);
        out
    }

    /// A lossy key frame header for a `w`×`h` image.
    fn vp8(w: u16, h: u16) -> Vec<u8> {
        let mut data = vec![0x10, 0, 0, 0x9D, 0x01, 0x2A];
        data.extend_from_slice(&w.to_le_bytes());
        data.extend_from_slice(&h.to_le_bytes());
        data.extend_from_slice(&[0; 5]);
        raw(b"VP8 ", &data)
    }

    /// A lossless header for a `w`×`h` image, with the alpha hint.
    fn vp8l(w: u32, h: u32, alpha: bool) -> Vec<u8> {
        let packed = (w - 1) | ((h - 1) << 14) | (u32::from(alpha) << 28);
        let mut data = vec![0x2F];
        data.extend_from_slice(&packed.to_le_bytes());
        raw(b"VP8L", &data)
    }

    fn vp8x(flags: u8, w: u32, h: u32) -> Vec<u8> {
        let w = (w - 1).to_le_bytes();
        let h = (h - 1).to_le_bytes();
        raw(
            b"VP8X",
            &[flags, 0, 0, 0, w[0], w[1], w[2], h[0], h[1], h[2]],
        )
    }

    use alloc::vec;

    #[test]
    fn simple_lossy_becomes_extended() {
        let out = splice(&webp(&[vp8(640, 480)]), Some(TIFF)).unwrap();
        assert_eq!(
            out,
            webp(&[vp8x(FLAG_EXIF, 640, 480), vp8(640, 480), raw(b"EXIF", TIFF)])
        );
    }

    #[test]
    fn simple_lossless_keeps_its_alpha() {
        let out = splice(&webp(&[vp8l(3, 5, true)]), Some(TIFF)).unwrap();
        assert_eq!(
            out,
            webp(&[
                vp8x(FLAG_EXIF | FLAG_ALPHA, 3, 5),
                vp8l(3, 5, true),
                raw(b"EXIF", TIFF)
            ])
        );
    }

    #[test]
    fn exif_goes_before_xmp_and_flag_follows() {
        let xmp = raw(b"XMP ", b"<x/>");
        let original = webp(&[vp8x(0x04, 8, 8), vp8l(8, 8, false), xmp.clone()]);
        let out = splice(&original, Some(TIFF)).unwrap();
        assert_eq!(
            out,
            webp(&[
                vp8x(0x04 | FLAG_EXIF, 8, 8),
                vp8l(8, 8, false),
                raw(b"EXIF", TIFF),
                xmp.clone()
            ])
        );
        // Removing clears the flag and keeps the extended format.
        assert_eq!(
            splice(&out, None).unwrap(),
            webp(&[vp8x(0x04, 8, 8), vp8l(8, 8, false), xmp])
        );
    }

    #[test]
    fn odd_sized_exif_is_padded_and_riff_size_is_right() {
        let odd = &TIFF[..13];
        let out = splice(&webp(&[vp8x(0, 8, 8), vp8l(8, 8, false)]), Some(odd)).unwrap();
        let size = u32::from_le_bytes(out[4..8].try_into().unwrap());
        assert_eq!(size as usize, out.len() - 8);
        assert_eq!(out.len() % 2, 0);
    }

    #[test]
    fn unknown_first_chunk_is_not_converted() {
        // Found by fuzzing: a corrupt `VP6X` header in front of the bitstream.
        let junk = raw(b"VP6X", &[0; 10]);
        let original = webp(&[junk, vp8(8, 8), raw(b"EXIF", TIFF)]);
        assert!(matches!(
            splice(&original, Some(TIFF)),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn exif_after_an_unpadded_last_chunk_starts_even() {
        // An odd-sized bitstream whose writer dropped the padding byte.
        let mut lossless = vp8l(8, 8, false);
        lossless.pop();
        let out = splice(&webp(&[vp8x(0, 8, 8), lossless]), Some(TIFF)).unwrap();
        let mut diagnostics = Diagnostics::default();
        let range = locate(&mut crate::container::Slice(&out), &mut diagnostics)
            .unwrap()
            .unwrap();
        assert_eq!(&out[range.start as usize..range.end as usize], TIFF);
        assert_eq!(out.len() % 2, 0);
    }

    #[test]
    fn no_canvas_no_conversion() {
        let original = webp(&[raw(b"VP8 ", &[1; 10])]);
        assert!(matches!(
            splice(&original, Some(TIFF)),
            Err(Error::Unsupported { .. })
        ));
    }
}
