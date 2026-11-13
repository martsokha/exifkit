//! PNG: Exif lives in the `eXIf` chunk (PNG 3rd edition), or in older files
//! in a "Raw profile type exif" text chunk written by ImageMagick.

use alloc::vec::Vec;
use core::ops::Range;

use super::pieces::Pieces;
use super::{Encoding, Format, Input, Located, malformed, truncated};
use crate::diagnostic::{Diagnostic, DiagnosticKind, Diagnostics};
use crate::error::{Error, Result};

const SIGNATURE: [u8; 8] = *b"\x89PNG\r\n\x1a\n";
/// Largest chunk length the PNG spec allows.
const MAX_CHUNK: u32 = 0x7FFF_FFFF;
/// Text-chunk keywords that carry Exif as a hex "raw profile".
const RAW_PROFILE_KEYWORDS: &[&[u8]] = &[b"Raw profile type exif", b"Raw profile type APP1"];

/// CRC-32 (ISO 3309), as PNG uses it. The table is built at compile time.
// Indexing runs during const evaluation: an out-of-range index would be a
// compile error, never a runtime panic.
#[allow(clippy::indexing_slicing)]
pub(crate) const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 == 1 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

/// Computes the CRC-32 of the given byte runs, as one stream.
pub(crate) fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for part in parts {
        for &byte in *part {
            let index = usize::from((c as u8) ^ byte);
            c = CRC_TABLE.get(index).copied().unwrap_or(0) ^ (c >> 8);
        }
    }
    c ^ 0xFFFF_FFFF
}

struct Chunk {
    kind: [u8; 4],
    data: Range<u64>,
    crc: Option<u32>,
}

/// Finds the Exif payload. `eXIf` wins over a legacy raw profile; any
/// further Exif source is reported as [`DiagnosticKind::DuplicateExif`].
pub(super) fn locate(input: &mut impl Input, diagnostics: &mut Diagnostics) -> Result<Located> {
    let mut found: Option<(Range<u64>, Encoding)> = None;
    let mut legacy: Option<(Range<u64>, Encoding)> = None;
    let mut seen_idat = false;
    let mut at: u64 = 8;
    if input.array::<8>(0)? != Some(SIGNATURE) {
        return Ok(located(None));
    }
    while let Some(chunk) = next_chunk(input, &mut at, diagnostics)? {
        match &chunk.kind {
            b"IDAT" => seen_idat = true,
            b"IEND" => break,
            b"eXIf" => {
                check_crc(input, &chunk, diagnostics)?;
                if found.is_some() {
                    diagnostics
                        .push(Diagnostic::new(DiagnosticKind::DuplicateExif).at(chunk.data.start));
                    continue;
                }
                if seen_idat {
                    // **quirk:** allowed since PNG 3rd edition, but some readers miss it.
                    diagnostics.push(
                        Diagnostic::new(DiagnosticKind::ExifAfterImageData).at(chunk.data.start),
                    );
                }
                found = Some((chunk.data, Encoding::Raw));
            }
            b"tEXt" | b"zTXt" | b"iTXt" => {
                if let Some(encoding) = raw_profile(input, &chunk)? {
                    if legacy.is_some() {
                        diagnostics.push(
                            Diagnostic::new(DiagnosticKind::DuplicateExif).at(chunk.data.start),
                        );
                    } else {
                        check_crc(input, &chunk, diagnostics)?;
                        legacy = Some((chunk.data, encoding));
                    }
                }
            }
            _ => {}
        }
    }
    if found.is_some() && legacy.is_some() {
        diagnostics.push(Diagnostic::new(DiagnosticKind::DuplicateExif));
    }
    Ok(located(found.or(legacy)))
}

fn located(found: Option<(Range<u64>, Encoding)>) -> Located {
    match found {
        Some((range, encoding)) => Located {
            format: Format::Png,
            exif: Some(range),
            encoding,
        },
        None => Located {
            format: Format::Png,
            exif: None,
            encoding: Encoding::Raw,
        },
    }
}

