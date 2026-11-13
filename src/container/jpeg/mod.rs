//! JPEG: Exif lives in an APP1 segment, after an `Exif\0\0` header.

use alloc::vec::Vec;
use core::ops::Range;

use super::pieces::{Pieces, read_range};
use super::{Input, Slice, malformed, truncated};
use crate::diagnostic::{Diagnostic, DiagnosticKind, Diagnostics};
use crate::error::{Error, Result};
use crate::tiff::{ByteOrder, Header as TiffHeader, RawIfd, Reader};

const SOI: [u8; 2] = [0xFF, 0xD8];
const APP1: u8 = 0xE1;
const SOS: u8 = 0xDA;
const EOI: u8 = 0xD9;

/// Finds the TIFF block in the first Exif APP1 segment.
///
/// Walks the marker segments from SOI and stops at SOS: metadata never
/// follows the scan in practice. Later Exif APP1 segments are reported as
/// [`DiagnosticKind::DuplicateExif`].
pub(super) fn locate(
    input: &mut impl Input,
    diagnostics: &mut Diagnostics,
) -> Result<Option<Range<u64>>> {
    if input.array::<2>(0)? != Some(SOI) {
        return Ok(None);
    }
    let mut at: u64 = 2;
    let mut found: Option<Range<u64>> = None;
    loop {
        let Some([0xFF, mut marker]) = input.array::<2>(at)? else {
            // Missing marker: not a well-formed segment boundary.
            if input.array::<1>(at)?.is_none() {
                truncated(diagnostics, at);
            }
            break;
        };
        // Fill bytes: any number of 0xFF before a marker.
        while marker == 0xFF {
            at = at.saturating_add(1);
            match input.array::<1>(at.saturating_add(1))? {
                Some([next]) => marker = next,
                None => {
                    truncated(diagnostics, at);
                    return Ok(found);
                }
            }
        }
        let segment = at;
        at = at.saturating_add(2);
        // Standalone markers have no length: TEM, RST0-7.
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if marker == SOS || marker == EOI {
            break;
        }
        let Some(len) = input.array::<2>(at)?.map(u16::from_be_bytes) else {
            truncated(diagnostics, at);
            break;
        };
        let Some(payload_len) = u64::from(len).checked_sub(2) else {
            // A length below 2 cannot be skipped reliably.
            truncated(diagnostics, at);
            break;
        };
        let payload = at.saturating_add(2);
        let end = payload.saturating_add(payload_len);
        if marker == APP1
            && let Some(header) = exif_header(input, payload, payload_len)?
        {
            if header.non_standard {
                diagnostics
                    .push(Diagnostic::new(DiagnosticKind::NonStandardExifHeader).at(payload));
            }
            let start = payload.saturating_add(header.len);
            let mut range = start..end;
            if let Some(file_len) = input.len()?
                && end > file_len
            {
                truncated(diagnostics, segment);
                range = start..file_len.max(start);
            }
            if found.is_none() {
                found = Some(range);
            } else {
                diagnostics.push(Diagnostic::new(DiagnosticKind::DuplicateExif).at(segment));
            }
        }
        at = end;
    }
    Ok(found)
}

struct Header {
    len: u64,
    non_standard: bool,
}

/// Recognizes the Exif header at the start of an APP1 payload.
///
/// Standard is `Exif\0\0`. **quirk:** `Exif\0` + any byte, and `Exif`
/// followed directly by a TIFF header, are accepted too.
fn exif_header(input: &mut impl Input, payload: u64, len: u64) -> Result<Option<Header>> {
    if len < 8 {
        return Ok(None);
    }
    let Some(head) = input.array::<8>(payload)? else {
        return Ok(None);
    };
    Ok(match head {
        [b'E', b'x', b'i', b'f', 0, 0, ..] => Some(Header {
            len: 6,
            non_standard: false,
        }),
        [b'E', b'x', b'i', b'f', b'I', b'I', 42, 0]
        | [b'E', b'x', b'i', b'f', b'M', b'M', 0, 42] => Some(Header {
            len: 4,
            non_standard: true,
        }),
        [b'E', b'x', b'i', b'f', 0, _, ..] => Some(Header {
            len: 6,
            non_standard: true,
        }),
        _ => None,
    })
}

