# exifkit

[![Build](https://img.shields.io/github/actions/workflow/status/martsokha/exifkit/build.yml?branch=main&label=build%20%26%20test&style=flat-square)](https://github.com/martsokha/exifkit/actions/workflows/build.yml)
[![Crate](https://img.shields.io/crates/v/exifkit.svg?style=flat-square)](https://crates.io/crates/exifkit)
[![Docs](https://img.shields.io/docsrs/exifkit?style=flat-square)](https://docs.rs/exifkit)

Read, edit and write Exif metadata in pure Rust.

> **Status:** early development. Reading, editing and writing work for JPEG,
> TIFF (including DNG, NEF, CR2, ORF, RW2 and other TIFF-based raw files), PNG,
> WebP and JPEG XL. HEIF/AVIF is next. See
> [`docs/`](https://github.com/martsokha/exifkit/tree/main/docs) for the
> design and [`docs/roadmap.md`](https://github.com/martsokha/exifkit/blob/main/docs/roadmap.md)
> for progress.

## Features

- **Spec-faithful**: targets CIPA DC-008-2026 (Exif 3.1) and TIFF 6.0,
  including the Exif 3.0 UTF-8 type. Real-world deviations are handled explicitly.
- **Robust on hostile input**: never panics, terminates on cyclic IFDs and
  caps memory use. Malformed data produces diagnostics and a partial
  result, not a failed parse.
- **Read and write in every common container**: JPEG, TIFF, PNG, WebP,
  HEIF/HEIC/AVIF and JPEG XL. Only the metadata segment is rewritten.
- **Typed and checked**: `exif.set(Tag::ORIENTATION, Orientation::Rotate90)`
  picks the field type from the tag table and rejects values the spec does
  not allow; text becomes ASCII or Exif 3.0 UTF-8 as needed.
- **Lossless edits**: only the Exif data is rewritten, as a minimal diff:
  unchanged values, MakerNotes and data exifkit does not understand keep
  their offsets, so vendor MakerNotes still decode. Removed data is zeroed,
  not left behind. Rationals are stored exactly.
- **Streaming**: `read_from` reads only the metadata from any `Read + Seek`,
  and `write_stream` copies image data through without holding it in memory.
- **XMP and ICC**: the raw XMP packet and ICC profile are readable, and
  always preserved byte for byte.
- **Small**: no dependencies by default, and `no_std + alloc` compatible.

## Usage

```rust,no_run
use exifkit::tag::Orientation;
use exifkit::{Exif, IfdKind, Tag};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // JPEG, TIFF, PNG or WebP; the format is detected. `Exif::read_from`
    // takes any `Read + Seek` and reads only the metadata.
    let data = std::fs::read("photo.jpg")?;
    let (exif, diagnostics) = Exif::read(&data)?;

    let orientation: Option<Orientation> = exif.get(Tag::ORIENTATION)?;
    let taken = exif.date_time_original()?; // DateTimeOriginal + sub-seconds + offset
    let position = exif.gps().coordinates(); // decimal degrees

    println!("{orientation:?} {taken:?} {position:?}");
    for entry in exif.entries().map(|(_, entry)| entry) {
        println!("{entry}"); // e.g. "Orientation: Rotate 90 CW"
    }
    // Malformed data never panics; problems are reported here instead.
    for diagnostic in &diagnostics {
        eprintln!("warning: {diagnostic}");
    }

    // Edit and write back. Only the Exif data changes: image data, the
    // MakerNote and everything exifkit does not understand stay as they were.
    let mut exif = exif;
    exif.set(Tag::ARTIST, "Jane Doe")?; // checked against the tag table
    exif.set(Tag::ORIENTATION, Orientation::Normal)?;
    exif.remove_ifd(IfdKind::Gps); // strip the location
    std::fs::write("photo.jpg", exif.write_into(&data)?)?;
    Ok(())
}
```

## Specification

exifkit targets **CIPA DC-008-2026 (Exif 3.1)**, also published as JEITA
CP-3451H, and reads every earlier Exif version. The standard is free to
download from [CIPA's standards page](https://www.cipa.jp/e/std/std-sec.html).
It is copyrighted, so it is not included in this repository. Design notes
and a summary of the format live in
[`docs/`](https://github.com/martsokha/exifkit/tree/main/docs).

## Feature flags

| Feature      | Default | Enables                                              |
|--------------|---------|------------------------------------------------------|
| `std`        | yes     | `Read + Seek` APIs and `std::error::Error`           |
| `jpeg`       | yes     | JPEG container                                       |
| `tiff`       | yes     | TIFF container, and TIFF-based raw files (DNG, ORF, RW2…) |
| `png`        | yes     | PNG `eXIf` chunk                                     |
| `webp`       | yes     | WebP container                                       |
| `heif`       | yes     | HEIF/HEIC/AVIF container                             |
| `jxl`        | yes     | JPEG XL `Exif` box                                   |
| `inflate`    | no      | zlib, for PNG ICC profiles and compressed XMP        |
| `png-legacy` | no      | Legacy PNG text-chunk "Raw profile type exif" (and `xmp`) |
| `jxl-brob`   | no      | Brotli-compressed JPEG XL `brob` boxes (needs `std`) |
| `serde`      | no      | `Serialize`/`Deserialize` for the model              |
| `jiff`       | no      | Conversions to and from `jiff` date-time types       |
| `chrono`     | no      | Conversions to and from `chrono` date-time types     |

## Changelog

See [CHANGELOG.md](https://github.com/martsokha/exifkit/blob/main/CHANGELOG.md)
for release notes and version history.

## License

Licensed under the [MIT License](https://github.com/martsokha/exifkit/blob/main/LICENSE).
