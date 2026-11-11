//! PNG: Exif lives in the `eXIf` chunk (PNG 3rd edition), or in older files
//! in a "Raw profile type exif" text chunk written by ImageMagick.

use alloc::vec::Vec;
use core::ops::Range;

use super::{Encoding, Format, Input, Located, truncated};
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
    let longest = RAW_PROFILE_KEYWORDS
        .iter()
        .map(|k| k.len())
        .max()
        .unwrap_or(0);
    let mut head = [0u8; 32];
    let available = usize::try_from(chunk.data.end.saturating_sub(chunk.data.start)).unwrap_or(0);
    let head = head.get_mut(..available.min(32)).unwrap_or_default();
    if head.len() <= longest || !input.read_at(chunk.data.start, head)? {
        return Ok(None);
    }
    let Some(keyword) = RAW_PROFILE_KEYWORDS
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
        inflated =
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(text, limit).map_err(|e| {
                if e.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput {
                    Error::LimitExceeded(Limit::Alloc)
                } else {
                    malformed("invalid zlib data in PNG text chunk")
                }
            })?;
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
        return Err(Error::LimitExceeded(Limit::Alloc));
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

/// A chunk as byte offsets into the file: `start..end` covers length,
/// type, data and CRC.
struct Span {
    kind: [u8; 4],
    start: usize,
    end: usize,
}

fn malformed(offset: usize, reason: &'static str) -> Error {
    Error::Malformed {
        offset: u64::try_from(offset).unwrap_or(u64::MAX),
        reason,
    }
}

