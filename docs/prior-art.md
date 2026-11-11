# Prior art

Status: draft

A survey of the existing Rust Exif crates and what we take from, or
avoid in, each. The main subject is `little_exif`, the only crate that
claims full read and write support across many containers.

## `little_exif` 0.6.23

- Released 2026-01-13. Repository: <https://github.com/TechnikTobi/little_exif>
- MIT OR Apache-2.0, MSRV 1.65, edition 2021
- About 12.3k lines of Rust, `#![forbid(unsafe_code)]`
- Dependencies (none optional): `brotli`, `crc`, `log`, `miniz_oxide`,
  `paste`, `quick-xml`

Line references below are to the published 0.6.23 sources.

### What it does well (keep)

- **Broad container coverage.** JPEG, TIFF, PNG (`eXIf` and legacy
  `zTXt`/`iTXt` "Raw profile type exif"), WebP (lossless and extended
  only), HEIF/HEIC/AVIF and JPEG XL (including `brob` boxes compressed
  with Brotli).
- **Real-world quirks, collected from issues.** For example, it accepts
  `GPSAltitudeRef` stored as ASCII `"0"`/`"1"` and unsigned rationals
  stored in signed-rational tags. Its `tests/issue_*` corpus is a useful
  source of hostile and odd files. We should use these as test vectors,
  with the license checked per file.
- **Removes Exif duplicated in XMP** when it rewrites PNG, so the XMP
  copy does not silently override the new Exif data.
- **No unsafe code.**

### Problems (avoid)

#### 1. Data model: one enum variant per tag, carrying its value

`ExifTag` is a macro-generated enum. Each of its 147 known tags is a
variant that carries a value in one fixed format, for example
`ImageWidth(Vec<u32>)`. Twelve `Unknown<FORMAT>(value, u16, group)`
variants catch everything else. A few special variants, such as
`StripOffsets(Vec<u32>, Vec<Vec<u8>>)`, also hold side data.

Consequences:

- **One fixed format per tag, but the spec often allows several.**
  `ImageWidth` may be SHORT or LONG, and many tags accept ASCII or UTF-8
  under Exif 3.0. The crate converts other formats to the one it
  expects, and some of those conversions lose data:
  - `INT32U → INT16U` is a plain `as u16` cast, which silently truncates
    (`exif_tag/decode.rs:57`).
  - `INT16U → INT8U` calls `assert_eq!`, so an out-of-range value from
    the file panics (`decode.rs:78`).
  - `RATIONAL64U → RATIONAL64S` round-trips through `f64` and a
    continued-fraction approximation, so the original numerator and
    denominator are lost.
- **Tags are matched by `(id, group)`.** A known tag in an unexpected IFD
  turns into an `Unknown*` variant, and `get_tag` then returns nothing
  for it.
- **The tag identity and its value are fused.** To look up a tag you
  build a dummy value, for example
  `get_tag(&ExifTag::ImageDescription(String::new()))`.
- **The tag table has errors when checked against CIPA DC-008:**

  | Tag                                | Table says     | Spec says                   | Line |
  |------------------------------------|----------------|-----------------------------|------|
  | `SubjectLocation` (0xA214)         | count 1        | count 2                     | 656  |
  | `SpatialFrequencyResponse` (0xA20C) | SHORT          | UNDEFINED                   | 652  |
  | `TransferFunction` (0x012D)        | count 3        | count 3 × 256               | 568  |
  | `ImageWidth` (0x0100)              | LONG only      | SHORT or LONG               | 542  |
  | `ComponentsConfiguration` (0x9101) | any count      | count 4                     | 615  |

- **Tag names follow ExifTool, not the spec.** For example `ModifyDate`
  instead of `DateTime`, `CreateDate` instead of `DateTimeDigitized`, and
  `ExifImageWidth` instead of `PixelXDimension`.
- **None of the Exif 3.0 tags are present**, such as `ImageTitle`,
  `Photographer` and `ImageEditor`.

#### 2. Exif 3.0 UTF-8 and TIFF/EP IFD types are fatal errors

`ExifTagFormat::from_u16` only knows types 1–12. When an entry has any
other type, the whole IFD decode fails with an error
(`ifd/mod.rs:180`). That includes type 129 (UTF-8, Exif 3.0) and type 13
(IFD, TIFF/EP). So a valid Exif 3.0 file containing a single UTF-8 tag
loses all of its metadata. The spec says readers must skip unknown
types, not fail.

#### 3. Parser safety on hostile input

- **The IFD chain is not checked for cycles.** The `while let
  Some(ifd_offset)` loop over the IFD0 → IFD1 → … chain
  (`metadata/mod.rs:307`) keeps no record of visited offsets. If an IFD's
  next-pointer points back at itself, the loop never ends and memory
  grows with every pass.