/// Largest TIFF block an APP1 segment can hold: the u16 length counts
/// itself (2) and the `Exif\0\0` header (6).
pub(crate) const MAX_TIFF: usize = u16::MAX as usize - 2 - 6;
const APP0: u8 = 0xE0;
const APP2: u8 = 0xE2;
/// MP Entry tag of the MP Index IFD.
const MP_ENTRY: u16 = 0xB002;

/// One marker segment: `start` includes any fill bytes before the marker,
/// `marker_at` is the marker's 0xFF, and `end` is one past the payload.
struct Segment {
    marker: u8,
    start: u64,
    marker_at: u64,
    end: u64,
}

impl Segment {
    /// The payload, from a file held in memory.
    fn payload<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        let at = |o: u64| usize::try_from(o).unwrap_or(usize::MAX);
        data.get(at(self.marker_at.saturating_add(4))..at(self.end))
            .unwrap_or_default()
    }
}

/// Splits a JPEG into its marker segments, up to the scan, reading only
/// marker headers. Returns them and where the rest of the file (SOS
/// onwards, copied verbatim) starts.
fn segments(input: &mut impl Input, len: u64) -> Result<(Vec<Segment>, u64)> {
    if input.array::<2>(0)? != Some(SOI) {
        return Err(malformed(0, "not a JPEG"));
    }
    let byte = |input: &mut _, at: u64| -> Result<Option<u8>> {
        Ok(Input::array::<1>(input, at)?.map(|[b]| b))
    };
    let mut segments = Vec::new();
    let mut at = 2u64;
    loop {
        let Some(first) = byte(input, at)? else {
            return Ok((segments, at));
        };
        if first != 0xFF {
            return Err(malformed(at, "expected a JPEG marker"));
        }
        let mut marker_at = at;
        while byte(input, marker_at.saturating_add(1))? == Some(0xFF) {
            marker_at = marker_at.saturating_add(1);
        }
        let Some(marker) = byte(input, marker_at.saturating_add(1))? else {
            return Err(malformed(at, "truncated JPEG marker"));
        };
        if marker == SOS || marker == EOI {
            return Ok((segments, at));
        }
        let end = if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            marker_at.saturating_add(2)
        } else {
            let Some(length) = input.array::<2>(marker_at.saturating_add(2))? else {
                return Err(malformed(at, "truncated JPEG segment"));
            };
            let length = u16::from_be_bytes(length);
            if length < 2 {
                return Err(malformed(at, "invalid JPEG segment length"));
            }
            marker_at
                .saturating_add(2)
                .saturating_add(u64::from(length))
        };
        if end > len {
            return Err(malformed(at, "truncated JPEG segment"));
        }
        segments.push(Segment {
            marker,
            start: at,
            marker_at,
            end,
        });
        at = end;
    }
}

fn is_exif(input: &mut impl Input, segment: &Segment) -> Result<bool> {
    let payload = segment.marker_at.saturating_add(4);
    let len = segment.end.saturating_sub(payload);
    Ok(segment.marker == APP1 && exif_header(input, payload, len)?.is_some())
}

fn is_mpf(input: &mut impl Input, segment: &Segment) -> Result<bool> {
    Ok(segment.marker == APP2
        && segment.end >= segment.marker_at.saturating_add(8)
        && input.array::<4>(segment.marker_at.saturating_add(4))? == Some(*b"MPF\0"))
}

