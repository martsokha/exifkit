//! ISOBMFF boxes (ISO/IEC 14496-12), as JPEG XL and HEIF use them. Only box
//! headers are read here; each format decides what to do with the payloads.

use alloc::vec::Vec;
use core::ops::Range;

use super::Input;
use crate::error::{Error, Result};

/// Most boxes walked in one container level. Each box is at least 8 bytes,
/// so this only bounds the header list for a hostile file of tiny boxes.
const MAX_BOXES: usize = 1 << 16;

/// A box as byte offsets: `start..end` covers the header and payload,
/// `data` the payload alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BmffBox {
    pub(crate) kind: [u8; 4],
    pub(crate) start: u64,
    pub(crate) data: Range<u64>,
    pub(crate) end: u64,
}

impl BmffBox {
    /// The payload, from a file held in memory.
    pub(crate) fn payload<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        let at = |o: u64| usize::try_from(o).unwrap_or(usize::MAX);
        data.get(at(self.data.start)..at(self.data.end))
            .unwrap_or_default()
    }

    /// The payload length.
    pub(crate) const fn len(&self) -> u64 {
        self.data.end.saturating_sub(self.data.start)
    }
}

/// The boxes in `range`, in order, and the offset of the first box that is
/// malformed or runs past `range` (walking stops there), if any.
///
/// Size 1 means a 64-bit size follows the type; size 0 means the box runs to
/// the end of `range`. A `uuid` box's 16-byte extended type is part of its
/// header.
pub(crate) fn boxes(
    input: &mut impl Input,
    range: Range<u64>,
) -> Result<(Vec<BmffBox>, Option<u64>)> {
    let mut boxes = Vec::new();
    let mut at = range.start;
    while at < range.end {
        if boxes.len() >= MAX_BOXES {
            return Err(Error::Malformed {
                offset: at,
                reason: "too many ISOBMFF boxes",
            });
        }
        let Some([s0, s1, s2, s3, k0, k1, k2, k3]) = input.array::<8>(at)? else {
            return Ok((boxes, Some(at)));
        };
        let kind = [k0, k1, k2, k3];
        let (size, mut header) = match u32::from_be_bytes([s0, s1, s2, s3]) {
            0 => (range.end.saturating_sub(at), 8u64),
            1 => match input.array::<8>(at.saturating_add(8))? {
                Some(large) => (u64::from_be_bytes(large), 16),
                None => return Ok((boxes, Some(at))),
            },
            size => (u64::from(size), 8),
        };
        if &kind == b"uuid" {
            header = header.saturating_add(16);
        }
        let end = at.saturating_add(size);
        if size < header || end > range.end {
            return Ok((boxes, Some(at)));
        }
        boxes.push(BmffBox {
            kind,
            start: at,
            data: at.saturating_add(header)..end,
            end,
        });
        at = end;
    }
    Ok((boxes, None))
}

/// Encodes a box header for a payload of `len` bytes, with a 64-bit size
/// when it does not fit 32 bits.
pub(crate) fn header(kind: [u8; 4], len: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    match u32::try_from(len.saturating_add(8)) {
        Ok(size) if size > 1 => {
            out.extend_from_slice(&size.to_be_bytes());
            out.extend_from_slice(&kind);
        }
        _ => {
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(&kind);
            out.extend_from_slice(&len.saturating_add(16).to_be_bytes());
        }
    }
    out
}

/// The TIFF block inside an `Exif` item or box payload: a u32 BE offset,
/// then that many bytes to skip (usually nothing, sometimes `Exif\0\0`),
/// then the TIFF header. Returns the TIFF block's range, or `None` if the
/// offset points past the payload.
pub(crate) fn exif_tiff(input: &mut impl Input, payload: Range<u64>) -> Result<Option<Range<u64>>> {
    let Some(offset) = input.array::<4>(payload.start)? else {
        return Ok(None);
    };
    let start = payload
        .start
        .saturating_add(4)
        .saturating_add(u64::from(u32::from_be_bytes(offset)));
    Ok(
        (start <= payload.end && payload.end.saturating_sub(payload.start) >= 4)
            .then_some(start..payload.end),
    )
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::container::Slice;

    fn bmff(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = header(*kind, payload.len() as u64);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn walks_boxes_of_every_size_form() {
        let mut data = bmff(b"ftyp", b"jxl \0\0\0\0");
        // 64-bit size.
        data.extend_from_slice(&1u32.to_be_bytes());
        data.extend_from_slice(b"Exif");
        data.extend_from_slice(&20u64.to_be_bytes());
        data.extend_from_slice(&[1, 2, 3, 4]);
        // uuid with its extended type, then a box running to the end.
        data.extend(bmff(b"uuid", &[9; 18]));
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(b"jxlc");
        data.extend_from_slice(b"codestream");
        let len = data.len() as u64;
        let (found, broken) = boxes(&mut Slice(&data), 0..len).unwrap();
        assert_eq!(broken, None);
        let kinds: Vec<_> = found.iter().map(|b| &b.kind).collect();
        assert_eq!(kinds, [b"ftyp", b"Exif", b"uuid", b"jxlc"]);
        assert_eq!(found[1].payload(&data), &[1, 2, 3, 4]);
        assert_eq!(found[2].payload(&data), &[9, 9]);
        assert_eq!(found[3].payload(&data), b"codestream");
    }

    #[test]
    fn stops_at_a_broken_box() {
        let mut data = bmff(b"ftyp", b"jxl ");
        let good = data.len() as u64;
        data.extend_from_slice(&100u32.to_be_bytes());
        data.extend_from_slice(b"Exif\0\0");
        let (found, broken) = boxes(&mut Slice(&data), 0..data.len() as u64).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(broken, Some(good));
        // A size smaller than the header.
        let data = [0, 0, 0, 4, b'b', b'a', b'd', b'!'];
        assert_eq!(boxes(&mut Slice(&data), 0..8).unwrap().1, Some(0));
    }

    #[test]
    fn exif_payload_offset() {
        let mut payload = 6u32.to_be_bytes().to_vec();
        payload.extend_from_slice(b"Exif\0\0II*\0");
        let data = bmff(b"Exif", &payload);
        let (found, _) = boxes(&mut Slice(&data), 0..data.len() as u64).unwrap();
        let tiff = exif_tiff(&mut Slice(&data), found[0].data.clone())
            .unwrap()
            .unwrap();
        assert_eq!(&data[tiff.start as usize..tiff.end as usize], b"II*\0");
        let bad = bmff(b"Exif", &[0, 0, 1, 0, 1, 2]);
        let (found, _) = boxes(&mut Slice(&bad), 0..bad.len() as u64).unwrap();
        assert_eq!(
            exif_tiff(&mut Slice(&bad), found[0].data.clone()).unwrap(),
            None
        );
    }

    #[test]
    fn large_payloads_get_64_bit_headers() {
        assert_eq!(
            header(*b"Exif", 4),
            vec![0, 0, 0, 12, b'E', b'x', b'i', b'f']
        );
        let big = header(*b"jxlc", u64::from(u32::MAX));
        assert_eq!(&big[..8], &[0, 0, 0, 1, b'j', b'x', b'l', b'c']);
        assert_eq!(
            u64::from_be_bytes(big[8..].try_into().unwrap()),
            u64::from(u32::MAX) + 16
        );
    }
}
