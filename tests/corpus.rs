//! Compares exifkit against ExifTool on real-world sample images.
//!
//! Files are read through `Exif::read`, so the container code (JPEG, TIFF,
//! PNG, WebP) is exercised too. The images are downloaded by `cargo xtask fixtures` (they are not committed);
//! `tests/golden/` holds what ExifTool 13.x reads from each, recorded by
//! `cargo xtask golden`. Without the images this test only checks that the
//! goldens parse, so a plain `cargo test` works offline.
//!
//! For every tag ExifTool reports in IFD0, IFD1, the Exif, GPS and Interop
//! IFDs and SubIFDs, exifkit must hold the same tag, in the same IFD, with
//! the same value. Some tags ExifTool converts even with `-n`; those are
//! compared after the same conversion or skipped (see `skip` and `compare`).

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

use exifkit::tiff::ByteOrder;
use exifkit::{
    EncodedText, Entry, Exif, FromValue, IfdKind, Namespace, Rational, SRational, Tag, Value,
};
use serde_json::Value as Json;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn goldens(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            goldens(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
}

/// Maps an ExifTool family-1 group to the IFDs it can correspond to.
fn ifds_for(group: &str) -> Option<Vec<IfdKind>> {
    Some(match group {
        "IFD0" => vec![IfdKind::Primary],
        "IFD1" => vec![IfdKind::Thumbnail],
        "IFD2" => vec![IfdKind::Chain(2)],
        "ExifIFD" => vec![IfdKind::Exif],
        "GPS" => vec![IfdKind::Gps],
        "InteropIFD" => vec![IfdKind::Interop],
        g if g.starts_with("SubIFD") => (0..64).map(IfdKind::SubIfd).collect(),
        _ => return None,
    })
}

fn namespace(kind: IfdKind) -> Namespace {
    match kind {
        IfdKind::Gps => Namespace::Gps,
        IfdKind::Interop => Namespace::Interop,
        _ => Namespace::Main,
    }
}

/// Tags ExifTool decodes or converts in ways `-n` does not undo, which are
/// not (yet) compared.
fn skip(ns: Namespace, id: u16) -> bool {
    match ns {
        Namespace::Main => matches!(
            id,
            0x9201 | 0x9202 | 0x9205 // APEX values: ExifTool converts to seconds / f-number
            | 0x927C // MakerNote: ExifTool parses it into its own groups
            | 0x9C9B
                ..=0x9C9F // XP*: UCS-2 decoded
            | 0x0201 // ThumbnailOffset: consumed into Exif::thumbnail
            | 0x02BC | 0x83BB | 0x8773 // XMP, IPTC, ICC: parsed into other groups
            | 0xC4A5 | 0xC634 // PrintIM, DNGPrivateData: parsed into other groups
        ),
        _ => false,
    }
}

fn f64s(value: &Value) -> Option<Vec<f64>> {
    let rational = |r: &Rational| f64::from(r.num) / f64::from(r.den);
    let srational = |r: &SRational| f64::from(r.num) / f64::from(r.den);
    Some(match value {
        Value::Byte(v) | Value::Undefined(v) => v.iter().copied().map(f64::from).collect(),
        Value::Short(v) => v.iter().copied().map(f64::from).collect(),
        Value::Long(v) | Value::Ifd(v) => v.iter().copied().map(f64::from).collect(),
        Value::SByte(v) => v.iter().copied().map(f64::from).collect(),
        Value::SShort(v) => v.iter().copied().map(f64::from).collect(),
        Value::SLong(v) => v.iter().copied().map(f64::from).collect(),
        Value::Rational(v) => v.iter().map(rational).collect(),
        Value::SRational(v) => v.iter().map(srational).collect(),
        Value::Float(v) => v.iter().copied().map(f64::from).collect(),
        Value::Double(v) => v.clone(),
        _ => return None,
    })
}

/// ExifTool prints about 10 significant digits.
fn close(a: f64, b: f64) -> bool {
    if a.is_nan() || b.is_nan() || a.is_infinite() || b.is_infinite() {
        return a.is_nan() && b.is_nan() || a == b;
    }
    (a - b).abs() <= 1e-9_f64.max(a.abs().max(b.abs()) * 2e-8)
}

fn parse_numbers(text: &str) -> Option<Vec<f64>> {
    text.split_whitespace()
        .map(|t| match t {
            "inf" => Some(f64::INFINITY),
            "-inf" => Some(f64::NEG_INFINITY),
            "undef" => Some(f64::NAN),
            t => t.parse().ok(),
        })
        .collect()
}

fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::Ascii(_) | Value::Utf8(_) => value
            .text_bytes()
            .map(|b| String::from_utf8_lossy(b).trim_end().to_owned()),
        Value::Undefined(b) | Value::Byte(b) => Some(
            String::from_utf8_lossy(b)
                .trim_end_matches(['\0', ' '])
                .to_owned(),
        ),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The value list as ExifTool prints it, space separated.
fn joined(value: &Value) -> Option<String> {
    let numbers = f64s(value)?;
    Some(
        numbers
            .iter()
            .map(f64::to_string)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Whether exifkit's value matches ExifTool's `-n` value for this tag.
///
/// `base` is where the TIFF block starts in the file: ExifTool reports data
/// offsets (`StripOffsets`, …) relative to the file, exifkit relative to the
/// TIFF block, as stored.
fn compare(tag: Tag, ours: &Value, theirs: &Json, base: f64) -> bool {
    let data_offset = tag
        .info()
        .is_some_and(|i| i.role == exifkit::tag::Role::DataOffset);
    if data_offset
        && base > 0.0
        && let Some(values) = f64s(ours)
    {
        let shifted = Value::Double(values.iter().map(|v| v + base).collect());
        if compare_plain(tag, &shifted, theirs) {
            return true;
        }
    }
    compare_plain(tag, ours, theirs)
}

fn compare_plain(tag: Tag, ours: &Value, theirs: &Json) -> bool {
    // Encoded text: ExifTool decodes the character-code prefix.
    let encoded = tag == Tag::USER_COMMENT
        || tag == Tag::GPS_PROCESSING_METHOD
        || tag == Tag::GPS_AREA_INFORMATION;
    if encoded && let Value::Undefined(bytes) = ours {
        let expected = match theirs {
            Json::String(s) => s.clone(),
            other => other.to_string(),
        };
        return match EncodedText::parse(bytes, ByteOrder::LittleEndian) {
            Ok(text) => text.text().is_some_and(|t| t == expected),
            // Shorter than the prefix: ExifTool shows the raw bytes.
            Err(_) => text_of(ours).is_some_and(|t| t == expected),
        };
    }
    // CFAPattern: ExifTool decodes the two SHORT dimensions, accepting either
    // byte order since vendors disagree, then lists the pattern bytes.
    if tag == Tag::CFA_PATTERN
        && let (Value::Undefined(bytes), Some(text)) = (ours, theirs.as_str())
        && let [a, b, c, d, pattern @ ..] = bytes.as_slice()
    {
        let dims = |x: u16, y: u16| {
            let mut v = vec![f64::from(x), f64::from(y)];
            v.extend(pattern.iter().copied().map(f64::from));
            v
        };
        let be = dims(u16::from_be_bytes([*a, *b]), u16::from_be_bytes([*c, *d]));
        let le = dims(u16::from_le_bytes([*a, *b]), u16::from_le_bytes([*c, *d]));
        return parse_numbers(text).is_some_and(|t| t == be || t == le);
    }
    // GPS coordinates: ExifTool converts degrees/minutes/seconds to degrees.
    let gps_coordinate =
        tag.namespace() == Namespace::Gps && matches!(tag.id(), 0x02 | 0x04 | 0x14 | 0x16);
    if gps_coordinate {
        let (Some(dms), Some(expected)) = (f64s(ours), theirs.as_f64()) else {
            return false;
        };
        let degrees = dms
            .iter()
            .zip([1.0, 60.0, 3600.0])
            .map(|(v, d)| v / d)
            .sum::<f64>();
        return close(degrees, expected) || (dms.len() == 1 && close(dms[0], expected));
    }
    // GPSTimeStamp: ExifTool prints "h:m:s".
    if tag == Tag::GPS_TIME_STAMP
        && let (Some(hms), Some(text)) = (f64s(ours), theirs.as_str())
    {
        let parts: Option<Vec<f64>> = text.split(':').map(|p| p.parse().ok()).collect();
        return parts.is_some_and(|p| {
            p.len() == hms.len() && p.iter().zip(&hms).all(|(a, b)| close(*a, *b))
        });
    }
    match theirs {
        Json::Number(n) => {
            let expected = n.as_f64().unwrap_or(f64::NAN);
            match f64s(ours).as_deref() {
                Some([v]) => close(*v, expected),
                _ => text_of(ours)
                    .is_some_and(|t| t.trim().parse::<f64>().is_ok_and(|v| close(v, expected))),
            }
        }
        Json::String(text) => {
            // "(Binary data N bytes, …)": N is the byte count, or for long
            // number lists the length of the printed list.
            if let Some(len) = text
                .strip_prefix("(Binary data ")
                .and_then(|r| r.split_whitespace().next())
                .and_then(|n| n.parse::<usize>().ok())
            {
                return ours.count() == len || joined(ours).is_some_and(|j| j.len() == len);
            }
            // "(large array of N float values)".
            if let Some(len) = text
                .strip_prefix("(large array of ")
                .and_then(|r| r.split_whitespace().next())
                .and_then(|n| n.parse::<usize>().ok())
            {
                return ours.count() == len;
            }
            // Digests: ExifTool prints BYTE arrays as hex.
            if let Value::Byte(bytes) | Value::Undefined(bytes) = ours
                && hex(bytes).eq_ignore_ascii_case(text)
            {
                return true;
            }
            // ISO 8601 date-times (DNG): ExifTool rewrites them Exif style.
            if let Some(ours_text) = text_of(ours)
                && ours_text.len() >= 19
                && ours_text.as_bytes().get(10) == Some(&b'T')
            {
                let exif_style = ours_text.replacen('-', ":", 2).replacen('T', " ", 1);
                if exif_style == *text {
                    return true;
                }
            }
            if let Some(ours_text) = text_of(ours)
                && (ours_text == text.trim_end() || ours_text.trim() == text.trim())
            {
                return true;
            }
            match (f64s(ours), parse_numbers(text)) {
                (Some(a), Some(b)) => {
                    a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| close(*x, *y))
                }
                _ => false,
            }
        }
        _ => false,
    }
}

fn find<'a>(exif: &'a Exif, kinds: &[IfdKind], tag: u16, copy: usize) -> Option<&'a Entry> {
    kinds.iter().find_map(|&kind| {
        let ifd = exif.ifd(kind)?;
        let tag = Tag::new(namespace(kind), tag);
        if copy == 0 {
            ifd.get(tag)
        } else {
            ifd.duplicates()
                .iter()
                .filter(|e| e.tag() == tag)
                .nth(copy - 1)
        }
    })
}

