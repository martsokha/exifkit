//! WebP: Exif lives in the `EXIF` chunk of the RIFF container.

use alloc::vec::Vec;
use core::ops::Range;

use super::pieces::Pieces;
use super::{Input, malformed, truncated};
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
    start: u64,
    data: Range<u64>,
    end: u64,
}

/// Splits a WebP into its chunks, reading only chunk headers. Returns them
/// and where the bytes after the RIFF payload (copied verbatim) start.
fn spans(input: &mut impl Input, len: u64) -> Result<(Vec<Span>, u64)> {
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
        return Err(malformed(0, "not a WebP"));
    };
    let riff_end = u64::from(u32::from_le_bytes([s0, s1, s2, s3])).saturating_add(8);
    if riff_end > len {
        return Err(malformed(4, "truncated WebP"));
    }
    let mut spans = Vec::new();
    let mut at = 12u64;
    while at < riff_end {
        let Some([f0, f1, f2, f3, l0, l1, l2, l3]) = input.array::<8>(at)? else {
            return Err(malformed(at, "truncated WebP chunk"));
        };
        let length = u64::from(u32::from_le_bytes([l0, l1, l2, l3]));
        let data_start = at.saturating_add(8);
        let data_end = data_start.saturating_add(length);
        let end = data_end.saturating_add(length & 1).min(riff_end);
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

/// Canvas size and alpha of a simple-format image, from the first bytes of
/// its bitstream.
fn canvas(input: &mut impl Input, span: &Span) -> Result<Option<(u32, u32, bool)>> {
    Ok(match &span.fourcc {
        // Lossy: 3-byte frame tag (bit 0 clear on a key frame), start code
        // 9D 01 2A, then 14-bit width and height.
        b"VP8 " => match input.array::<10>(span.data.start)? {
            Some([tag, _, _, 0x9D, 0x01, 0x2A, w0, w1, h0, h1])
                if tag & 1 == 0 && span.data.end.saturating_sub(span.data.start) >= 10 =>
            {
                let width = u32::from(u16::from_le_bytes([w0, w1]) & 0x3FFF);
                let height = u32::from(u16::from_le_bytes([h0, h1]) & 0x3FFF);
                Some((width, height, false))
            }
            _ => None,
        },
        // Lossless: signature 0x2F, then width-1 and height-1 (14 bits each)
        // and the alpha hint.
        b"VP8L" => match input.array::<5>(span.data.start)? {
            Some([0x2F, b0, b1, b2, b3]) if span.data.end.saturating_sub(span.data.start) >= 5 => {
                let packed = u32::from_le_bytes([b0, b1, b2, b3]);
                let width = (packed & 0x3FFF).saturating_add(1);
                let height = ((packed >> 14) & 0x3FFF).saturating_add(1);
                Some((width, height, packed >> 28 & 1 == 1))
            }
            _ => None,
        },
        _ => None,
    })
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

/// The data of the first chunk with this FourCC (`XMP `, `ICCP`).
pub(super) fn chunk_data<'a>(data: &'a [u8], fourcc: &[u8; 4]) -> Result<Option<&'a [u8]>> {
    let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
    let (spans, _) = spans(&mut super::Slice(data), len)?;
    let index = |o: u64| usize::try_from(o).unwrap_or(usize::MAX);
    Ok(spans
        .iter()
        .find(|s| &s.fourcc == fourcc)
        .and_then(|s| data.get(index(s.data.start)..index(s.data.end))))
}

/// Appends a chunk at an even offset. Only the file's last chunk can lack
/// its padding byte (some writers drop it), so pad before writing after it.
fn push_chunk(body: &mut Pieces, chunk: &[u8]) {
    if body.len() % 2 == 1 {
        body.bytes([0]);
    }
    body.bytes(chunk);
}

/// Plans replacing the `EXIF` chunk with one holding `tiff`, or removing it
/// when `tiff` is `None`, keeping the `VP8X` EXIF flag in step. A
/// simple-format file (a bare `VP8 ` or `VP8L` chunk) gains a `VP8X`
/// header with the canvas size read from the bitstream, since only the
/// extended format can carry Exif. A new `EXIF` chunk goes before `XMP `,
/// or last. Everything else is copied verbatim. Only chunk headers, the
/// `VP8X` flags and the first bytes of the bitstream are read.
pub(super) fn plan(input: &mut impl Input, len: u64, tiff: Option<&[u8]>) -> Result<Pieces> {
    let (spans, rest) = spans(input, len)?;
    let new_chunk = tiff.map(|t| chunk(b"EXIF", t)).transpose()?;
    let target = spans.iter().position(|s| &s.fourcc == b"EXIF");
    let has_vp8x = spans.iter().any(|s| &s.fourcc == b"VP8X");

    // A simple file has to become extended to carry Exif.
    let new_vp8x = match (&new_chunk, has_vp8x) {
        (Some(_), false) => {
            // A simple file starts with its bitstream chunk. Anything else
            // without VP8X is not a layout we can extend safely.
            let first = match spans.first() {
                Some(span) => canvas(input, span)?,
                None => None,
            };
            let (width, height, alpha) = first.ok_or(Error::Unsupported {
                what: "WebP without VP8X whose first chunk is not an image bitstream with a canvas size",
            })?;
            let flags = FLAG_EXIF | if alpha { FLAG_ALPHA } else { 0 };
            let w = width.saturating_sub(1).to_le_bytes();
            let h = height.saturating_sub(1).to_le_bytes();
            Some(chunk(
                b"VP8X",
                &[flags, 0, 0, 0, w[0], w[1], w[2], h[0], h[1], h[2]],
            )?)
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

    let mut body = Pieces::default();
    if let Some(vp8x) = new_vp8x {
        body.bytes(vp8x);
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
        let flags_at = span.start.saturating_add(8);
        match input.array::<1>(flags_at)? {
            Some([flags]) if &span.fourcc == b"VP8X" && flags_at < span.end => {
                let flags = if new_chunk.is_some() {
                    flags | FLAG_EXIF
                } else {
                    flags & !FLAG_EXIF
                };
                body.copy(span.start..flags_at);
                body.bytes([flags]);
                body.copy(flags_at.saturating_add(1)..span.end);
            }
            _ => body.copy(span.start..span.end),
        }
    }
    if insert_at == Some(spans.len())
        && let Some(new) = &new_chunk
    {
        push_chunk(&mut body, new);
    }

    let riff_size = u32::try_from(body.len().saturating_add(4)).map_err(|_| Error::TooLarge {
        size: usize::try_from(body.len()).unwrap_or(usize::MAX),
        max: u32::MAX as usize,
    })?;
    let mut out = Pieces::default();
    let mut header = b"RIFF".to_vec();
    header.extend_from_slice(&riff_size.to_le_bytes());
    header.extend_from_slice(b"WEBP");
    out.bytes(header);
    out.extend(body);
    out.copy(rest..len);
    Ok(out)
}

#[cfg(test)]
mod tests;
