//! JPEG: Exif lives in an APP1 segment, after an `Exif\0\0` header.

use alloc::vec::Vec;
use core::ops::Range;

use super::pieces::{Pieces, read_range};
use super::{Input, Slice, truncated};
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

const fn malformed(offset: u64, reason: &'static str) -> Error {
    Error::Malformed { offset, reason }
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
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::container::Slice;

    fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFF, marker];
        out.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn jpeg(segments: &[Vec<u8>]) -> Vec<u8> {
        let mut out = SOI.to_vec();
        for s in segments {
            out.extend_from_slice(s);
        }
        out.extend_from_slice(&[0xFF, SOS, 0, 2, 0xAB, 0xFF, EOI]);
        out
    }

    fn find(data: &[u8]) -> (Option<Range<u64>>, Diagnostics) {
        let mut diagnostics = Diagnostics::default();
        let found = locate(&mut Slice(data), &mut diagnostics).unwrap();
        (found, diagnostics)
    }

    use alloc::vec;

    #[test]
    fn finds_exif_after_jfif_and_skips_xmp() {
        let data = jpeg(&[
            segment(0xE0, b"JFIF\0\x01\x02"),
            segment(APP1, b"http://ns.adobe.com/xap/1.0/\0<x/>"),
            segment(APP1, b"Exif\0\0II*\0\x08\0\0\0"),
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
    fn second_exif_is_reported() {
        let data = jpeg(&[
            segment(APP1, b"Exif\0\0II*\0\x08\0\0\0"),
            segment(APP1, b"Exif\0\0MM\0*\0\0\0\x08"),
        ]);
        let (found, diagnostics) = find(&data);
        assert_eq!(found.map(|r| r.start), Some(12));
        assert!(diagnostics.contains(DiagnosticKind::DuplicateExif));
    }

    #[test]
    fn quirky_headers() {
        let (found, d) = find(&jpeg(&[segment(APP1, b"ExifII*\0\x08\0\0\0")]));
        assert_eq!(found.map(|r| r.start), Some(10));
        assert!(d.contains(DiagnosticKind::NonStandardExifHeader));
        let (found, _) = find(&jpeg(&[segment(APP1, b"Exif\0\xFFII*\0\x08\0\0\0")]));
        assert_eq!(found.map(|r| r.start), Some(12));
    }

    #[test]
    fn fill_bytes_and_restart_markers() {
        let mut data = SOI.to_vec();
        data.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
        data.extend_from_slice(&[0xD0]); // RST0 after fill bytes
        data.extend(segment(APP1, b"Exif\0\0II*\0\x08\0\0\0"));
        let (found, diagnostics) = find(&data);
        assert!(found.is_some(), "{diagnostics:?}");
    }

    #[test]
    fn stops_at_sos_and_handles_truncation() {
        let mut data = jpeg(&[]);
        data.extend(segment(APP1, b"Exif\0\0II*\0\x08\0\0\0")); // after the scan
        assert_eq!(find(&data).0, None);

        let mut cut = SOI.to_vec();
        cut.extend(segment(APP1, b"Exif\0\0II*\0\x08\0\0\0\x01\x02\x03"));
        cut.truncate(cut.len() - 2);
        let (found, diagnostics) = find(&cut);
        let found = found.unwrap();
        assert_eq!(found.end, cut.len() as u64);
        assert!(diagnostics.contains(DiagnosticKind::TruncatedContainer));
    }

    #[test]
    fn garbage_never_loops() {
        assert_eq!(find(&[0xFF, 0xD8, 0xFF, 0xE1, 0, 0]).0, None);
        assert_eq!(find(&[0xFF, 0xD8, 0x00, 0x00]).0, None);
        assert_eq!(find(&[0xFF, 0xD8, 0xFF, 0xFF, 0xFF]).0, None);
    }
}

#[cfg(test)]
mod splice_tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;

    /// The plan, run over a file held in memory.
    fn splice(data: &[u8], tiff: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut input = crate::container::Slice(data);
        let len = u64::try_from(data.len()).unwrap();
        plan(&mut input, len, tiff)?.to_vec(&mut input)
    }

    const TIFF_A: &[u8] = b"II*\0\x08\0\0\0\0\0\0\0\0\0";
    const TIFF_B: &[u8] = b"MM\0*\0\0\0\x08\0\0\0\0\0\0";

    fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFF, marker];
        out.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn exif(tiff: &[u8]) -> Vec<u8> {
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(tiff);
        segment(APP1, &payload)
    }

    /// SOI, the given segments, then a scan with entropy data and EOI.
    fn jpeg(segments: &[Vec<u8>]) -> Vec<u8> {
        let mut out = SOI.to_vec();
        for s in segments {
            out.extend_from_slice(s);
        }
        out.extend_from_slice(&[0xFF, SOS, 0, 4, 1, 2, 0xAB, 0xFF, 0x00, 0xCD, 0xFF, EOI]);
        out
    }

    fn jfif() -> Vec<u8> {
        segment(APP0, b"JFIF\0\x01\x02")
    }

    #[test]
    fn replaces_exif_in_place() {
        let dqt = segment(0xDB, &[0; 5]);
        let original = jpeg(&[jfif(), exif(TIFF_A), dqt.clone()]);
        let out = splice(&original, Some(TIFF_B)).unwrap();
        assert_eq!(out, jpeg(&[jfif(), exif(TIFF_B), dqt]));
    }

    #[test]
    fn inserts_after_jfif() {
        let dqt = segment(0xDB, &[0; 5]);
        let out = splice(&jpeg(&[jfif(), dqt.clone()]), Some(TIFF_A)).unwrap();
        assert_eq!(out, jpeg(&[jfif(), exif(TIFF_A), dqt.clone()]));
        // Without JFIF it goes right after SOI.
        let out = splice(&jpeg(core::slice::from_ref(&dqt)), Some(TIFF_A)).unwrap();
        assert_eq!(out, jpeg(&[exif(TIFF_A), dqt]));
    }

    #[test]
    fn removes_every_exif_segment_and_keeps_xmp() {
        let xmp = segment(APP1, b"http://ns.adobe.com/xap/1.0/\0<x/>");
        let original = jpeg(&[jfif(), exif(TIFF_A), xmp.clone(), exif(TIFF_B)]);
        assert_eq!(
            splice(&original, None).unwrap(),
            jpeg(&[jfif(), xmp.clone()])
        );
        // Writing keeps a duplicate as it was: only the first is replaced.
        let out = splice(&original, Some(TIFF_B)).unwrap();
        assert_eq!(out, jpeg(&[jfif(), exif(TIFF_B), xmp, exif(TIFF_B)]));
    }

    #[test]
    fn too_large_is_an_error_not_a_corrupt_file() {
        let original = jpeg(&[jfif()]);
        let big = vec![0u8; MAX_TIFF + 1];
        assert!(matches!(
            splice(&original, Some(&big)),
            Err(Error::TooLarge { max: MAX_TIFF, .. })
        ));
        assert!(splice(&original, Some(&big[..MAX_TIFF])).is_ok());
    }

    /// An MPF segment whose MP Index lists the primary image (offset 0) and
    /// one secondary image at `offset`.
    fn mpf(offset: u32) -> Vec<u8> {
        let mut tiff = b"MM\0*\0\0\0\x08".to_vec();
        tiff.extend_from_slice(&1u16.to_be_bytes());
        // MPEntry: UNDEFINED, 32 bytes at offset 26.
        tiff.extend_from_slice(&[0xB0, 0x02, 0, 7, 0, 0, 0, 32, 0, 0, 0, 26]);
        tiff.extend_from_slice(&0u32.to_be_bytes());
        for entry_offset in [0u32, offset] {
            tiff.extend_from_slice(&[0; 8]);
            tiff.extend_from_slice(&entry_offset.to_be_bytes());
            tiff.extend_from_slice(&[0; 4]);
        }
        let mut payload = b"MPF\0".to_vec();
        payload.extend_from_slice(&tiff);
        segment(APP2, &payload)
    }

    fn mpf_offset(data: &[u8]) -> u32 {
        let mut input = Slice(data);
        let (segments, _) = segments(&mut input, data.len() as u64).unwrap();
        let s = segments
            .iter()
            .find(|s| is_mpf(&mut input, s).unwrap())
            .unwrap();
        let mp = &data[s.marker_at as usize + 8..s.end as usize];
        u32::from_be_bytes(mp[26 + 16 + 8..26 + 16 + 12].try_into().unwrap())
    }

    #[test]
    fn mpf_offsets_follow_exif_growth_behind_them() {
        // Exif after MPF: the secondary image moves, so its offset changes.
        let original = jpeg(&[mpf(1000), exif(TIFF_A)]);
        let bigger = [TIFF_A, &[0u8; 10]].concat();
        let out = splice(&original, Some(&bigger)).unwrap();
        assert_eq!(mpf_offset(&out), 1010);
        assert_eq!(
            mpf_offset(&splice(&original, None).unwrap()),
            1000 - exif(TIFF_A).len() as u32
        );
        // Exif before MPF: both move together, so nothing changes.
        let original = jpeg(&[exif(TIFF_A), mpf(1000)]);
        assert_eq!(mpf_offset(&splice(&original, Some(&bigger)).unwrap()), 1000);
    }

    #[test]
    fn scan_data_is_copied_verbatim_and_broken_files_refused() {
        let original = jpeg(&[jfif()]);
        let out = splice(&original, Some(TIFF_A)).unwrap();
        let scan = original.iter().position(|&b| b == SOS).unwrap() - 1;
        assert!(out.ends_with(&original[scan..]));
        let mut cut = jpeg(&[jfif(), exif(TIFF_A)]);
        cut.truncate(8);
        assert!(matches!(
            splice(&cut, Some(TIFF_B)),
            Err(Error::Malformed { .. })
        ));
    }

    fn icc(seq: u8, count: u8, body: &[u8]) -> Vec<u8> {
        let mut payload = ICC_SIGNATURE.to_vec();
        payload.extend_from_slice(&[seq, count]);
        payload.extend_from_slice(body);
        segment(APP2, &payload)
    }

    #[test]
    fn extracts_xmp_and_joins_icc_in_sequence_order() {
        let xmp_segment = segment(APP1, &[XMP_SIGNATURE, b"<x:xmpmeta/>"].concat());
        let data = jpeg(&[
            jfif(),
            exif(TIFF_A),
            xmp_segment,
            icc(2, 2, b"world"),
            icc(1, 2, b"hello "),
        ]);
        assert_eq!(xmp(&data).unwrap(), Some(&b"<x:xmpmeta/>"[..]));
        assert_eq!(
            super::icc(&data).unwrap().as_deref(),
            Some(&b"hello world"[..])
        );
        assert_eq!(xmp(&jpeg(&[jfif()])).unwrap(), None);
        assert_eq!(super::icc(&jpeg(&[jfif()])).unwrap(), None);
        // A missing or repeated segment is an error, not a corrupt profile.
        // 255 segments (the most a count byte allows) must not overflow.
        let all: Vec<_> = (1..=255).map(|seq| icc(seq, 255, b"x")).collect();
        assert_eq!(super::icc(&jpeg(&all)).unwrap().map(|p| p.len()), Some(255));
        for broken in [
            vec![icc(1, 2, b"a")],
            vec![icc(1, 2, b"a"), icc(1, 2, b"b")],
            vec![icc(1, 1, b"a"), icc(2, 2, b"b")],
        ] {
            assert!(matches!(
                super::icc(&jpeg(&broken)),
                Err(Error::Malformed { .. })
            ));
        }
        // Writing Exif leaves both alone.
        let written = splice(&data, Some(TIFF_B)).unwrap();
        assert_eq!(xmp(&written).unwrap(), Some(&b"<x:xmpmeta/>"[..]));
        assert_eq!(
            super::icc(&written).unwrap().as_deref(),
            Some(&b"hello world"[..])
        );
    }
}
