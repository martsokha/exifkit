//! Writing the sample corpus back: every file survives read, write, read,
//! and writing into a file leaves every byte outside the Exif data alone.

// A test harness over trusted, pinned inputs: the library's no-panic lints
// (docs/architecture.md#safety-model) do not apply here.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::fmt::Write as _;

use common::{fixture, goldens, root};
use exifkit::tag::Role;
use exifkit::{Exif, FromValue, IfdKind, Tag, Value};

mod common;

/// What the writer cannot keep: entries of unknown type and duplicates.
fn writable(exif: &Exif) -> Exif {
    let mut exif = exif.clone();
    let kinds: Vec<IfdKind> = exif.ifds().iter().map(exifkit::Ifd::kind).collect();
    for kind in kinds {
        let ifd = exif.ifd_mut(kind);
        ifd.retain(|e| !matches!(e.value(), Value::Unknown { .. }));
        ifd.clear_duplicates();
    }
    exif
}

/// What survives writing with `source`: as [`writable`], minus data
/// references (strips, tiles, thumbnail) that point outside the source.
fn writable_with_source(exif: &Exif, source: &[u8]) -> Exif {
    let pairs = [
        (Tag::STRIP_OFFSETS, Tag::STRIP_BYTE_COUNTS),
        (Tag::TILE_OFFSETS, Tag::TILE_BYTE_COUNTS),
        (
            Tag::JPEG_INTERCHANGE_FORMAT,
            Tag::JPEG_INTERCHANGE_FORMAT_LENGTH,
        ),
    ];
    let mut exif = writable(exif);
    let kinds: Vec<IfdKind> = exif.ifds().iter().map(exifkit::Ifd::kind).collect();
    for kind in kinds {
        for (offsets_tag, lengths_tag) in pairs {
            let ifd = exif.ifd_mut(kind);
            let values = |tag| {
                ifd.get(tag)
                    .and_then(|e| Vec::<u32>::from_value(e.value()).ok())
            };
            let Some(offsets) = values(offsets_tag) else {
                continue;
            };
            let lengths = values(lengths_tag);
            let dangles = offsets.iter().enumerate().any(|(i, &o)| {
                let l = lengths
                    .as_ref()
                    .and_then(|l| l.get(i))
                    .copied()
                    .unwrap_or(0);
                u64::from(o) + u64::from(l) > source.len() as u64
                    || u64::from(o) >= source.len() as u64
            });
            if dangles {
                ifd.remove(offsets_tag);
                ifd.remove(lengths_tag);
            }
        }
    }
    exif
}

/// Every sample survives read → write → read unchanged: with the source
/// block (image data kept in place, the MakerNote at its original offset)
/// and without it (where the model references no image data by offset).
#[test]
fn writer_round_trips_the_corpus() {
    let root = root();
    let mut paths = Vec::new();
    goldens(&root.join("tests/golden"), &mut paths);
    paths.sort();
    let external = root.join("tests/fixtures/external");
    let (mut preserved, mut compact, mut needs_source, mut anchored) = (0, 0, 0, 0);
    let mut report = String::new();
    for path in &paths {
        let rel = path
            .strip_prefix(root.join("tests/golden"))
            .expect("golden under tests/golden")
            .with_extension("");
        let Some(data) = fixture(&external.join(&rel)) else {
            continue;
        };
        let Ok((exif, _)) = Exif::read(&data) else {
            continue;
        };
        let expected = writable(&exif);
        let name = rel.display();
        // Strips, tiles or a thumbnail by offset: the only reason a compact
        // write (no source) may refuse.
        let data_refs = exif.entries().any(|(_, e)| {
            e.tag()
                .info()
                .is_some_and(|i| matches!(i.role, Role::DataOffset | Role::DataLength))
        });

        // With the source TIFF block (not available for decoded PNG text).
        let source = exifkit::container::locate(&data)
            .ok()
            .and_then(|(located, _)| {
                let range = located
                    .exif
                    .filter(|_| located.encoding == exifkit::container::Encoding::Raw)?;
                let start = usize::try_from(range.start).ok()?;
                let end = usize::try_from(range.end).ok()?.min(data.len());
                data.get(start..end)
            });
        if let Some(source) = source {
            let options = exifkit::WriteOptions::DEFAULT.source(source);
            match exif.to_tiff_with(&options) {
                Ok(bytes) => match Exif::from_tiff(&bytes) {
                    Ok((back, _)) => {
                        preserved += 1;
                        if back != writable_with_source(&exif, source) {
                            let _ =
                                writeln!(report, "{name}: preserving round trip changed the model");
                        }
                        if exif.maker_note_offset().is_some() {
                            anchored += 1;
                            if back.maker_note_offset() != exif.maker_note_offset() {
                                let _ = writeln!(
                                    report,
                                    "{name}: MakerNote moved from {:?} to {:?}",
                                    exif.maker_note_offset(),
                                    back.maker_note_offset()
                                );
                            }
                        }
                    }
                    Err(err) => {
                        let _ = writeln!(report, "{name}: preserving output unreadable: {err}");
                    }
                },
                Err(err) => {
                    let _ = writeln!(report, "{name}: preserving write failed: {err}");
                }
            }
        }

        // Without the source: compact, unless image data is referenced.
        match exif.to_tiff() {
            Ok(bytes) => match Exif::from_tiff(&bytes) {
                Ok((back, _)) => {
                    compact += 1;
                    if back != expected {
                        let _ = writeln!(report, "{name}: compact round trip changed the model");
                    }
                }
                Err(err) => {
                    let _ = writeln!(report, "{name}: compact output unreadable: {err}");
                }
            },
            Err(exifkit::Error::Unsupported { .. }) if data_refs => needs_source += 1,
            Err(err) => {
                let _ = writeln!(report, "{name}: compact write failed: {err}");
            }
        }
    }
    eprintln!(
        "corpus: round-tripped {preserved} files with their source ({anchored} MakerNotes kept in \
         place) and {compact} without; {needs_source} need their source for image data"
    );
    assert!(report.is_empty(), "writer round-trip failures:\n{report}");
}

