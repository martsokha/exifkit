mod locate {
    use alloc::vec::Vec;

    use super::super::*;
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

mod splice {
    use alloc::vec::Vec;

    use super::super::*;

    /// The write plan, run over a file held in memory.
    fn splice(data: &[u8], tiff: Option<&[u8]>) -> Result<Vec<u8>> {
        crate::container::splice(data, crate::container::Format::WebP, tiff)
    }

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

    #[test]
    fn extracts_xmp_and_icc_chunks() {
        let data = webp(&[
            vp8x(0x2C, 8, 8),
            raw(b"ICCP", b"profile"),
            vp8l(8, 8, false),
            raw(b"XMP ", b"<x/>"),
        ]);
        assert_eq!(chunk_data(&data, b"ICCP").unwrap(), Some(&b"profile"[..]));
        assert_eq!(chunk_data(&data, b"XMP ").unwrap(), Some(&b"<x/>"[..]));
        let written = splice(&data, Some(TIFF)).unwrap();
        assert_eq!(
            chunk_data(&written, b"ICCP").unwrap(),
            Some(&b"profile"[..])
        );
        assert_eq!(chunk_data(&written, b"XMP ").unwrap(), Some(&b"<x/>"[..]));
        assert_eq!(chunk_data(&webp(&[vp8(8, 8)]), b"XMP ").unwrap(), None);
    }
}
