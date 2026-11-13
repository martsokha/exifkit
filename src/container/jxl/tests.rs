use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::container::Slice;

const TIFF: &[u8] = b"II*\0\x08\0\0\0\0\0\0\0\0\0";
/// `brotli -q 11` of `00000000` + [`TIFF`]: a compressed Exif payload.
const BROTLI_EXIF: &[u8] = &[
    0xa1, 0x88, 0x00, 0xc0, 0x3f, 0x0d, 0x90, 0xa4, 0x82, 0x30, 0x0c, 0x0a, 0x04, 0x80, 0xba, 0x01,
];

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = bmff::header(*kind, payload.len() as u64);
    out.extend_from_slice(payload);
    out
}

fn exif(offset: u32, rest: &[u8]) -> Vec<u8> {
    bx(b"Exif", &[&offset.to_be_bytes()[..], rest].concat())
}

fn jxl(boxes: &[Vec<u8>]) -> Vec<u8> {
    let mut out = SIGNATURE.to_vec();
    out.extend(ftyp());
    for b in boxes {
        out.extend_from_slice(b);
    }
    out
}

fn find(data: &[u8]) -> (Located, Diagnostics) {
    let mut diagnostics = Diagnostics::default();
    let located = locate(&mut Slice(data), &mut diagnostics).unwrap();
    (located, diagnostics)
}

/// The write plan, run over a file held in memory.
fn splice(data: &[u8], tiff: Option<&[u8]>) -> Result<Vec<u8>> {
    crate::container::splice(data, crate::container::Format::Jxl, tiff)
}

fn tiff_at(data: &[u8], located: &Located) -> Vec<u8> {
    let range = located.exif.clone().unwrap();
    data[range.start as usize..range.end as usize].to_vec()
}

#[test]
fn locates_exif_after_its_offset() {
    let codestream = bx(b"jxlc", &[0xFF, 0x0A, 1, 2]);
    let data = jxl(&[exif(0, TIFF), codestream.clone()]);
    let (located, diagnostics) = find(&data);
    assert_eq!(tiff_at(&data, &located), TIFF);
    assert!(diagnostics.is_empty());
    // An offset skips a header some writers add.
    let data = jxl(&[exif(6, &[b"Exif\0\0", TIFF].concat()), codestream.clone()]);
    assert_eq!(tiff_at(&data, &find(&data).0), TIFF);
    // An offset past the end is reported, not followed.
    let data = jxl(&[exif(99, TIFF), codestream.clone()]);
    let (located, diagnostics) = find(&data);
    assert_eq!(located.exif, None);
    assert!(diagnostics.contains(DiagnosticKind::InvalidExifOffset));
    // A second Exif box is reported.
    let data = jxl(&[exif(0, TIFF), exif(0, TIFF), codestream]);
    assert!(find(&data).1.contains(DiagnosticKind::DuplicateExif));
    // A bare codestream has no Exif.
    assert_eq!(find(&[0xFF, 0x0A, 0, 0]).0.exif, None);
}

#[test]
fn locates_and_decodes_brotli_exif() {
    let brob = bx(b"brob", &[b"Exif", BROTLI_EXIF].concat());
    let data = jxl(&[brob, bx(b"jxlc", &[0xFF, 0x0A])]);
    let (located, _) = find(&data);
    assert_eq!(located.encoding, Encoding::Brotli);
    let compressed = tiff_at(&data, &located);
    #[cfg(feature = "jxl-brob")]
    assert_eq!(
        decode_brob(&mut Slice(&compressed), 0..compressed.len() as u64, 1 << 20).unwrap(),
        TIFF
    );
    #[cfg(not(feature = "jxl-brob"))]
    assert!(matches!(
        decode_brob(&mut Slice(&compressed), 0..compressed.len() as u64, 1 << 20),
        Err(Error::Unsupported { .. })
    ));
}

#[test]
#[cfg(feature = "jxl-brob")]
fn streamed_brotli_matches_in_memory() {
    let data = [b"junk", BROTLI_EXIF, b"more"].concat();
    let range = 4..4 + BROTLI_EXIF.len() as u64;
    assert_eq!(
        decode_brob(&mut Slice(&data), range.clone(), 1 << 20).unwrap(),
        TIFF
    );
    // The output cap holds when streaming too.
    assert!(matches!(
        decode_brob(&mut Slice(&data), range, 8),
        Err(Error::LimitExceeded(_))
    ));
    // A range past the input is an error, not a hang or a panic.
    assert!(decode_brob(&mut Slice(&data), 4..1 << 40, 1 << 20).is_err());
}

