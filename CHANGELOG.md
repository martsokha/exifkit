# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- JPEG XL: reading and writing the `Exif` box, including Brotli-compressed
  `brob` boxes (feature `jxl-brob`, which now needs `std`), XMP from the `xml `
  box, and `remove_exif`. A bare codestream is wrapped in a container to carry
  Exif. A file with JPEG reconstruction data (`jbrd`) keeps its Exif payload
  size, so `djxl` can still rebuild the original JPEG.
- `Encoding::Brotli` and `DiagnosticKind::InvalidExifOffset`.

- Typed writing: `Exif::set` with the `IntoValue` trait. The tag table picks the
  field type and checks the value (type, count, enumeration values), returning
  the new `Error::InvalidValue`. UTF-8 text and Exif 3.0+ tags raise
  `ExifVersion` to `"0300"`. Also `Exif::set_date_time_original`, `_digitized`
  and `set_date_time` (writing the sub-second and offset tags too),
  `EncodedText::new`, and `IntoValue` for the generated enums.
- `Exif::write_stream`: the same output as `write_into`, but JPEG, PNG and WebP
  image data is copied through without being held in memory.
- `container::xmp` and `container::icc_profile`: the raw XMP packet and ICC
  profile of JPEG, TIFF, PNG and WebP files.
- Olympus ORF and Panasonic RW2 files (TIFF structures with their own magic
  number). `tiff::Header` gains `magic` and `Header::new`; `write_into` keeps
  the file's magic.
- An `inflate` feature (zlib, for PNG `iCCP` and compressed XMP); `png-legacy`
  now builds on it.

- Writing into image files: `Exif::write_into` and `Exif::write_into_with` for
  JPEG, TIFF, PNG and WebP. Only the Exif data changes, as a minimal diff of
  the file's own block. Also `remove_exif`. JPEG checks the 64 KiB APP1 limit
  (`Error::TooLarge`) and keeps MPF offsets valid. PNG recomputes CRCs and drops
  legacy raw-profile Exif. WebP converts simple files to VP8X and keeps the EXIF
  flag in step.
- A `write_into` fuzz target, and corpus tests:
  - every sample written back reads as the same model;
  - bytes outside the Exif data are unchanged;
  - `exiftool -validate` reports no new warnings after an edit.

### Fixed

- `Limits::max_sub_ifds` now applies per IFD, across all of its SubIFDs tags.
  Before, an IFD with duplicate SubIFDs tags could exceed the limit, and a
  written file then read back with SubIFDs missing.

- Writing: `Exif::to_tiff`, `Exif::to_tiff_with(&WriteOptions)` and
  `Exif::encoded_len`. Without a source block the output is packed. With the
  block the model was read from (`WriteOptions::source`), the layout is a
  minimal diff:
  - unchanged values keep their offsets;
  - bytes the model does not account for (MakerNote data, image strips,
    vendor data referenced by offset) stay in place;
  - removed data is zeroed, not copied.
  On 122 real files, ExifTool reads an edited file exactly like the original
  apart from the edit, MakerNotes included.
- `MakerNotePolicy` (`Anchor`, `RequireAnchored`, `Move`, `Drop`). The tracked
  MakerNote keeps its original offset; when it moves, `OffsetSchema` records
  how far. `Exif::offset_schema` exposes the source value.
- Editing: `Exif::insert`, `remove`, `ifd_mut`, `remove_ifd`, `set_thumbnail`
  and `set_byte_order`; `Ifd::insert`, `remove`, `retain` and
  `clear_duplicates`. `Error::InvalidEntry` rejects tags in the wrong
  namespace, and tags the writer computes (pointers, data offsets,
  `OffsetSchema`).
- `Ifd::parent`.
- A `roundtrip` fuzz target, plus corpus tests: write/read round trips on
  every sample, and an opt-in ExifTool comparison of edited files.

### Changed

- Equality on `Exif`, `Ifd` and `Entry` compares content, not source layout
  (IFD offsets, the MakerNote offset, `OffsetSchema`, value offsets).
- SubIFDs are numbered canonically: by parent, then by order among that
  parent's SubIFDs. Numbers no longer depend on file layout or on failed
  pointers, so they survive a write.
- `OffsetSchema` is no longer an entry; it is `Exif::offset_schema`.
- Exif, GPS and Interop IFDs always report their canonical parent.

- Container reading: `Exif::read` and `Exif::read_with` for files in memory,
  `Exif::read_from` and `Exif::read_from_with` for `Read + Seek` streams
  (reading only the metadata). Supports JPEG, TIFF (including TIFF-based raw
  files such as DNG), PNG (`eXIf`, and with the `png-legacy` feature the
  ImageMagick "Raw profile type exif" text chunks) and WebP. HEIF and JPEG XL
  are detected and reported as unsupported for now.
- `container::detect`, `container::locate`, `Format`, `Located` and `Encoding`.
- Container diagnostics: `DuplicateExif`, `NonStandardExifHeader`,
  `ExifAfterImageData`, `CrcMismatch` and `TruncatedContainer`.
- `Value`, `Entry`, `Ifd` and `Exif` implement `Eq`. Floats compare bit for
  bit, so a value containing NaN equals itself.

- Crate scaffold: `Error`, `Result` and `Limits`, with CI for format, clippy,
  docs, tests, a `no_std` build, generated-code freshness, cargo-deny and
  nightly fuzzing.
- `Tag`, `Namespace` and `TagInfo`, generated from `data/tags.toml`. The table
  covers the IFD0/IFD1, Exif, GPS and Interoperability tags up to Exif 3.1,
  checked against CIPA DC-008-2026, and value enums such as `Orientation`,
  `LightSource` and `AltitudeRef`.
- `tiff::Header`, `tiff::ByteOrder` and `tiff::Type`, including the Exif 3.0
  UTF-8 type.
- `IfdKind`.
- Reading: `Exif::from_tiff` and `Exif::from_tiff_with(ReadOptions)` build an
  owned model from a raw TIFF block, never panicking on malformed input. The
  IFD graph walk detects cycles and enforces depth, IFD, entry, SubIFD and
  allocation limits. Problems are returned as `Diagnostics`; strict mode turns
  errors into `Error`s.
- `Value` with every TIFF and Exif type, including UTF-8 (129) and IFD (13),
  plus `Value::Unknown` for unknown types. Rationals are stored exactly.
- Lossless typed access through `FromValue`: `Exif::get::<T>`, with impls for
  integers, rationals, strings and every generated enum.
- `DateTime`, `Offset` and `SubSec`, with `Exif::date_time_original`,
  `date_time_digitized` and `date_time` merging the sub-second and offset tags.
- `Exif::gps()` with `latitude`, `longitude`, `coordinates` and `altitude`. The
  altitude datum follows the file's `ExifVersion`.
- The zero-copy parse layer: `tiff::Reader`, `tiff::RawIfd`, `tiff::RawEntry`.
- `EncodedText` and `TextEncoding` for `UserComment`, `GPSProcessingMethod` and
  `GPSAreaInformation` (ASCII, UTF-16 with byte-order detection, JIS, undefined),
  and `Copyright` with its photographer and editor parts.
- Corrupt-IFD detection matching ExifTool: an invalid type in the first entry
  drops the IFD, more than 10 bad entries stop it, and zero padding is skipped.
- The `dump` example.
- A real-world sample corpus (`data/fixtures.toml`, downloaded by
  `cargo xtask fixtures`) with ExifTool goldens. exifkit matches ExifTool on
  every compared tag.
- Design documents in `docs/`.