/// Reads the chunk header at `*at` and advances past the chunk.
fn next_chunk(
    input: &mut impl Input,
    at: &mut u64,
    diagnostics: &mut Diagnostics,
) -> Result<Option<Chunk>> {
    let Some([l0, l1, l2, l3, k0, k1, k2, k3]) = input.array::<8>(*at)? else {
        // A clean end of file (no IEND) is common enough to not report;
        // a partial header is a truncation.
        if input.array::<1>(*at)?.is_some() {
            truncated(diagnostics, *at);
        }
        return Ok(None);
    };
    let len = u32::from_be_bytes([l0, l1, l2, l3]);
    if len > MAX_CHUNK {
        truncated(diagnostics, *at);
        return Ok(None);
    }
    let start = at.saturating_add(8);
    let end = start.saturating_add(u64::from(len));
    let crc = input.array::<4>(end)?.map(u32::from_be_bytes);
    if crc.is_none() {
        truncated(diagnostics, *at);
    }
    let mut data = start..end;
    if let Some(file_len) = input.len()? {
        data.end = data.end.min(file_len);
    }
    *at = end.saturating_add(4);
    Ok(Some(Chunk {
        kind: [k0, k1, k2, k3],
        data,
        crc,
    }))
}

/// Verifies a chunk's CRC, reporting a mismatch as a warning.
fn check_crc(input: &mut impl Input, chunk: &Chunk, diagnostics: &mut Diagnostics) -> Result<()> {
    let Some(expected) = chunk.crc else {
        return Ok(());
    };
    // Exif chunks are small; reading one to verify it is cheap.
    let len = usize::try_from(chunk.data.end.saturating_sub(chunk.data.start)).unwrap_or(0);
    let mut data = alloc::vec![0u8; len];
    if !input.read_at(chunk.data.start, &mut data)? {
        return Ok(());
    }
    if crc32(&[&chunk.kind, &data]) != expected {
        diagnostics.push(Diagnostic::new(DiagnosticKind::CrcMismatch).at(chunk.data.start));
    }
    Ok(())
}

/// If a text chunk carries an Exif raw profile, returns how to decode it.
fn raw_profile(input: &mut impl Input, chunk: &Chunk) -> Result<Option<Encoding>> {
    raw_profile_with(input, chunk, RAW_PROFILE_KEYWORDS)
}

/// If a text chunk is a raw profile under one of `keywords`, returns how to
/// decode it.
fn raw_profile_with(
    input: &mut impl Input,
    chunk: &Chunk,
    keywords: &[&[u8]],
) -> Result<Option<Encoding>> {
    let longest = keywords.iter().map(|k| k.len()).max().unwrap_or(0);
    let mut head = [0u8; 32];
    let available = usize::try_from(chunk.data.end.saturating_sub(chunk.data.start)).unwrap_or(0);
    let head = head.get_mut(..available.min(32)).unwrap_or_default();
    if head.len() <= longest || !input.read_at(chunk.data.start, head)? {
        return Ok(None);
    }
    let Some(keyword) = keywords
        .iter()
        .find(|k| head.starts_with(k) && head.get(k.len()) == Some(&0))
    else {
        return Ok(None);
    };
    let after = keyword.len().saturating_add(1);
    Ok(match &chunk.kind {
        b"tEXt" => Some((false, after)),
        // compression method byte (0 = zlib)
        b"zTXt" => Some((true, after.saturating_add(1))),
        // compression flag, method, then language and translated keyword (NUL-terminated)
        b"iTXt" => {
            let flag = head.get(after).copied().unwrap_or(0) == 1;
            let mut rest = vec_from(input, chunk, after.saturating_add(2))?;
            let skip = (0..2).try_fold(0usize, |offset, _| {
                rest.get(offset..)?
                    .iter()
                    .position(|&b| b == 0)
                    .map(|p| offset + p + 1)
            });
            rest.clear();
            skip.map(|s| (flag, after.saturating_add(2).saturating_add(s)))
        }
        _ => None,
    }
    .map(|(compressed, text_offset)| Encoding::PngRawProfile {
        compressed,
        text_offset: u32::try_from(text_offset).unwrap_or(u32::MAX),
    }))
}

fn vec_from(input: &mut impl Input, chunk: &Chunk, from: usize) -> Result<alloc::vec::Vec<u8>> {
    let start = chunk
        .data
        .start
        .saturating_add(u64::try_from(from).unwrap_or(u64::MAX));
    let len = usize::try_from(chunk.data.end.saturating_sub(start)).unwrap_or(0);
    // The iTXt header (language, translated keyword) is short; cap the read.
    let mut buf = alloc::vec![0u8; len.min(1024)];
    Ok(if input.read_at(start, &mut buf)? {
        buf
    } else {
        alloc::vec::Vec::new()
    })
}