- **Sub-IFD recursion has no depth limit or cycle check.** `decode_ifd`
  calls itself for `ExifIFD`, `GPS` and `Interop` (`ifd/mod.rs:269`). An
  offset that points back to a parent IFD overflows the stack.
- **Allocation sizes are not checked against the input.** The parser
  calls `vec![0u8; byte_count]` before checking that the bytes exist
  (`ifd/mod.rs:206`, `385`, `420`). `byte_count` comes from the file and
  can be up to about 4 GiB, so a 100-byte file can trigger a 4 GiB
  allocation.
- **Panics reachable from input:**
  - `raw_data[0]` on an empty `GPSAltitudeRef` (`decode.rs:99`)
  - `assert!` and `assert_eq!` in the PNG, WebP and HEIF code
    (`png/mod.rs:642,867,893`, `webp/file.rs:384–389`,
    `heif/container.rs:473`)
  - The doc comments on `new_from_vec`/`new_from_path` themselves say
    they "currently panic" on unsupported input.

#### 4. The writer corrupts files

- **JPEG APP1 length can overflow.** `jpg.rs:37` computes the segment
  length as `2u16 + 6 + exif_vec.len() as u16`. If the Exif data is
  larger than 64 KiB, this panics in debug builds and wraps around in
  release builds, which writes a corrupt JPEG. Nothing checks the
  65 533-byte limit on an APP1 payload.
- **The MakerNote moves on rewrite.** The writer re-serializes the whole
  TIFF structure, so the MakerNote ends up at a new offset. Most vendor
  MakerNotes (Canon, Nikon type 1/2, Sony, Pentax and others) use
  offsets measured from the start of the TIFF header, so after any edit
  they point at the wrong bytes. The writer has no "keep the MakerNote
  where it was" mode and does not write `OffsetSchema`.
- **SubIFDs (0x014A) are not supported.** In DNG and other raw files,
  these hold the full-resolution image, so they are dropped on rewrite.
  MPF (APP2) is not supported either.
- **`reduce_to_a_minimum`** keeps a hard-coded list of tags and drops
  everything else, including Exif and GPS. The name hides how
  destructive it is.

#### 5. API ergonomics

- Inputs are `&Vec<u8>` (not `&[u8]`). The whole file is read into
  memory even when only a few KB of metadata are needed. There is no
  `Read + Seek` path.
- Errors are `std::io::Error` with formatted `String` messages, so a
  caller cannot match on the kind of failure.
- Diagnostics go through `log::warn!`/`log::error!` rather than being
  returned to the caller.
- `FileExtension::PNG { as_zTXt_chunk }` mixes how to write into the
  file-type enum.
- Every dependency is always compiled in (Brotli and quick-xml even for
  a JPEG-only user). There are no feature flags.
- Style: `return` at the end of every function, `nominator` instead of
  numerator, upper-case type names such as `INT16U` and `GENERIC`.

### Verdict

Contributing upstream would mean replacing the data model, the decoder
and the writer, which is a rewrite. Its container code and its
issue-driven test corpus are a useful reference. Its architecture is not
something we can build on.

## Other crates

| Crate                 | Read | Write   | Containers                       | Notes                                                                                                                                      |
|-----------------------|------|---------|----------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------|
| `kamadak-exif`        | yes  | limited | TIFF, JPEG, HEIF, PNG, WebP      | Solid, spec-faithful reader with a good `Tag(Context, u16)` constant design. The writer (`experimental::Writer`) only produces a TIFF block and cannot edit a file in place. The closest model for our read side. |
| `nom-exif`            | yes  | no      | JPEG, HEIF, TIFF, also MOV/MP4 tracks | Built on nom, has async readers. Read-only.                                                                                         |
| `rexif`               | yes  | no      | JPEG, TIFF                       | Old and limited.                                                                                                                           |
| `exif` (libexif-sys)  | yes  | yes     | via libexif                      | C binding, not pure Rust.                                                                                                                  |
| `img-parts`           | n/a  | n/a     | JPEG, PNG, WebP                  | Splices raw Exif/ICC blobs in and out of containers without parsing them. Useful reference for chunk and segment handling.                |
| `image`               | raw  | no      | many                             | `ImageDecoder::exif_metadata()` returns the raw TIFF blob. Our crate should accept that blob directly (`Exif::from_tiff`).                 |

**Takeaways for our design:**

- Adopt the `kamadak-exif` approach to tag identity (a constant per tag,
  kept separate from the value).
- Adopt the `img-parts` approach to containers: change only the metadata
  segment and copy every other byte through untouched.
- Cover the same containers as `little_exif`, and also write MakerNotes,
  SubIFDs and UTF-8 correctly.

