mod locate {
    use alloc::vec::Vec;

    use super::super::*;
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

mod splice {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::super::*;

    /// The write plan, run over a file held in memory.
    fn splice(data: &[u8], tiff: Option<&[u8]>) -> Result<Vec<u8>> {
        crate::container::splice(data, crate::container::Format::Jpeg, tiff)
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
            super::super::icc(&data).unwrap().as_deref(),
            Some(&b"hello world"[..])
        );
        assert_eq!(xmp(&jpeg(&[jfif()])).unwrap(), None);
        assert_eq!(super::super::icc(&jpeg(&[jfif()])).unwrap(), None);
        // A missing or repeated segment is an error, not a corrupt profile.
        // 255 segments (the most a count byte allows) must not overflow.
        let all: Vec<_> = (1..=255).map(|seq| icc(seq, 255, b"x")).collect();
        assert_eq!(
            super::super::icc(&jpeg(&all)).unwrap().map(|p| p.len()),
            Some(255)
        );
        for broken in [
            vec![icc(1, 2, b"a")],
            vec![icc(1, 2, b"a"), icc(1, 2, b"b")],
            vec![icc(1, 1, b"a"), icc(2, 2, b"b")],
        ] {
            assert!(matches!(
                super::super::icc(&jpeg(&broken)),
                Err(Error::Malformed { .. })
            ));
        }
        // Writing Exif leaves both alone.
        let written = splice(&data, Some(TIFF_B)).unwrap();
        assert_eq!(xmp(&written).unwrap(), Some(&b"<x:xmpmeta/>"[..]));
        assert_eq!(
            super::super::icc(&written).unwrap().as_deref(),
            Some(&b"hello world"[..])
        );
    }
}
