//! JPEG: Exif lives in an APP1 segment, after an `Exif\0\0` header.

use core::ops::Range;

use super::{Input, truncated};
use crate::diagnostic::{Diagnostic, DiagnosticKind, Diagnostics};
use crate::error::Result;

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