/// Decodes an ImageMagick raw profile: optional zlib, then
/// `"\n<name>\n<spaces><decimal length>\n<hex, any whitespace>"`.
#[cfg(feature = "png-legacy")]
pub(super) fn decode_raw_profile(
    text: &[u8],
    compressed: bool,
    limit: usize,
) -> Result<alloc::vec::Vec<u8>> {
    use crate::error::Error;
    use crate::limits::Limit;

    let malformed = |reason| Error::Malformed { offset: 0, reason };
    let inflated;
    let text = if compressed {
        inflated = inflate(text, limit)?;
        &inflated[..]
    } else {
        text
    };
    // Skip "\n<name>\n", then read the decimal length.
    let mut lines = text.splitn(4, |&b| b == b'\n');
    let (Some(_), Some(_), Some(len_line), Some(hex)) =
        (lines.next(), lines.next(), lines.next(), lines.next())
    else {
        return Err(malformed("malformed PNG raw profile"));
    };
    let len: usize = core::str::from_utf8(len_line)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| malformed("malformed PNG raw profile length"))?;
    if len > limit {
        return Err(Error::LimitExceeded(Limit::Metadata));
    }
    let mut out = alloc::vec::Vec::with_capacity(len);
    let mut digits = hex.iter().filter(|b| !b.is_ascii_whitespace());
    while out.len() < len {
        let (Some(hi), Some(lo)) = (digits.next(), digits.next()) else {
            return Err(malformed("PNG raw profile shorter than its length"));
        };
        let nibble = |b: u8| char::from(b).to_digit(16);
        let (Some(hi), Some(lo)) = (nibble(*hi), nibble(*lo)) else {
            return Err(malformed("invalid hex in PNG raw profile"));
        };
        out.push(u8::try_from(hi * 16 + lo).unwrap_or(0));
    }
    Ok(out)
}

/// Inflates zlib data, failing past `limit` output bytes.
#[cfg(feature = "inflate")]
fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>> {
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, limit).map_err(|e| {
        if e.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput {
            Error::LimitExceeded(crate::limits::Limit::Metadata)
        } else {
            Error::Malformed {
                offset: 0,
                reason: "invalid zlib data in PNG chunk",
            }
        }
    })
}

/// Splits a chunk's data at its NUL-terminated keyword.
fn keyword<'a>(data: &'a [u8], span: &Span) -> Option<(&'a [u8], &'a [u8])> {
    let body = span.body(data);
    let nul = body.iter().position(|&b| b == 0)?;
    Some((body.get(..nul)?, body.get(nul.saturating_add(1)..)?))
}

/// Output cap for inflating a PNG metadata chunk. Absolute, since ICC
/// lookup tables and padded XMP compress far better than any fixed ratio;
/// inflating stops at the cap, so a zlib bomb still cannot exhaust memory.
#[cfg(feature = "inflate")]
const INFLATE_LIMIT: usize = super::METADATA_LIMIT;

/// The XMP packet: the `iTXt` chunk with keyword `XML:com.adobe.xmp`.
pub(super) fn xmp(data: &[u8]) -> Result<Option<alloc::borrow::Cow<'_, [u8]>>> {
    use alloc::borrow::Cow;

    let (spans, _) = spans(
        &mut super::Slice(data),
        u64::try_from(data.len()).unwrap_or(u64::MAX),
    )?;
    for span in spans.iter().filter(|s| &s.kind == b"iTXt") {
        let Some((b"XML:com.adobe.xmp", rest)) = keyword(data, span) else {
            continue;
        };
        // Compression flag and method, then language and translated keyword.
        let &[flag, _method, ref rest @ ..] = rest else {
            continue;
        };
        let mut fields = rest.splitn(3, |&b| b == 0);
        let (Some(_), Some(_), Some(text)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        if flag == 0 {
            return Ok(Some(Cow::Borrowed(text)));
        }
        #[cfg(feature = "inflate")]
        return inflate(text, INFLATE_LIMIT).map(|t| Some(Cow::Owned(t)));
        #[cfg(not(feature = "inflate"))]
        return Err(Error::Unsupported {
            what: "compressed PNG XMP (enable the `inflate` feature)",
        });
    }
    #[cfg(feature = "png-legacy")]
    if let Some(xmp) = legacy_xmp(data, &spans)? {
        return Ok(Some(Cow::Owned(xmp)));
    }
    Ok(None)
}