#[test]
#[cfg(feature = "jxl-brob")]
fn brotli_output_is_capped() {
    assert!(matches!(
        decode_brob(&mut Slice(BROTLI_EXIF), 0..BROTLI_EXIF.len() as u64, 8),
        Err(Error::LimitExceeded(_))
    ));
    assert!(matches!(
        decode_brob(
            &mut Slice(b"not brotli at all"),
            0..b"not brotli at all".len() as u64,
            1 << 20
        ),
        Err(Error::Malformed { .. })
    ));
}

#[test]
fn replaces_inserts_and_removes() {
    let codestream = bx(b"jxlc", &[0xFF, 0x0A, 1, 2]);
    let xml = bx(b"xml ", b"<x/>");
    let other = b"MM\0*\0\0\0\x08\0\0\0\0\0\0";
    let data = jxl(&[
        xml.clone(),
        exif(6, &[b"Exif\0\0", TIFF].concat()),
        codestream.clone(),
    ]);
    assert_eq!(
        splice(&data, Some(other)).unwrap(),
        jxl(&[xml.clone(), exif(0, other), codestream.clone()])
    );
    // No Exif yet: before the codestream.
    let data = jxl(&[xml.clone(), codestream.clone()]);
    assert_eq!(
        splice(&data, Some(TIFF)).unwrap(),
        jxl(&[xml.clone(), exif(0, TIFF), codestream.clone()])
    );
    // Removing drops every Exif box, compressed or not.
    let brob = bx(b"brob", &[b"Exif", BROTLI_EXIF].concat());
    let data = jxl(&[exif(0, TIFF), xml.clone(), brob, codestream.clone()]);
    assert_eq!(splice(&data, None).unwrap(), jxl(&[xml, codestream]));
}

#[test]
fn bare_codestream_is_wrapped() {
    let codestream = [0xFF, 0x0A, 9, 8, 7];
    let out = splice(&codestream, Some(TIFF)).unwrap();
    assert_eq!(out, jxl(&[exif(0, TIFF), bx(b"jxlc", &codestream)]));
    assert_eq!(tiff_at(&out, &find(&out).0), TIFF);
    // Nothing to remove from a bare codestream.
    assert_eq!(splice(&codestream, None).unwrap(), codestream);
}

#[test]
fn reconstruction_data_pins_the_exif_size() {
    let original = [TIFF, &[0u8; 10]].concat();
    let jbrd = bx(b"jbrd", &jbrd_header(&[2], 4 + original.len() as u32));
    let codestream = bx(b"jxlc", &[0xFF, 0x0A]);
    let data = jxl(&[jbrd.clone(), exif(0, &original), codestream.clone()]);
    // Smaller: padded back to the original size.
    let out = splice(&data, Some(TIFF)).unwrap();
    assert_eq!(
        out,
        jxl(&[jbrd.clone(), exif(0, &original), codestream.clone()])
    );
    // Bigger: refused rather than losing the reconstruction.
    let bigger = [TIFF, &[0u8; 11]].concat();
    assert!(matches!(
        splice(&data, Some(&bigger)),
        Err(Error::TooLarge { max: 24, .. })
    ));
    // Removing writes an empty TIFF block of the same size.
    let out = splice(&data, None).unwrap();
    assert_eq!(out.len(), data.len());
    let tiff = tiff_at(&out, &find(&out).0);
    let (exif, _) = crate::Exif::from_tiff(&tiff).unwrap();
    assert_eq!(exif.entries().count(), 0);
}

