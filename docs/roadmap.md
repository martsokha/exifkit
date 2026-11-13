# Roadmap

Status: draft

## Milestones

| #  | Scope                                                                                                                                                                                       | Exit criteria                                                                                                     |
|----|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------|
| M0 | **Done.** Repo scaffold and CI (fmt, clippy, docs, test, `no_std`, codegen check, cargo-deny, nightly fuzzing). `xtask` workspace. `data/tags.toml` with the IFD0/IFD1, Exif, GPS and Interop tags (including Exif 3.0 and 3.1) and their value enums. Generated `Tag` constants, `TagInfo` table and enums. `tiff::Header`/`Type`. `cargo-fuzz` skeleton | CI green. `cargo xtask codegen --check` passes                                                                     |
| M1 | **Done.** Corpus: 4,432 tags across 138 real files match ExifTool 13.55 in both directions. Fuzzing: `tiff_parse` seeded with the corpus, no findings. **Read core.** `tiff` parse layer, owned model, all value types including 13 and 129, graph walk with limits, diagnostics, full tag table for DC-008 3.0, typed `get`, `DateTime`, `Gps`, `Display` | Reads the whole fixture corpus with no panics. Results match `exiftool -j -n` on the compared tags. Fuzz target `tiff_parse` runs for 1 h with no findings |
| M2 | **Done.** Done (PR 1): container reading for JPEG, TIFF, PNG (incl. legacy raw profiles) and WebP; `Exif::read`/`read_from`; corpus grown to 151 files; `read_any` fuzz target. Done (PR 2): the writer (minimal-diff layout with the source, MakerNote anchoring, `OffsetSchema`), editing API, corpus round trips, ExifTool edit comparison, `roundtrip` fuzz target. Done (PR 3): writing into JPEG, TIFF, PNG and WebP files (`write_into`, `remove_exif`), with no new `exiftool -validate` warnings on 127 edited files, plus a `write_into` fuzz target. Done (PR 4): typed `set` with validation and the `ExifVersion` bump, `write_stream` (streams JPEG/PNG/WebP image data), raw XMP/ICC read access (matches ExifTool on 54 XMP packets and 38 ICC profiles), ORF/RW2 magic numbers, ExifTool raw samples (RW2, CR2, NEF, DNG) in the corpus. **Write core + JPEG/TIFF/PNG/WebP.** Encoder (Compact + Preserve), MakerNote anchoring, `OffsetSchema`, thumbnail, JPEG APP1 (with the size limit and MPF check), TIFF rewritten with the file as its own source, PNG `eXIf`, WebP including simple→extended conversion. Raw read access to XMP and ICC profiles (JPEG multi-segment ICC, PNG `iCCP`/`iTXt`, WebP `ICCP`/`XMP `), passed through untouched on write. ORF and RW2 adapters (non-standard TIFF magic) | Round-trip property tests pass. `exiftool -validate` reports no new warnings on our output. Vendor MakerNotes still decode in ExifTool after an edit (on fixtures) |
| M3 | **In progress.** Done (PR 1): JPEG XL read and write (`Exif` and `brob` boxes, bare codestreams wrapped in a container, the Exif size kept for `jbrd` JPEG reconstruction, checked with `djxl`), the shared ISOBMFF box walker, JXL samples in the corpus. Next: HEIF/AVIF read, then write, then CR3. **ISOBMFF.** HEIF/AVIF read and write (append + `iloc` rewrite), JXL `Exif` box, `brob` read, Canon CR3 read (Exif in ISOBMFF `CMT1`–`CMT4` boxes)                                                                                                 | Edited HEIC files open in Apple Photos, libheif and exiftool. AVIF in Chrome. JXL in libjxl `djxl`                |
| M4 | **1.0 polish.** `serde`, `jiff`/`chrono`, `strip` presets, human-readable values with units ("1/250 s", "f/7.1", APEX converted to shutter speed and aperture) from a `unit` column in `tags.toml`, Annex H `prepare_for_edit`, `png-legacy`, docs, examples, benchmarks against `kamadak-exif` / `little_exif`                         | API reviewed, semver commitment                                                                                    |
| Later | Fujifilm RAF and Phase One IIQ, on demand. MakerNote offset fix-up for the main vendors. MPF read API. Raw-format writing. MOV/MP4 Exif. Companion XMP crate                                                         | n/a                                                                                                               |

## Testing

