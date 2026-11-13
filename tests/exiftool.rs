//! Checks against a live ExifTool: an edit is the only change ExifTool sees,
//! with no new `-validate` warnings, and the XMP and ICC exifkit extracts
//! match ExifTool's. Ignored by default: needs `exiftool` on PATH and the
//! downloaded samples.

// A test harness over trusted, pinned inputs: the library's no-panic lints
// (docs/architecture.md#safety-model) do not apply here.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use common::{fixture, goldens, root};
use exifkit::{Exif, FromValue, Tag, Value};
use serde_json::Value as Json;

mod common;

/// Runs ExifTool on `file` and returns its standard output. These tests
/// are run on purpose, so ExifTool missing from PATH fails them.
fn exiftool(args: &[&str], file: &Path) -> Vec<u8> {
    std::process::Command::new("exiftool")
        .args(args)
        .arg(file)
        .output()
        .unwrap_or_else(|err| panic!("running exiftool (is it on PATH?): {err}"))
        .stdout
}

/// Tags read from one file, keyed by `family0:family1:name`.
type Tags = std::collections::BTreeMap<String, Vec<(String, Json)>>;

/// Runs ExifTool and returns its EXIF and MakerNotes tags, keyed by
/// `family0:family1:name` with copy numbers removed, as sorted `(id, value)`
/// lists (one per copy).
fn exiftool_tags(file: &Path) -> Result<Tags, String> {
    let output = exiftool(&["-j", "-D", "-n", "-a", "-u", "-G0:1:4"], file);
    let parsed: Json = serde_json::from_slice(&output)
        .map_err(|err| format!("exiftool -j output is not JSON: {err}"))?;
    let object = parsed
        .get(0)
        .and_then(Json::as_object)
        .ok_or("exiftool -j output has no object")?;
    let mut tags = Tags::new();
    for (key, item) in object {
        if !(key.starts_with("EXIF:") || key.starts_with("MakerNotes:")) {
            continue;
        }
        let key: Vec<&str> = key
            .split(':')
            .filter(|part| !part.starts_with("Copy"))
            .collect();
        let id = item.get("id").map(ToString::to_string).unwrap_or_default();
        let value = item.get("val").cloned().unwrap_or(Json::Null);
        tags.entry(key.join(":")).or_default().push((id, value));
    }
    for list in tags.values_mut() {
        list.sort_by_key(|(id, value)| (id.clone(), value.to_string()));
    }
    Ok(tags)
}