/// Checks one fixture; returns a description of every mismatch.
fn check(golden: &Json, data: &[u8], stats: &mut Stats) -> Vec<String> {
    let entries = golden["entries"].as_array().cloned().unwrap_or_default();
    let mut problems = Vec::new();
    // ExifTool reports data offsets relative to the file; find where the
    // TIFF block starts so they can be compared.
    let base = match exifkit::container::locate(data) {
        Ok((located, _)) => located.exif.map_or(0.0, |r| r.start as f64),
        Err(_) => 0.0,
    };
    // The streaming reader must agree with the in-memory one exactly.
    let streamed = Exif::read_from(std::io::Cursor::new(data));
    match (&streamed, Exif::read(data)) {
        (Ok(a), Ok(b)) if *a == b => {}
        (Err(a), Err(b)) if a.to_string() == b.to_string() => {}
        (a, b) => problems.push(format!("read_from and read disagree: {a:?} vs {b:?}")),
    }
    let (exif, _diagnostics) = match Exif::read(data) {
        Ok(read) => read,
        Err(exifkit::Error::Unsupported { .. }) if !cfg!(feature = "png-legacy") => {
            // Legacy PNG text chunks need the `png-legacy` feature.
            stats.skipped += entries.len();
            return problems;
        }
        Err(err) => {
            if !entries.is_empty() {
                problems.push(format!(
                    "read failed ({err}), ExifTool reads {} tags",
                    entries.len()
                ));
            }
            return problems;
        }
    };
    for entry in &entries {
        let group = entry["group"].as_str().unwrap_or_default();
        let name = entry["name"].as_str().unwrap_or_default();
        let id = u16::try_from(entry["id"].as_u64().unwrap_or(u64::MAX)).unwrap_or(u16::MAX);
        let copy = usize::try_from(entry["copy"].as_u64().unwrap_or(0)).unwrap_or(0);
        let Some(kinds) = ifds_for(group) else {
            continue;
        };
        let ns = namespace(kinds[0]);
        if skip(ns, id) {
            stats.skipped += 1;
            continue;
        }
        stats.compared += 1;
        let theirs = &entry["value"];
        if ns == Namespace::Main && id == 0xEA1D {
            // OffsetSchema is layout, held apart from the entries.
            let ours = exif.offset_schema().map(f64::from);
            if ours.is_none_or(|o| !theirs.as_f64().is_some_and(|t| close(o, t))) {
                problems.push(format!("OffsetSchema: exifkit {ours:?}, ExifTool {theirs}"));
            }
            continue;
        }
        if ns == Namespace::Main && id == 0x0202 {
            // ThumbnailLength: exifkit extracts the thumbnail itself.
            let ours = exif.thumbnail().map(<[u8]>::len).map(|n| n as f64);
            let entry_len = find(&exif, &kinds, id, copy)
                .and_then(|e| f64s(e.value()))
                .and_then(|v| v.first().copied());
            if ours
                .or(entry_len)
                .is_some_and(|n| theirs.as_f64().is_some_and(|t| close(n, t)))
            {
                continue;
            }
        }
        // A later copy of a tag with no duplicate in exifkit's IFD comes from a
        // second Exif block ExifTool also reads (for example the copy inside
        // Photoshop's image resources), not from a duplicate entry.
        let second_block = copy > 0
            && kinds.iter().all(|&kind| {
                exif.ifd(kind).is_none_or(|ifd| {
                    !ifd.duplicates()
                        .iter()
                        .any(|e| e.tag() == Tag::new(namespace(kind), id))
                })
            });
        if second_block {
            stats.skipped += 1;
            continue;
        }
        match find(&exif, &kinds, id, copy) {
            None => problems.push(format!("missing {group}:{name} (0x{id:04X}) = {theirs}")),
            Some(ours) if !compare(ours.tag(), ours.value(), theirs, base) => {
                problems.push(format!(
                    "{group}:{name} (0x{id:04X}): exifkit {:?}, ExifTool {theirs}",
                    ours.value()
                ))
            }
            Some(_) => {}
        }
    }
    // The other direction: exifkit must not invent entries ExifTool does not
    // see (for example by reading garbage as an IFD).
    let reported: std::collections::HashSet<(String, u64)> = entries
        .iter()
        .filter_map(|e| Some((e["group"].as_str()?.to_owned(), e["id"].as_u64()?)))
        .map(|(g, id)| {
            (
                if g.starts_with("SubIFD") {
                    "SubIFD".to_owned()
                } else {
                    g
                },
                id,
            )
        })
        .collect();
    // ExifTool stops reading an IFD at the first unreadable value; exifkit
    // skips just that entry and keeps the rest, so it may see more there.
    let stopped_early = |group: &str| {
        golden["warnings"].as_array().is_some_and(|w| {
            w.iter()
                .filter_map(Json::as_str)
                .any(|w| w.starts_with(&format!("Error reading value for {group} entry")))
        })
    };
    for (kind, entry) in exif.entries() {
        let group = match kind {
            IfdKind::Primary => "IFD0",
            IfdKind::Thumbnail => "IFD1",
            IfdKind::Chain(2) => "IFD2",
            IfdKind::Exif => "ExifIFD",
            IfdKind::Gps => "GPS",
            IfdKind::Interop => "InteropIFD",
            IfdKind::SubIfd(_) => "SubIFD",
            _ => continue,
        };
        let id = entry.tag().id();
        if skip(entry.tag().namespace(), id) || extra_ok(entry.tag()) || stopped_early(group) {
            continue;
        }
        if !reported.contains(&(group.to_owned(), u64::from(id))) {
            stats
                .extra
                .push(format!("{group}:{} = {}", entry.tag(), entry.value()));
        }
    }
    problems
}