/// An APP1 segment holding `tiff`.
fn exif_segment(tiff: &[u8]) -> Result<Vec<u8>> {
    let too_large = Error::TooLarge {
        size: tiff.len(),
        max: MAX_TIFF,
    };
    if tiff.len() > MAX_TIFF {
        return Err(too_large);
    }
    let len = u16::try_from(tiff.len().saturating_add(8)).map_err(|_| too_large)?;
    let mut segment = Vec::with_capacity(tiff.len().saturating_add(10));
    segment.extend_from_slice(&[0xFF, APP1]);
    segment.extend_from_slice(&len.to_be_bytes());
    segment.extend_from_slice(b"Exif\0\0");
    segment.extend_from_slice(tiff);
    Ok(segment)
}

/// Rewrites the MP Entry offsets of an MPF segment after the data behind
/// it grew by `delta` bytes. MPF offsets are relative to the MPF header,
/// so they only change when something between the MPF segment and the
/// secondary images changes size. Returns `None` if the MPF data cannot
/// be parsed; the segment is then copied unchanged.
fn patch_mpf(segment: &[u8], delta: i64) -> Option<Vec<u8>> {
    // FF E2 len(2) "MPF\0" then a TIFF structure.
    let mp_start = 8usize;
    let mp = segment.get(mp_start..)?;
    let header = TiffHeader::parse(mp).ok()?;
    let order = header.byte_order;
    let reader = Reader::new(mp, order);
    let ifd = RawIfd::parse(reader, header.ifd0_offset)?;
    let entry = ifd.entries().find(|e| e.tag == MP_ENTRY)?;
    let at = usize::try_from(entry.data_offset().ok()?).ok()?;
    let len = usize::try_from(entry.byte_len().ok()?).ok()?;
    let mut out = segment.to_vec();
    for index in 0..len / 16 {
        // Each 16-byte MP Entry: attributes, size, offset, two dependents.
        let field = mp_start
            .checked_add(at)?
            .checked_add(index.checked_mul(16)?)?
            .checked_add(8)?;
        let bytes: [u8; 4] = out.get(field..field.checked_add(4)?)?.try_into().ok()?;
        let offset = order.u32(bytes);
        // Offset 0 is the primary image itself.
        if offset == 0 {
            continue;
        }
        let moved = u32::try_from(i64::from(offset).checked_add(delta)?).ok()?;
        let encoded = match order {
            ByteOrder::LittleEndian => moved.to_le_bytes(),
            ByteOrder::BigEndian => moved.to_be_bytes(),
        };
        out.get_mut(field..field.checked_add(4)?)?
            .copy_from_slice(&encoded);
    }
    Some(out)
}