/// `exiftool -validate` warnings, one per occurrence (`[x3]` counts three),
/// with numbers masked so the same warning at another offset still matches.
fn validate(file: &Path) -> Result<Vec<String>, String> {
    let output = exiftool(&["-validate", "-warning", "-a", "-s3"], file);
    let text = String::from_utf8(output)
        .map_err(|err| format!("exiftool -validate output is not UTF-8: {err}"))?;
    let mask = |s: &str| {
        s.split(' ')
            .map(|word| {
                if word.chars().all(|c| c.is_ascii_digit()) && !word.is_empty() {
                    "N"
                } else {
                    word
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut warnings = Vec::new();
    // The first line is the "N Warnings" summary.
    for line in text
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        let (warning, times) = match line.rsplit_once(" [x") {
            Some((warning, count)) => (warning, count.trim_end_matches(']').parse().unwrap_or(1)),
            None => (line, 1),
        };
        warnings.extend(std::iter::repeat_n(mask(warning), times));
    }
    Ok(warnings)
}

/// After an edit (a long new `ImageDescription`, which moves data around),
/// ExifTool must read the file exactly like the original apart from that
/// edit: every EXIF and MakerNotes tag, so vendor MakerNotes still decode.
/// The rewrite must also add no `exiftool -validate` warnings.
/// Needs `exiftool` on PATH and `cargo xtask fixtures`.
///
/// Not compared: `ThumbnailOffset` (the thumbnail moves; its bytes are
/// compared as `ThumbnailImage`), tags the source duplicates (a TIFF IFD
/// holds each tag once, so the writer keeps the first; a duplicated
/// MakerNote takes its decoded tags with it), MakerNotes ExifTool itself
/// reports as having incorrect offsets (they read garbage outside the
/// MakerNote), and files where ExifTool stops reading an IFD early
/// (exifkit reads past the bad entry).
#[test]
#[ignore = "needs exiftool and the downloaded samples"]
fn exiftool_sees_only_the_edit_in_rewritten_files() {
    let root = root();
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("rewritten");
    std::fs::create_dir_all(&tmp).unwrap();
    let mut paths = Vec::new();
    goldens(&root.join("tests/golden"), &mut paths);
    paths.sort();
    let external = root.join("tests/fixtures/external");
    let (mut checked, mut tags) = (0, 0);
    let mut report = String::new();
    for (index, path) in paths.iter().enumerate() {
        let rel = path
            .strip_prefix(root.join("tests/golden"))
            .unwrap()
            .with_extension("");
        let golden: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let stopped_early = golden["warnings"].as_array().is_some_and(|w| {
            w.iter()
                .filter_map(Json::as_str)
                .any(|w| w.starts_with("Error reading value"))
        });
        if stopped_early {
            continue;
        }
        // ExifTool's own check: a MakerNote whose internal offsets already
        // point outside it in the original reads garbage there, and that
        // garbage changes whenever the surrounding data is rewritten.
        let broken_maker_note = golden["warnings"].as_array().is_some_and(|w| {
            w.iter()
                .filter_map(Json::as_str)
                .any(|w| w.contains("incorrect maker notes offsets"))
        });
        let original = external.join(&rel);
        let Some(data) = fixture(&original) else {
            continue;
        };
        let Ok((exif, _)) = Exif::read(&data) else {
            continue;
        };
        // An edit that forces relocation: a long new value in IFD0. It must
        // be the only difference ExifTool sees.
        let description = "exifkit ".repeat(40);
        let mut edited = exif.clone();
        let _ = edited.remove(Tag::IMAGE_DESCRIPTION);
        if let Err(err) = edited.insert(exifkit::Entry::new(
            Tag::IMAGE_DESCRIPTION,
            Value::ascii(&description),
        )) {
            let _ = writeln!(report, "{}: the edit failed: {err}", rel.display());
            continue;
        }
        // Skipped: formats exifkit does not write, and containers the long
        // description does not fit (a JPEG APP1 segment is at most 64 KiB; a
        // JXL file with `jbrd` keeps its Exif size). Any other failure is a
        // regression.
        let file = match edited.write_into(&data) {
            Ok(file) => file,
            Err(exifkit::Error::Unsupported { .. } | exifkit::Error::TooLarge { .. }) => continue,
            Err(err) => {
                let _ = writeln!(report, "{}: writing the edit failed: {err}", rel.display());
                continue;
            }
        };
        // Keep the extension: ExifTool names some tags by file type.
        let ext = original
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin");
        let written = tmp.join(format!("{index}.{ext}"));
        std::fs::write(&written, &file).unwrap();
        let (mut before, mut after) = match (exiftool_tags(&original), exiftool_tags(&written)) {
            (Ok(before), Ok(after)) => (before, after),
            (Err(err), _) | (_, Err(err)) => {
                let _ = writeln!(report, "{}: {err}", rel.display());
                continue;
            }
        };
        // Found by tag id: ExifTool's RW2 table has no name for 0x010E.
        let id = Tag::IMAGE_DESCRIPTION.id().to_string();
        let edit_key = after
            .iter()
            .find(|(k, v)| k.starts_with("EXIF:IFD0:") && v.iter().any(|(i, _)| *i == id))
            .map_or_else(
                || "EXIF:IFD0:ImageDescription".to_owned(),
                |(k, _)| k.clone(),
            );
        match after.remove(&edit_key) {
            Some(values)
                if values.iter().any(|(_, v)| {
                    v.as_str().map(str::trim_end) == Some(description.trim_end())
                }) => {}
            other => {
                let _ = writeln!(
                    report,
                    "{}: the edit is not visible: {other:?}",
                    rel.display()
                );
            }
        }
        before.remove(&edit_key);
        let duplicated: std::collections::BTreeSet<String> = exif
            .ifds()
            .iter()
            .flat_map(|ifd| ifd.duplicates().iter().map(|e| e.tag().id().to_string()))
            .collect();
        let maker_note_duplicated = duplicated.contains(&Tag::MAKER_NOTE.id().to_string());
        // The writer drops strip and tile references that point past the end
        // of the file (stripped samples), so those may go missing.
        let dangling = exif.ifds().iter().any(|ifd| {
            [Tag::STRIP_OFFSETS, Tag::TILE_OFFSETS].iter().any(|&tag| {
                ifd.get(tag)
                    .and_then(|e| Vec::<u32>::from_value(e.value()).ok())
                    .is_some_and(|offsets| offsets.iter().any(|&o| o as usize >= data.len()))
            })
        });
        let data_refs = ["273", "279", "324", "325"];
        checked += 1;
        for (key, values) in &before {
            if key.ends_with(":ThumbnailOffset")
                || values.iter().any(|(id, _)| duplicated.contains(id))
                || (dangling
                    && !after.contains_key(key)
                    && values
                        .iter()
                        .all(|(id, _)| data_refs.contains(&id.as_str())))
                || ((maker_note_duplicated || broken_maker_note) && key.starts_with("MakerNotes:"))
            {
                continue;
            }
            tags += values.len();
            match after.get(key) {
                Some(v) if v == values => {}
                Some(v) => {
                    let _ = writeln!(report, "{}: {key}: {values:?} -> {v:?}", rel.display());
                }
                None => {
                    let _ = writeln!(report, "{}: {key} missing ({values:?})", rel.display());
                }
            }
        }
        for key in after.keys().filter(|k| !before.contains_key(*k)) {
            let _ = writeln!(report, "{}: {key} added", rel.display());
        }
        // The rewrite must not add `exiftool -validate` warnings.
        let (before, after) = match (validate(&original), validate(&written)) {
            (Ok(before), Ok(after)) => (before, after),
            (Err(err), _) | (_, Err(err)) => {
                let _ = writeln!(report, "{}: {err}", rel.display());
                continue;
            }
        };
        for warning in after.iter().filter(|w| {
            after.iter().filter(|x| x == w).count() > before.iter().filter(|x| x == w).count()
        }) {
            let _ = writeln!(
                report,
                "{}: new validation warning: {warning}",
                rel.display()
            );
        }
    }
    eprintln!("exiftool: compared {tags} tags in {checked} rewritten files");
    assert!(
        report.is_empty(),
        "ExifTool reads rewritten files differently:\n{report}"
    );
}

/// `exiftool -b -<tag>`: the raw bytes ExifTool extracts, or `None`.
fn exiftool_binary(file: &Path, tag: &str) -> Option<Vec<u8>> {
    let output = exiftool(&["-b", &format!("-{tag}")], file);
    (!output.is_empty()).then_some(output)
}

/// The XMP packet and ICC profile exifkit extracts match ExifTool's, byte
/// for byte. Needs ExifTool, so it is ignored by default.
#[test]
#[ignore = "needs exiftool"]
fn xmp_and_icc_match_exiftool() {
    let root = root();
    let mut paths = Vec::new();
    goldens(&root.join("tests/golden"), &mut paths);
    paths.sort();
    let external = root.join("tests/fixtures/external");
    let (mut xmp, mut icc) = (0, 0);
    let mut report = String::new();
    for path in &paths {
        let rel = path
            .strip_prefix(root.join("tests/golden"))
            .unwrap()
            .with_extension("");
        let file = external.join(&rel);
        let Some(data) = fixture(&file) else {
            continue;
        };
        let name = rel.display();
        for (tag, ours, count) in [
            ("XMP", exifkit::container::xmp(&data), &mut xmp),
            (
                "ICC_Profile",
                exifkit::container::icc_profile(&data),
                &mut icc,
            ),
        ] {
            let theirs = exiftool_binary(&file, tag);
            match (ours, theirs) {
                (Ok(Some(ours)), Some(theirs)) if *ours == *theirs => *count += 1,
                // JPEG XL keeps its ICC profile in the codestream, which
                // neither tool decodes.
                (Ok(None) | Err(exifkit::Error::Unsupported { .. }), None) => {}
                (Ok(ours), theirs) => {
                    let _ = writeln!(
                        report,
                        "{name}: {tag}: exifkit {:?} bytes, ExifTool {:?} bytes",
                        ours.map(|o| o.len()),
                        theirs.map(|t| t.len())
                    );
                }
                (Err(err), theirs) => {
                    let _ = writeln!(
                        report,
                        "{name}: {tag}: exifkit failed ({err}), ExifTool {:?} bytes",
                        theirs.map(|t| t.len())
                    );
                }
            }
        }
    }
    eprintln!("extras: {xmp} XMP packets and {icc} ICC profiles match ExifTool");
    assert!(report.is_empty(), "extraction mismatches:\n{report}");
}
