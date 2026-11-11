# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