/// Writing into the original file, unedited, gives back the same model, and
/// every byte outside the Exif data is untouched: stripping Exif from the
/// original and from the rewrite gives identical files.
#[test]
fn write_into_round_trips_and_keeps_image_data() {
    let root = root();
    let mut paths = Vec::new();
    goldens(&root.join("tests/golden"), &mut paths);
    paths.sort();
    let external = root.join("tests/fixtures/external");
    let (mut written, mut untouched, mut refused) = (0, 0, 0);
    let mut report = String::new();
    for path in &paths {
        let rel = path
            .strip_prefix(root.join("tests/golden"))
            .unwrap()
            .with_extension("");
        let Some(data) = fixture(&external.join(&rel)) else {
            continue;
        };
        let Ok((exif, _)) = Exif::read(&data) else {
            continue;
        };
        let name = rel.display();
        let out = match exif.write_into(&data) {
            Ok(out) => out,
            Err(exifkit::Error::Unsupported { .. }) => {
                refused += 1;
                continue;
            }
            Err(err) => {
                let _ = writeln!(report, "{name}: write_into failed: {err}");
                continue;
            }
        };
        written += 1;
        let source = exifkit::container::locate(&data)
            .ok()
            .and_then(|(located, _)| {
                let range = located
                    .exif
                    .filter(|_| located.encoding == exifkit::container::Encoding::Raw)?;
                let block = data.get(range.start as usize..(range.end as usize).min(data.len()))?;
                Some(block.strip_prefix(b"Exif\0\0").unwrap_or(block).to_vec())
            });
        let expected = match &source {
            Some(source) => writable_with_source(&exif, source),
            None => writable(&exif),
        };
        match Exif::read(&out) {
            Ok((back, _)) if back == expected => {}
            Ok(_) => {
                let _ = writeln!(report, "{name}: model changed");
            }
            Err(err) => {
                let _ = writeln!(report, "{name}: rewritten file unreadable: {err}");
            }
        }
        // Streaming gives exactly the same file.
        let mut streamed = Vec::new();
        match exif.write_stream(
            std::io::Cursor::new(&data),
            &mut streamed,
            &exifkit::WriteOptions::DEFAULT,
        ) {
            Ok(()) if streamed == out => {}
            Ok(()) => {
                let _ = writeln!(report, "{name}: write_stream differs from write_into");
            }
            Err(err) => {
                let _ = writeln!(report, "{name}: write_stream failed: {err}");
            }
        }
        // XMP and ICC are never touched by an Exif write.
        let extras = |file: &[u8]| {
            (
                exifkit::container::xmp(file)
                    .ok()
                    .flatten()
                    .map(|x| x.into_owned()),
                exifkit::container::icc_profile(file)
                    .ok()
                    .flatten()
                    .map(|x| x.into_owned()),
            )
        };
        if extras(&data) != extras(&out) {
            let _ = writeln!(report, "{name}: XMP or ICC changed");
        }
        if let (Ok(before), Ok(after)) = (exifkit::remove_exif(&data), exifkit::remove_exif(&out)) {
            untouched += 1;
            if before != after {
                let _ = writeln!(report, "{name}: bytes outside the Exif data changed");
            }
        }
    }
    eprintln!(
        "corpus: wrote {written} files back ({untouched} checked byte for byte outside Exif); {refused} unsupported"
    );
    assert!(report.is_empty(), "write_into failures:\n{report}");
}
