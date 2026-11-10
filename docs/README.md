# Docs

Design and specification documents for **exifkit**, a pure-Rust crate for
reading, editing and writing Exif metadata across image containers.
Written for maintainers, future contributors and AI agents working in
this repo.

## Why another Exif crate

The existing Rust crates are each missing something we need:

- Some only read Exif, or write it only in a narrow, experimental way.
- [`little_exif`](https://crates.io/crates/little_exif), the one crate
  with broad read and write support, can panic, hang or run out of memory
  on malformed input, cannot parse Exif 3.0 files that use the new UTF-8
  type, and moves the MakerNote when it rewrites a file, which corrupts it.

The full analysis is in [`prior-art.md`](prior-art.md).

## Index

| File                                 | Contents                                                                    |
|--------------------------------------|-----------------------------------------------------------------------------|
| [`prior-art.md`](prior-art.md)       | Survey of existing crates, with a deep dive into `little_exif` 0.6.23       |
| [`format.md`](format.md)             | Exif/TIFF format reference: byte layout, IFDs, types and Exif 3.x changes   |
| [`architecture.md`](architecture.md) | Goals, non-goals, layering, module layout, features, safety model           |
| [`api.md`](api.md)                   | Public API: data model, reading, typed access, editing, errors              |
| [`containers.md`](containers.md)     | How Exif is found in and written to JPEG, TIFF, PNG, WebP, HEIF/AVIF and JXL |
| [`writing.md`](writing.md)           | Serialization: layout, offsets, MakerNote preservation, size limits         |
| [`roadmap.md`](roadmap.md)           | Milestones, testing and fuzzing strategy, open questions                    |

## Conventions

- One topic per file, named in `kebab-case.md`.
- Each doc starts with a status line: `Status: draft | agreed | implemented`.
- Decisions live in docs, not in issues or PR threads. Link to the doc
  from the issue instead.
- Keep docs in sync with the code. A PR that changes behavior described
  here updates the doc in the same PR.
- Cite the spec by section when a rule comes from it, for example
  "CIPA DC-008 §4.6.2". Mark behavior we chose for compatibility with
  real-world files (and not from the spec) as **quirk**.

## Normative references

| Document                         | Notes                                                                                     |
|----------------------------------|-------------------------------------------------------------------------------------------|
| CIPA DC-008-2026 (Exif 3.1, 2026-01-30) | Primary spec, our target. Also published as JEITA CP-3451H. Download it from <https://www.cipa.jp/e/std/std-sec.html>; it is copyrighted, so never commit it. Summary of the changes: [`format.md` §8](format.md#8-exif-31-changes) |
| CIPA DC-010-2026                 | Exif metadata for XMP. Relevant only to the XMP companion crate                           |
| TIFF 6.0 (Adobe, 1992)           | Base container: IFD structure and the baseline tags in IFD0 and IFD1                      |
| TIFF/EP (ISO 12234-2), DNG 1.7   | SubIFDs, type 13 (IFD), raw files                                                         |
| CIPA DC-007 (MPF)                | Multi-Picture Format in JPEG APP2                                                         |
| PNG 3rd edition (W3C, 2025)      | `eXIf` chunk                                                                              |
| WebP container (RFC 9649)        | `VP8X` + `EXIF` chunk                                                                     |
| ISO/IEC 23008-12 (HEIF), 14496-12 (ISOBMFF) | Exif item, `iinf` / `iloc` / `iref`                                            |
| ISO/IEC 18181-2 (JPEG XL container) | `Exif` and `brob` boxes                                                                |
| ExifTool tag tables              | De-facto reference for tag names, vendor tags and real-world quirks                       |