- **Sample corpus** (`tests/corpus.rs`): 138 real-world files (cameras
  from about 20 makers, phones, scanners, DNG/ARW, deliberately broken
  files and `little_exif` crash reports), listed in `data/fixtures.toml`
  and pinned by commit and SHA-256. The images are **never committed**,
  because their licenses vary per file. `cargo xtask fixtures` downloads
  them into the gitignored `tests/fixtures/external/`, and CI caches them.
- **Goldens:** `tests/golden/**.json` holds what ExifTool reads from each
  sample (`-j -D -n -a -u`, IFD0/IFD1/Exif/GPS/Interop/SubIFDs). These
  are metadata facts, so they are committed. Regenerate them with `cargo
  xtask golden` after adding samples or upgrading ExifTool, and review
  the diff. The test checks both directions: every tag ExifTool reports
  must be in exifkit with the same value, and exifkit must not report
  entries ExifTool doesn't. ExifTool's presentation (APEX conversion,
  decimal GPS, hex digests, file-relative offsets, …) is undone in the
  comparison and documented there. Without the images, the test only
  validates the goldens, so `cargo test` works offline.
- **Goldens are never redacted.** They are ExifTool's output unchanged,
  GPS coordinates included. The source files are already public at the
  pinned URLs, and a redacted golden would make mismatches unreadable. To
  keep a file's metadata out of the repository, drop the file from
  `data/fixtures.toml` instead (see the note at its top).
- **Known, intended differences from ExifTool:**
  - Duplicate tags: exifkit uses the first and keeps the rest in
    `Ifd::duplicates`.
  - ExifTool stops reading an IFD at the first unreadable value, while
    exifkit skips only that entry.
- **Own fixtures:** hand-built TIFF blocks in `src/model/tests.rs` cover
  both byte orders, every type, UTF-8, misplaced tags, duplicates,
  cycles, truncation and odd offsets. A deterministic mutation test
  (every truncation, and 7 values written into every byte) runs on every
  `cargo test`.
- **Property tests** (`proptest`): generate an arbitrary `Exif`, then
  check that `to_tiff` followed by `from_tiff` gives back an equal model,
  and that `encoded_len()` equals `to_tiff().len()`. For containers,
  splice a generated block into each fixture, read it back, and check
  that every byte outside the metadata is unchanged (we diff the
  container structure and confirm that the decoded image pixels are
  identical by running `image` in a dev-dependency).
- **Fuzzing** (`cargo-fuzz`, nightly CI for 10 minutes per target, and
  OSS-Fuzz once public), with these targets:
  - `tiff_parse`
  - `read_any` (container detection plus parse)
  - `roundtrip`: parse, write, parse again, and assert the model is
    equal and nothing panics
  - `write_into`: an arbitrary container combined with a valid model

  A memory cap (`-rss_limit_mb=256`) catches the allocation-size bugs
  described in [`prior-art.md`](prior-art.md#3-parser-safety-on-hostile-input).
- **Regression rule:** every bug fix adds a fixture or a fuzz-corpus
  entry.

## Decided

- **Crate name: `exifkit`.** `exif` on crates.io is an abandoned libexif
  wrapper (0.0.1, 2016). The repository directory stays `exif`.
- **MSRV: 1.95**, edition 2024. CI pins the toolchain to the MSRV.
- **Workspace:** the root package is `exifkit`, with `xtask` as the only
  other member. `fuzz/` is a separate workspace (cargo-fuzz needs
  nightly and its own lockfile).
- **Spec target: CIPA DC-008-2026 (Exif 3.1).** `data/tags.toml` has been
  checked against it, and every row is verified. The PDF is copyrighted
  ("All Rights Reserved"), so it is **not** committed. Download it from
  <https://www.cipa.jp/e/std/std-sec.html>.
- **Tag namespaces:** `Main` (shared by IFD0, IFD1, Exif IFD and
  SubIFDs), `Gps` and `Interop`. See [`format.md` §2](format.md#2-ifd-graph).

## Open questions

1. **The `image` crate integration.** Should we provide a feature that
   implements a small adapter (for example applying `Orientation` to a
   `DynamicImage`), or leave that to callers? Leaning towards leaving it
   out, with an example instead.
2. **Lenient vs. strict default.** We currently plan lenient reads with
   diagnostics. Confirm that this is the right default for a library, as
   opposed to strict by default with a lenient opt-in.