## Overlap review (2026-11-11)

Before M2's writers, we checked which crates already do what we are
building, by reading their source, not just their descriptions.

| Crate (version, first release) | Overlaps with | Verdict |
|---|---|---|
| `kamadak-exif` 0.6.2 (2016) | `tiff/`, `model.rs` (read) | Keep ours. Read-only apart from a writer that builds a TIFF block from scratch, so it cannot edit a file. Unknown types such as UTF-8 (129) are kept opaque, not decoded. 0.6.2 (2026-11-05) is a security fix: already-visited IFDs are now skipped to stop O(n³) memory growth, which is the hostile-input class our walker is built around. No diagnostics, no `no_std`, `std::io::BufRead` only. |
| `nom-exif` 3.8 (2024) | reading | Read-only. Depends on nom, regex and chrono. Not a fit. |
| `img-parts` 0.4.2 (2020) | `container/` (read), M2 container writers | The real overlap: JPEG/PNG/RIFF read and write, `no_std`, MPF offsets updated when the Exif segment changes size. See the issues below. Not adopted as a dependency, but used as a reference and a test oracle. |
| `gamut-isobmff` 3.0 (2026-06) | M3 HEIF/AVIF | Strong on safety: forbids `unsafe`, caps payloads at the input size, has typed write errors. But `write` re-serialises the whole file into an "equivalent, not byte-identical" layout (it inlines `idat`), and it rejects valid files using `construction_method` 2 or protected items. That conflicts with our rule of copying untouched bytes. Decide in M3. Useful as a cross-check either way. |
| `mp4-atom` 0.17 | M3 | General MP4/ISOBMFF, 31k lines. More than we need. |
| `c2pa` | embedding metadata in many formats | A full provenance SDK with a large dependency tree. Not a fit. |
| `crc32fast`, `infer` | PNG CRC, format detection | Ours are about 20 lines each. Not worth a dependency. |

### `img-parts` 0.4.2 issues

These are why we don't build on it. Each is worth reporting upstream.

- **JPEG: oversized Exif is dropped silently.** `set_exif` removes the
  old Exif segment, then inserts nothing if the new block is over 64 KB,
  without returning an error. The file loses its metadata. We return
  `Error::TooLarge` (see `writing.md`).
- **WebP: the `EXIF` chunk goes after `XMP `.** `set_exif` appends at
  the end, but the container spec orders EXIF before XMP.
- **WebP: lossless files can be left invalid.** Converting a simple
  lossless (VP8L) file to VP8X is a `TODO`. When the canvas size is
  unknown, it returns without adding `VP8X`, which leaves an `EXIF`
  chunk in a simple-format file.
- **Whole-file, always-on dependencies.** It parses the whole file
  (including JPEG entropy data) into `bytes::Bytes`, with no streaming.
  `bytes`, `crc32fast` and `miniz_oxide` are always compiled in.
- **No legacy PNG raw profiles,** and no diagnostics for duplicates,
  bad CRCs or truncation.

What we take from it: its MPF entry update (`jpeg/mpf.rs`) is a useful
reference for our MPF check, and its `exif()` accessor can serve as a
second opinion on where the Exif block is in the corpus files.

## Memory bounds on hostile lengths

Checked against the published sources (little_exif 0.6.23, kamadak-exif
0.6.2, nom-exif 3.8.0, img-parts 0.4.2, rexif 0.7.6, jxl-oxide 0.12.6):

- **little_exif** allocates from untrusted u32 box and chunk sizes before
  the bytes exist (`vec![0u8; len]` then `read_exact`, in its JXL, WebP, PNG
  and HEIF readers), so a tiny file can force a ~4 GiB allocation. Brotli
  (`brob`) and zlib decompression have no output cap. Writes hold the whole
  file in memory.
- **kamadak-exif** reads lengths with `take(len).read_to_end` on purpose (a
  comment in `util.rs` explains the OOM risk of preallocating), caps HEIF
  Exif at 128 KiB, but reads TIFF input to the end. No compressed metadata.
- **nom-exif** grows buffers as bytes arrive, with a 1 GiB hard cap and
  per-chunk caps on PNG text. Read-only.
- **img-parts** slices a fully resident input with checked splits and caps
  `iCCP` inflation at 16 MiB.
- **rexif** reads the whole file, then bounds-checks before `try_reserve`.
- **jxl-oxide** streams `brob` input, but found no output cap.

exifkit: buffers grow as bytes arrive (`read_range`), decoded metadata is
capped at 64 MiB (`METADATA_LIMIT`), `brob` is decompressed straight from
the stream, and `write_stream` holds only metadata, except for TIFF, whose
whole file is the writer's source.