#[test]
fn recorded_size_needs_no_decompression() {
    // Found in review: removing a `brob` Exif that the reconstruction
    // records must not need `jxl-brob`; the size comes from `jbrd`.
    let jbrd = bx(b"jbrd", &jbrd_header(&[2], 4 + TIFF.len() as u32));
    let brob = bx(b"brob", &[b"Exif", BROTLI_EXIF].concat());
    let codestream = bx(b"jxlc", &[0xFF, 0x0A]);
    let data = jxl(&[jbrd.clone(), brob, codestream.clone()]);
    let out = splice(&data, None).unwrap();
    assert_eq!(out, jxl(&[jbrd.clone(), exif(0, TIFF), codestream.clone()]));
    // With the Exif box gone (another tool dropped it), a new one is
    // padded to the recorded size, so the reconstruction works again.
    let data = jxl(&[jbrd.clone(), codestream.clone()]);
    let small = &TIFF[..8];
    let padded = [small, &[0u8; 6]].concat();
    assert_eq!(
        splice(&data, Some(small)).unwrap(),
        jxl(&[jbrd, exif(0, &padded), codestream])
    );
}

/// Packs `(value, bits)` fields LSB first, as JPEG XL headers are.
fn pack(fields: &[(u32, usize)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for &(value, n) in fields {
        for i in 0..n {
            if at / 8 == out.len() {
                out.push(0);
            }
            out[at / 8] |= (((value >> i) & 1) as u8) << (at % 8);
            at += 1;
        }
    }
    out
}

/// A `jbrd` header: SOI, the given APP segment types, then EOI. An Exif
/// segment records an Exif box payload of `exif` bytes; others store 99.
fn jbrd_header(apps: &[u32], exif: u32) -> Vec<u8> {
    let mut fields = vec![(0, 1), (0xD8 - 0xC0, 6)];
    fields.extend(apps.iter().map(|_| (0xE1 - 0xC0, 6)));
    fields.push((0xD9 - 0xC0, 6));
    for &kind in apps {
        match kind {
            0 | 1 => fields.push((kind, 2)),
            2 | 3 => fields.extend([(2, 2), (kind - 2, 1)]),
            _ => fields.extend([(3, 2), (kind - 4, 2)]),
        }
        fields.push((if kind == 2 { exif + 4 } else { 99 }, 16));
    }
    pack(&fields)
}

#[test]
fn reads_what_reconstruction_records_about_exif() {
    assert_eq!(recorded(&jbrd_header(&[2], 95)), Recorded::Exif(95));
    assert_eq!(
        recorded(&jbrd_header(&[1, 3, 2], 2468)),
        Recorded::Exif(2468)
    );
    assert_eq!(recorded(&jbrd_header(&[], 0)), Recorded::Nothing);
    assert_eq!(recorded(&jbrd_header(&[0, 1, 3], 0)), Recorded::Nothing);
    assert_eq!(recorded(&[]), Recorded::Unreadable);
    assert_eq!(
        recorded(&jbrd_header(&[1, 2], 95)[..2]),
        Recorded::Unreadable
    );
    // A segment shorter than its own header, as libjxl rejects.
    assert_eq!(recorded(&jbrd_header(&[2], 0)), Recorded::Unreadable);
}

#[test]
fn exif_added_to_a_transcode_without_exif_can_grow() {
    // Found in review: Exif added after transcoding is not part of the
    // reconstruction, so a later, bigger edit must not be refused.
    let jbrd = bx(b"jbrd", &jbrd_header(&[1], 0));
    let codestream = bx(b"jxlc", &[0xFF, 0x0A]);
    let data = jxl(&[jbrd.clone(), exif(0, TIFF), codestream.clone()]);
    let bigger = [TIFF, &[0u8; 11]].concat();
    assert_eq!(
        splice(&data, Some(&bigger)).unwrap(),
        jxl(&[jbrd, exif(0, &bigger), codestream])
    );
}

#[test]
fn broken_files_are_refused() {
    let mut data = jxl(&[exif(0, TIFF)]);
    data.truncate(data.len() - 3);
    assert!(matches!(
        splice(&data, Some(TIFF)),
        Err(Error::Malformed { .. })
    ));
    assert!(find(&data).1.contains(DiagnosticKind::TruncatedContainer));
}

#[test]
fn extracts_xmp() {
    let data = jxl(&[bx(b"xml ", b"<x/>"), bx(b"jxlc", &[0xFF, 0x0A])]);
    assert_eq!(xmp(&data).unwrap().as_deref(), Some(&b"<x/>"[..]));
    assert_eq!(xmp(&[0xFF, 0x0A]).unwrap(), None);
}