/// Splits a PNG into its chunks, through IEND. Returns them and where the
/// bytes after IEND (copied verbatim) start.
fn spans(data: &[u8]) -> Result<(Vec<Span>, usize)> {
    if data.get(..8) != Some(&SIGNATURE[..]) {
        return Err(malformed(0, "not a PNG"));
    }
    let mut spans = Vec::new();
    let mut at = 8usize;
    while at < data.len() {
        let Some(&[l0, l1, l2, l3, k0, k1, k2, k3]) = data.get(at..at.saturating_add(8)) else {
            return Err(malformed(at, "truncated PNG chunk"));
        };
        let len = u32::from_be_bytes([l0, l1, l2, l3]);
        if len > MAX_CHUNK {
            return Err(malformed(at, "invalid PNG chunk length"));
        }
        let end = at
            .saturating_add(12)
            .saturating_add(usize::try_from(len).unwrap_or(usize::MAX));
        if end > data.len() {
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

/// Whether a text chunk carries an Exif raw profile.
fn is_raw_profile(data: &[u8], span: &Span) -> bool {
    if !matches!(&span.kind, b"tEXt" | b"zTXt" | b"iTXt") {
        return false;
    }
    let chunk = Chunk {
        kind: span.kind,
        data: u64::try_from(span.start.saturating_add(8)).unwrap_or(u64::MAX)
            ..u64::try_from(span.end.saturating_sub(4)).unwrap_or(0),
        crc: None,
    };
    raw_profile(&mut super::Slice(data), &chunk)
        .ok()
        .flatten()
        .is_some()
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

/// Replaces the `eXIf` chunk with one holding `tiff` (inserting it before
/// the first IDAT if there is none), or removes it when `tiff` is `None`.
/// PNG allows one `eXIf`, so extra ones are removed, and legacy raw-profile
/// Exif text chunks are removed too: left in place they would contradict
/// the new data. Everything else is copied verbatim.
pub(super) fn splice(data: &[u8], tiff: Option<&[u8]>) -> Result<Vec<u8>> {
    let (spans, rest) = spans(data)?;
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
    let mut out = Vec::with_capacity(
        data.len()
            .saturating_add(new_chunk.as_ref().map_or(0, Vec::len)),
    );
    out.extend_from_slice(&SIGNATURE);
    for (index, span) in spans.iter().enumerate() {
        if Some(index) == insert_at
            && let Some(new) = &new_chunk
        {
            out.extend_from_slice(new);
        }
        if Some(index) == target {
            if let Some(new) = &new_chunk {
                out.extend_from_slice(new);
            }
            continue;
        }
        if &span.kind == b"eXIf" || is_raw_profile(data, span) {
            continue;
        }
        out.extend_from_slice(data.get(span.start..span.end).unwrap_or_default());
    }
    if insert_at == Some(spans.len())
        && let Some(new) = &new_chunk
    {
        out.extend_from_slice(new);
    }
    out.extend_from_slice(data.get(rest..).unwrap_or_default());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::container::Slice;

    pub(crate) fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
        out
    }

    fn png(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = SIGNATURE.to_vec();
        for c in chunks {
            out.extend_from_slice(c);
        }
        out.extend(chunk(b"IEND", b""));
        out
    }

    fn find(data: &[u8]) -> (Located, Diagnostics) {
        let mut diagnostics = Diagnostics::default();
        let located = locate(&mut Slice(data), &mut diagnostics).unwrap();
        (located, diagnostics)
    }

    const TIFF: &[u8] = b"II*\0\x08\0\0\0";

    #[test]
    fn crc_matches_the_png_spec() {
        // The CRC of an empty IEND chunk is fixed by the spec.
        assert_eq!(crc32(&[b"IEND"]), 0xAE42_6082);
    }

    #[test]
    fn finds_exif_chunk() {
        let data = png(&[
            chunk(b"IHDR", &[0; 13]),
            chunk(b"eXIf", TIFF),
            chunk(b"IDAT", b"x"),
        ]);
        let (located, diagnostics) = find(&data);
        let range = located.exif.unwrap();
        assert_eq!(&data[range.start as usize..range.end as usize], TIFF);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn exif_after_idat_and_bad_crc_are_reported() {
        let mut exif = chunk(b"eXIf", TIFF);
        let last = exif.len() - 1;
        exif[last] ^= 0xFF;
        let data = png(&[chunk(b"IDAT", b"x"), exif]);
        let (located, diagnostics) = find(&data);
        assert!(located.exif.is_some());
        assert!(diagnostics.contains(DiagnosticKind::ExifAfterImageData));
        assert!(diagnostics.contains(DiagnosticKind::CrcMismatch));
    }

    #[test]
    fn finds_raw_profiles() {
        let text = chunk(
            b"tEXt",
            b"Raw profile type exif\0\nexif\n   8\n49492a0008000000\n",
        );
        let (located, _) = find(&png(&[text]));
        assert_eq!(
            located.encoding,
            Encoding::PngRawProfile {
                compressed: false,
                text_offset: 22
            }
        );
        let itxt = chunk(b"iTXt", b"Raw profile type exif\0\x01\0en\0\0compressed...");
        let (located, _) = find(&png(&[itxt]));
        assert_eq!(
            located.encoding,
            Encoding::PngRawProfile {
                compressed: true,
                text_offset: 28
            }
        );
    }

    #[test]
    fn exif_chunk_wins_over_raw_profile() {
        let text = chunk(
            b"tEXt",
            b"Raw profile type exif\0\nexif\n   8\n49492a0008000000\n",
        );
        let (located, diagnostics) = find(&png(&[text, chunk(b"eXIf", TIFF)]));
        assert_eq!(located.encoding, Encoding::Raw);
        assert!(diagnostics.contains(DiagnosticKind::DuplicateExif));
    }

    #[test]
    fn hostile_chunk_lengths() {
        let mut data = SIGNATURE.to_vec();
        data.extend_from_slice(&u32::MAX.to_be_bytes());
        data.extend_from_slice(b"eXIf");
        let (located, diagnostics) = find(&data);
        assert!(located.exif.is_none());
        assert!(diagnostics.contains(DiagnosticKind::TruncatedContainer));
    }

    #[cfg(feature = "png-legacy")]
    #[test]
    fn decodes_raw_profiles() {
        let plain =
            decode_raw_profile(b"\nexif\n   8\n49492a00\n08000000\n", false, 1 << 20).unwrap();
        assert_eq!(plain, TIFF);
        let compressed =
            miniz_oxide::deflate::compress_to_vec_zlib(b"\nexif\n   8\n49492a0008000000\n", 6);
        assert_eq!(
            decode_raw_profile(&compressed, true, 1 << 20).unwrap(),
            TIFF
        );
        assert!(decode_raw_profile(b"\nexif\n   99\n4949\n", false, 1 << 20).is_err());
        assert!(decode_raw_profile(b"\nexif\n   8\n49492a0008000000\n", false, 4).is_err());
    }
}

#[cfg(test)]
mod splice_tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::container::Slice;

    const TIFF: &[u8] = b"II*\0\x08\0\0\0\0\0\0\0\0\0";
    const OTHER: &[u8] = b"MM\0*\0\0\0\x08\0\0\0\0\0\0";

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
        out
    }

    fn png(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = SIGNATURE.to_vec();
        for c in chunks {
            out.extend_from_slice(c);
        }
        out.extend(chunk(b"IEND", b""));
        out
    }

    fn crcs_valid(data: &[u8]) -> bool {
        let (spans, _) = spans(data).unwrap();
        spans.iter().all(|s| {
            let body = &data[s.start + 4..s.end - 4];
            let crc = u32::from_be_bytes(data[s.end - 4..s.end].try_into().unwrap());
            crc32(&[body]) == crc
        })
    }

    #[test]
    fn replaces_in_place_and_inserts_before_idat() {
        let ihdr = chunk(b"IHDR", &[0; 13]);
        let idat = chunk(b"IDAT", b"pixels");
        let original = png(&[ihdr.clone(), chunk(b"eXIf", TIFF), idat.clone()]);
        let out = splice(&original, Some(OTHER)).unwrap();
        assert_eq!(
            out,
            png(&[ihdr.clone(), chunk(b"eXIf", OTHER), idat.clone()])
        );
        assert!(crcs_valid(&out));
        let out = splice(&png(&[ihdr.clone(), idat.clone()]), Some(TIFF)).unwrap();
        assert_eq!(out, png(&[ihdr, chunk(b"eXIf", TIFF), idat]));
    }

    #[test]
    fn legacy_profiles_and_duplicates_go() {
        let legacy = chunk(
            b"tEXt",
            b"Raw profile type exif\0\nexif\n   8\n49492a0008000000\n",
        );
        let comment = chunk(b"tEXt", b"Comment\0hello");
        let idat = chunk(b"IDAT", b"pixels");
        let original = png(&[
            legacy,
            comment.clone(),
            chunk(b"eXIf", TIFF),
            idat.clone(),
            chunk(b"eXIf", TIFF),
        ]);
        let out = splice(&original, Some(OTHER)).unwrap();
        assert_eq!(
            out,
            png(&[comment.clone(), chunk(b"eXIf", OTHER), idat.clone()])
        );
        assert_eq!(splice(&original, None).unwrap(), png(&[comment, idat]));
        let mut diagnostics = Diagnostics::default();
        assert!(
            locate(
                &mut Slice(&splice(&original, None).unwrap()),
                &mut diagnostics
            )
            .unwrap()
            .exif
            .is_none()
        );
    }

    #[test]
    fn broken_files_are_refused() {
        let mut cut = png(&[chunk(b"IDAT", b"pixels")]);
        cut.truncate(cut.len() - 14);
        assert!(matches!(
            splice(&cut, Some(TIFF)),
            Err(Error::Malformed { .. })
        ));
    }
}