/// Entries exifkit keeps that ExifTool does not list under these groups.
fn extra_ok(tag: Tag) -> bool {
    tag == Tag::JPEG_INTERCHANGE_FORMAT || tag == Tag::JPEG_INTERCHANGE_FORMAT_LENGTH
}

#[derive(Default)]
struct Stats {
    compared: usize,
    skipped: usize,
    extra: Vec<String>,
}

#[test]
fn matches_exiftool_on_the_sample_corpus() {
    let mut stats = Stats::default();
    let root = root();
    let mut paths = Vec::new();
    goldens(&root.join("tests/golden"), &mut paths);
    paths.sort();
    assert!(!paths.is_empty(), "no goldens in tests/golden");

    let external = root.join("tests/fixtures/external");
    let (mut compared, mut missing) = (0, 0);
    let mut report = String::new();
    for path in &paths {
        let text = std::fs::read_to_string(path).expect("golden is readable");
        let golden: Json = serde_json::from_str(&text).expect("golden is valid JSON");
        let rel = path
            .strip_prefix(root.join("tests/golden"))
            .expect("golden under tests/golden")
            .with_extension("");
        let Ok(data) = std::fs::read(external.join(&rel)) else {
            missing += 1;
            continue;
        };
        compared += 1;
        let before = stats.extra.len();
        for problem in check(&golden, &data, &mut stats) {
            let _ = writeln!(report, "{}: {problem}", rel.display());
        }
        for extra in stats.extra.drain(before..).collect::<Vec<_>>() {
            let _ = writeln!(report, "{}: extra {extra}", rel.display());
        }
    }
    if missing > 0 {
        eprintln!("corpus: {missing} fixtures not downloaded; run `cargo xtask fixtures`");
    }
    eprintln!(
        "corpus: compared {} tags in {compared} fixtures against ExifTool ({} skipped)",
        stats.compared, stats.skipped
    );
    assert!(report.is_empty(), "mismatches with ExifTool:\n{report}");
    // Guard against a vacuous pass: with the fixtures present, almost every
    // tag must actually be compared.
    if compared > 0 {
        assert!(stats.compared > 10 * stats.skipped, "too many tags skipped");
    }
}

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
        let Ok(data) = std::fs::read(external.join(&rel)) else {
            continue;
        };
        let Ok((exif, _)) = Exif::read(&data) else {
            continue;
        };
        let expected = writable(&exif);
        let name = rel.display();

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
            Err(exifkit::Error::Unsupported { .. }) => needs_source += 1,
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