/// Plans replacing the Exif APP1 segment with one holding `tiff`,
/// inserting it after any JFIF/JFXX APP0 segments if there is none, or
/// removing every Exif APP1 segment when `tiff` is `None`. Everything else
/// is copied verbatim; MPF offsets are adjusted when the change sits
/// behind them. Only marker headers, Exif headers and the MPF segment are
/// read.
pub(super) fn plan(input: &mut impl Input, len: u64, tiff: Option<&[u8]>) -> Result<Pieces> {
    let (segments, rest) = segments(input, len)?;
    let new_segment = tiff.map(exif_segment).transpose()?;
    let mut exif = Vec::with_capacity(segments.len());
    let mut mpf = None;
    for (index, segment) in segments.iter().enumerate() {
        exif.push(is_exif(input, segment)?);
        if mpf.is_none() && is_mpf(input, segment)? {
            mpf = Some(index);
        }
    }
    let target = exif.iter().position(|&e| e);
    let insert_at = match (target, &new_segment) {
        (None, Some(_)) => Some(
            segments
                .iter()
                .position(|s| s.marker != APP0)
                .unwrap_or(segments.len()),
        ),
        _ => None,
    };
    let new_len = new_segment
        .as_ref()
        .map_or(0, |s| u64::try_from(s.len()).unwrap_or(u64::MAX));

    // What each segment becomes: kept, replaced by the new one, or dropped.
    let keep = |index: usize| {
        Some(index) != target && !(exif.get(index) == Some(&true) && new_segment.is_none())
    };
    // How much the data behind the MPF segment moves.
    let size = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
    let mut delta: i64 = 0;
    if let Some(m) = mpf {
        for (index, segment) in segments.iter().enumerate().filter(|&(i, _)| i > m) {
            let original = segment.end.saturating_sub(segment.start);
            let now = if Some(index) == target {
                new_len
            } else if keep(index) {
                original
            } else {
                0
            };
            delta = delta
                .saturating_add(size(now))
                .saturating_sub(size(original));
        }
        if insert_at.is_some_and(|at| at > m) {
            delta = delta.saturating_add(size(new_len));
        }
    }
    let patched_mpf = match mpf.filter(|_| delta != 0).and_then(|m| segments.get(m)) {
        Some(s) => patch_mpf(&read_range(input, s.marker_at..s.end)?, delta).map(|p| (s.start, p)),
        None => None,
    };

    let mut out = Pieces::default();
    out.bytes(SOI);
    for (index, segment) in segments.iter().enumerate() {
        if Some(index) == insert_at
            && let Some(new) = &new_segment
        {
            out.bytes(new.clone());
        }
        if Some(index) == target {
            if let Some(new) = &new_segment {
                out.bytes(new.clone());
            }
            continue;
        }
        if !keep(index) {
            continue;
        }
        match &patched_mpf {
            Some((start, patched)) if *start == segment.start => {
                out.copy(segment.start..segment.marker_at);
                out.bytes(patched.clone());
            }
            _ => out.copy(segment.start..segment.end),
        }
    }
    if insert_at == Some(segments.len())
        && let Some(new) = &new_segment
    {
        out.bytes(new.clone());
    }
    out.copy(rest..len);
    Ok(out)
}

const XMP_SIGNATURE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const ICC_SIGNATURE: &[u8] = b"ICC_PROFILE\0";

/// The standard XMP packet: the first APP1 segment with the XMP signature.
/// Extended XMP (`http://ns.adobe.com/xmp/extension/`) is not merged in.
pub(super) fn xmp(data: &[u8]) -> Result<Option<&[u8]>> {
    let (segments, _) = segments(
        &mut Slice(data),
        u64::try_from(data.len()).unwrap_or(u64::MAX),
    )?;
    Ok(segments
        .iter()
        .filter(|s| s.marker == APP1)
        .find_map(|s| s.payload(data).strip_prefix(XMP_SIGNATURE)))
}

/// The ICC profile: APP2 `ICC_PROFILE` segments, each with a 1-based
/// sequence number and the total count, joined in sequence order.
pub(super) fn icc(data: &[u8]) -> Result<Option<Vec<u8>>> {
    let (segments, _) = segments(
        &mut Slice(data),
        u64::try_from(data.len()).unwrap_or(u64::MAX),
    )?;
    let mut chunks: Vec<(u8, u8, &[u8], u64)> = Vec::new();
    for segment in segments.iter().filter(|s| s.marker == APP2) {
        let Some(rest) = segment.payload(data).strip_prefix(ICC_SIGNATURE) else {
            continue;
        };
        let &[seq, count, ref body @ ..] = rest else {
            return Err(malformed(segment.start, "truncated ICC_PROFILE segment"));
        };
        chunks.push((seq, count, body, segment.start));
    }
    let Some(&(_, count, _, first)) = chunks.first() else {
        return Ok(None);
    };
    chunks.sort_by_key(|&(seq, ..)| seq);
    // Every segment must agree on the count, and the sequence must run 1..=count.
    let complete = usize::from(count) == chunks.len()
        && chunks
            .iter()
            .enumerate()
            .all(|(i, &(seq, c, ..))| usize::from(seq) == i.saturating_add(1) && c == count);
    if !complete {
        return Err(malformed(
            first,
            "ICC_PROFILE segments are missing or inconsistent",
        ));
    }
    Ok(Some(
        chunks
            .iter()
            .flat_map(|&(_, _, body, _)| body.iter().copied())
            .collect(),
    ))
}

#[cfg(test)]
mod tests;
