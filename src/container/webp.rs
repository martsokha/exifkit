//! WebP: Exif lives in the `EXIF` chunk of the RIFF container.

use core::ops::Range;

use super::{Input, truncated};
use crate::diagnostic::{Diagnostic, DiagnosticKind, Diagnostics};
use crate::error::Result;

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
                diagnostics.push(Diagnostic::new(DiagnosticKind::DuplicateExif).at_file(at));
            }
        }
        // Chunks are padded to an even length.
        at = data_end.saturating_add(len & 1);
    }
    Ok(found)
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