/// Runs ExifTool and returns its EXIF and MakerNotes tags, keyed by
/// `family0:family1:name` with copy numbers removed, as sorted `(id, value)`
/// lists (one per copy).
fn exiftool_tags(file: &Path) -> Option<std::collections::BTreeMap<String, Vec<(String, Json)>>> {
    let output = std::process::Command::new("exiftool")
        .args(["-j", "-D", "-n", "-a", "-u", "-G0:1:4"])
        .arg(file)
        .output()
        .ok()?;
    let parsed: Json = serde_json::from_slice(&output.stdout).ok()?;
    let object = parsed.get(0)?.as_object()?;
    let mut tags: std::collections::BTreeMap<String, Vec<(String, Json)>> =
        std::collections::BTreeMap::new();
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
    Some(tags)
}

/// `exiftool -validate` warnings, one per occurrence (`[x3]` counts three),
/// with numbers masked so the same warning at another offset still matches.
fn validate(file: &Path) -> Option<Vec<String>> {
    let output = std::process::Command::new("exiftool")
        .args(["-validate", "-warning", "-a", "-s3"])
        .arg(file)
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
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
    Some(warnings)
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
        let Ok(data) = std::fs::read(&original) else {
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
        if edited
            .insert(exifkit::Entry::new(
                Tag::IMAGE_DESCRIPTION,
                Value::ascii(&description),
            ))
            .is_err()
        {
            continue;
        }
        let Ok(file) = edited.write_into(&data) else {
            continue;
        };
        // Keep the extension: ExifTool names some tags by file type.
        let ext = original
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin");
        let written = tmp.join(format!("{index}.{ext}"));
        std::fs::write(&written, &file).unwrap();
        let (Some(mut before), Some(mut after)) =
            (exiftool_tags(&original), exiftool_tags(&written))
        else {
            continue;
        };
        let edit_key = "EXIF:IFD0:ImageDescription".to_owned();
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
        checked += 1;
        for (key, values) in &before {
            if key.ends_with(":ThumbnailOffset")
                || values.iter().any(|(id, _)| duplicated.contains(id))
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
        let (Some(before), Some(after)) = (validate(&original), validate(&written)) else {
            continue;
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
        let Ok(data) = std::fs::read(external.join(&rel)) else {
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