/// **quirk:** ImageMagick stores XMP as a hex `Raw profile type xmp` text
/// chunk, like its legacy Exif profiles. Read when there is no `iTXt` XMP.
#[cfg(feature = "png-legacy")]
fn legacy_xmp(data: &[u8], spans: &[Span]) -> Result<Option<Vec<u8>>> {
    for span in spans
        .iter()
        .filter(|s| matches!(&s.kind, b"tEXt" | b"zTXt" | b"iTXt"))
    {
        let chunk = Chunk {
            kind: span.kind,
            data: span.data(),
            crc: None,
        };
        let found = raw_profile_with(&mut super::Slice(data), &chunk, &[b"Raw profile type xmp"])?;
        let Some(Encoding::PngRawProfile {
            compressed,
            text_offset,
        }) = found
        else {
            continue;
        };
        let text = span
            .body(data)
            .get(index(u64::from(text_offset))..)
            .unwrap_or_default();
        return decode_raw_profile(text, compressed, INFLATE_LIMIT).map(Some);
    }
    Ok(None)
}

/// The ICC profile: the `iCCP` chunk, inflated.
pub(super) fn icc(data: &[u8]) -> Result<Option<Vec<u8>>> {
    let (spans, _) = spans(
        &mut super::Slice(data),
        u64::try_from(data.len()).unwrap_or(u64::MAX),
    )?;
    let Some(span) = spans.iter().find(|s| &s.kind == b"iCCP") else {
        return Ok(None);
    };
    // Profile name, NUL, compression method (0 = zlib), then the data.
    let Some((_, &[0, ref compressed @ ..])) = keyword(data, span) else {
        return Err(malformed(span.start, "invalid iCCP chunk"));
    };
    #[cfg(feature = "inflate")]
    return inflate(compressed, INFLATE_LIMIT).map(Some);
    #[cfg(not(feature = "inflate"))]
    {
        let _ = compressed;
        Err(Error::Unsupported {
            what: "PNG ICC profile (enable the `inflate` feature)",
        })
    }
}

/// A chunk as byte offsets into the file: `start..end` covers length,
/// type, data and CRC.
struct Span {
    kind: [u8; 4],
    start: u64,
    end: u64,
}

impl Span {
    /// The chunk's data range (without length, type and CRC).
    fn data(&self) -> Range<u64> {
        self.start.saturating_add(8)..self.end.saturating_sub(4)
    }

    /// The chunk's data, from a file held in memory.
    fn body<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        let range = self.data();
        data.get(index(range.start)..index(range.end))
            .unwrap_or_default()
    }
}

fn index(offset: u64) -> usize {
    usize::try_from(offset).unwrap_or(usize::MAX)
}

/// Splits a PNG into its chunks, through IEND, reading only chunk headers.
/// Returns them and where the bytes after IEND (copied verbatim) start.
fn spans(input: &mut impl Input, len: u64) -> Result<(Vec<Span>, u64)> {
    if input.array::<8>(0)? != Some(SIGNATURE) {
        return Err(malformed(0, "not a PNG"));
    }
    let mut spans = Vec::new();
    let mut at = 8u64;
    while at < len {
        let Some([l0, l1, l2, l3, k0, k1, k2, k3]) = input.array::<8>(at)? else {
            return Err(malformed(at, "truncated PNG chunk"));
        };
        let length = u32::from_be_bytes([l0, l1, l2, l3]);
        if length > MAX_CHUNK {
            return Err(malformed(at, "invalid PNG chunk length"));
        }
        let end = at.saturating_add(12).saturating_add(u64::from(length));
        if end > len {
            return Err(malformed(at, "truncated PNG chunk"));
        }
        let kind = [k0, k1, k2, k3];
        spans.push(Span {
            kind,
            start: at,
            end,
        });
        at = end;
        if &kind == b"IEND" {
            break;
        }
    }
    Ok((spans, at))
}

