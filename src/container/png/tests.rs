mod locate {
    use alloc::vec::Vec;

    use super::super::*;
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

mod splice {
    use alloc::vec::Vec;

    use super::super::*;

    /// The write plan, run over a file held in memory.
    fn splice(data: &[u8], tiff: Option<&[u8]>) -> Result<Vec<u8>> {
        crate::container::splice(data, crate::container::Format::Png, tiff)
    }
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
        let (spans, _) = spans(&mut Slice(data), data.len() as u64).unwrap();
        spans.iter().all(|s| {
            let (start, end) = (s.start as usize, s.end as usize);
            let body = &data[start + 4..end - 4];
            let crc = u32::from_be_bytes(data[end - 4..end].try_into().unwrap());
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
    #[cfg(feature = "png-legacy")]
    fn non_exif_app1_profiles_are_kept() {
        let hex = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|b| alloc::format!("{b:02x}"))
                .collect::<alloc::string::String>()
        };
        let profile = |payload: &[u8]| {
            let mut text = b"Raw profile type APP1\0\nAPP1\n".to_vec();
            text.extend_from_slice(
                alloc::format!("{}\n{}\n", payload.len(), hex(payload)).as_bytes(),
            );
            chunk(b"tEXt", &text)
        };
        let xmp = profile(b"http://ns.adobe.com/xap/1.0/\0<x/>");
        let exif = profile(&[b"Exif\0\0", TIFF].concat());
        let idat = chunk(b"IDAT", b"pixels");
        let original = png(&[xmp.clone(), exif, idat.clone()]);
        assert_eq!(splice(&original, None).unwrap(), png(&[xmp, idat]));
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

    #[test]
    fn extracts_xmp_from_itxt() {
        let itxt = chunk(b"iTXt", b"XML:com.adobe.xmp\0\0\0\0\0<x:xmpmeta/>");
        let other = chunk(b"iTXt", b"Comment\0\0\0\0\0hello");
        let data = png(&[
            chunk(b"IHDR", &[0; 13]),
            other,
            itxt,
            chunk(b"IDAT", b"pixels"),
        ]);
        assert_eq!(xmp(&data).unwrap().as_deref(), Some(&b"<x:xmpmeta/>"[..]));
        assert_eq!(xmp(&png(&[chunk(b"IDAT", b"pixels")])).unwrap(), None);
        assert_eq!(icc(&png(&[chunk(b"IDAT", b"pixels")])).unwrap(), None);
    }

    #[test]
    #[cfg(feature = "inflate")]
    fn inflates_iccp_and_compressed_xmp() {
        // Highly compressible, as ICC lookup tables are: far beyond 4:1.
        let profile = &[7u8; 300_000][..];
        let mut iccp = b"sRGB\0\0".to_vec();
        iccp.extend(miniz_oxide::deflate::compress_to_vec_zlib(profile, 6));
        let mut itxt = b"XML:com.adobe.xmp\0\x01\0\0\0".to_vec();
        itxt.extend(miniz_oxide::deflate::compress_to_vec_zlib(
            b"<x:xmpmeta/>",
            6,
        ));
        let data = png(&[
            chunk(b"iCCP", &iccp),
            chunk(b"iTXt", &itxt),
            chunk(b"IDAT", b"pixels"),
        ]);
        assert_eq!(icc(&data).unwrap().as_deref(), Some(profile));
        assert_eq!(xmp(&data).unwrap().as_deref(), Some(&b"<x:xmpmeta/>"[..]));
        // Writing Exif leaves both alone.
        let written = splice(&data, Some(TIFF)).unwrap();
        assert_eq!(icc(&written).unwrap().as_deref(), Some(profile));
        // A non-zlib method is refused.
        let data = png(&[chunk(b"iCCP", b"sRGB\0\x01junk"), chunk(b"IDAT", b"pixels")]);
        assert!(matches!(icc(&data), Err(Error::Malformed { .. })));
    }

    #[test]
    #[cfg(feature = "png-legacy")]
    fn reads_legacy_raw_profile_xmp() {
        let text = b"Raw profile type xmp\0\nxmp\n   4\n3c782f3e\n";
        let data = png(&[chunk(b"tEXt", text), chunk(b"IDAT", b"pixels")]);
        assert_eq!(xmp(&data).unwrap().as_deref(), Some(&b"<x/>"[..]));
    }
}