/// Whether a text chunk carries an Exif raw profile. `Raw profile type exif`
/// always does. `Raw profile type APP1` is a generic APP1 container that
/// can hold XMP too, so it counts only if its payload decodes to Exif;
/// without `png-legacy` it cannot be decoded and is kept.
fn is_raw_profile(input: &mut impl Input, span: &Span) -> Result<bool> {
    const EXIF: &[u8; 22] = b"Raw profile type exif\0";
    if !matches!(&span.kind, b"tEXt" | b"zTXt" | b"iTXt") {
        return Ok(false);
    }
    let chunk = Chunk {
        kind: span.kind,
        data: span.data(),
        crc: None,
    };
    let Some(encoding) = raw_profile(input, &chunk)? else {
        return Ok(false);
    };
    if input.array::<22>(chunk.data.start)?.as_ref() == Some(EXIF) {
        return Ok(true);
    }
    is_exif_payload(input, &chunk, encoding)
}

#[cfg(feature = "png-legacy")]
fn is_exif_payload(input: &mut impl Input, chunk: &Chunk, encoding: Encoding) -> Result<bool> {
    // Cap the work on huge profiles; an Exif profile that big is kept.
    const MAX: u64 = 1 << 24;
    let Encoding::PngRawProfile {
        compressed,
        text_offset,
    } = encoding
    else {
        return Ok(false);
    };
    let start = chunk.data.start.saturating_add(u64::from(text_offset));
    if start >= chunk.data.end || chunk.data.end.saturating_sub(start) > MAX {
        return Ok(false);
    }
    let text = super::pieces::read_range(input, start..chunk.data.end)?;
    Ok(
        decode_raw_profile(&text, compressed, 1 << 24).is_ok_and(|payload| {
            let tiff = payload.strip_prefix(b"Exif\0\0").unwrap_or(&payload);
            tiff.starts_with(b"II*\0") || tiff.starts_with(b"MM\0*")
        }),
    )
}

#[cfg(not(feature = "png-legacy"))]
fn is_exif_payload(_: &mut impl Input, _: &Chunk, _: Encoding) -> Result<bool> {
    Ok(false)
}

/// An `eXIf` chunk holding `tiff`.
fn exif_chunk(tiff: &[u8]) -> Result<Vec<u8>> {
    let len = u32::try_from(tiff.len())
        .ok()
        .filter(|&len| len <= MAX_CHUNK)
        .ok_or(Error::TooLarge {
            size: tiff.len(),
            max: MAX_CHUNK as usize,
        })?;
    let mut chunk = Vec::with_capacity(tiff.len().saturating_add(12));
    chunk.extend_from_slice(&len.to_be_bytes());
    chunk.extend_from_slice(b"eXIf");
    chunk.extend_from_slice(tiff);
    chunk.extend_from_slice(&crc32(&[b"eXIf", tiff]).to_be_bytes());
    Ok(chunk)
}

/// Plans replacing the `eXIf` chunk with one holding `tiff` (inserting it
/// before the first IDAT if there is none), or removing it when `tiff` is
/// `None`. PNG allows one `eXIf`, so extra ones are removed, and legacy
/// raw-profile Exif text chunks are removed too: left in place they would
/// contradict the new data. Everything else is copied verbatim. Only chunk
/// headers and text-chunk keywords are read.
pub(super) fn plan(input: &mut impl Input, len: u64, tiff: Option<&[u8]>) -> Result<Pieces> {
    let (spans, rest) = spans(input, len)?;
    let new_chunk = tiff.map(exif_chunk).transpose()?;
    let target = spans.iter().position(|s| &s.kind == b"eXIf");
    let insert_at = match (target, &new_chunk) {
        (None, Some(_)) => Some(
            spans
                .iter()
                .position(|s| &s.kind == b"IDAT" || &s.kind == b"IEND")
                .unwrap_or(spans.len()),
        ),
        _ => None,
    };
    let mut out = Pieces::default();
    out.bytes(SIGNATURE);
    for (index, span) in spans.iter().enumerate() {
        if Some(index) == insert_at
            && let Some(new) = &new_chunk
        {
            out.bytes(new.clone());
        }
        if Some(index) == target {
            if let Some(new) = &new_chunk {
                out.bytes(new.clone());
            }
            continue;
        }
        if &span.kind == b"eXIf" || is_raw_profile(input, span)? {
            continue;
        }
        out.copy(span.start..span.end);
    }
    if insert_at == Some(spans.len())
        && let Some(new) = &new_chunk
    {
        out.bytes(new.clone());
    }
    out.copy(rest..len);
    Ok(out)
}

#[cfg(test)]
mod tests;
