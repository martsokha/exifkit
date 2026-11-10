# Writing

Status: draft

How `Exif` turns back into a TIFF block. This document covers the
TIFF-level layout only. How the block is spliced into each image format
is in [`containers.md`](containers.md).

## Invariants

The encoder always produces a block that satisfies these:

1. The output is a valid TIFF/Exif structure. Entries are sorted, all
   offsets are in bounds, value data starts on an even offset, and
   inline values are zero-padded.
2. Every pointer and length tag the writer manages is recomputed:
   `ExifIFDPointer`, `GPSInfoIFDPointer`, `InteroperabilityIFDPointer`,
   `SubIFDs`, `JPEGInterchangeFormat`/`Length` and `OffsetSchema`.
   Values the caller put in those tags are ignored. In debug builds
   this is a `debug_assert`; in release builds it produces a diagnostic.
3. Byte order stays the same as the source, unless the caller sets
   `WriteOptions::byte_order`.
4. Values are written exactly as the model holds them. No numeric
   conversion happens on write.
5. **Reading the output gives back an equal model** (`==`), apart from
   the recomputed pointers. This is property-tested.

## Layout modes

```rust
#[non_exhaustive]
pub enum Layout {
    /// Default. Keep the original structure where possible; anchor the MakerNote.
    Preserve,
    /// Fresh, tightly packed layout (smallest output). Moves the MakerNote → OffsetSchema.
    Compact,
}
```

### Compact

Write everything from scratch, in one pass in a fixed order. Each IFD's
entry table is followed immediately by its out-of-line value data:

```
header(8) → IFD0 entries → IFD0 data → Exif IFD → Exif data → [MakerNote]
          → Interop IFD → GPS IFD → SubIFDs → IFD1 entries → IFD1 data → thumbnail
```

The planner works out every offset before anything is written: IFD
sizes are known from entry counts and value sizes. So the serializer is
one forward pass with no back-patching. The thumbnail goes last, so it
is easy to drop when trying to fit inside JPEG's 64 KiB limit.

### Preserve (default)

The goal is to keep the MakerNote's absolute offset when the edit
allows it, and to produce minimal diffs.

1. Start from the `SourceLayout` captured on read: the original offset
   and length of every IFD, value blob, MakerNote and thumbnail.
2. **Anchor the MakerNote** at its original TIFF-relative offset `M`.
   Treat `[M, M+len)` as reserved space.
3. Lay out everything else in compact order, skipping around the
   reserved range. Data that fits before `M` goes there. The rest goes
   after `M+len`. Any gap left before `M` is filled with zeros. The
   `Padding` tag (0xEA1C) is **not** used for this, because it is a
   Microsoft convention for a different purpose.
4. If the other data cannot fit before `M` without overlapping it (for
   example the caller added large tags), fall back to:
   - (a) moving the MakerNote to the end, and
   - (b) writing `OffsetSchema = new_offset − old_offset` (SLONG, in the
     Exif IFD), and
   - (c) reporting the diagnostic `MakerNoteMoved`.

   If `WriteOptions::makernote = MakerNotePolicy::RequireAnchored`, the
   writer returns an error instead.

If there was no source (a new `Exif` built from scratch), `Preserve`
behaves the same as `Compact`.

## MakerNote

```rust
pub struct MakerNote {
    data: Vec<u8>,
    original_offset: Option<u32>,  // TIFF-relative, from read
    vendor: Option<Vendor>,        // detected from header/Make, informational in v1
}

#[non_exhaustive]
pub enum MakerNotePolicy {
    Anchor,          // default: Preserve-mode behavior above
    RequireAnchored, // error rather than move
    Drop,            // remove the MakerNote entirely
}
```

How serious it is to move a MakerNote depends on the vendor:

| Kind                                    | Examples                                  | When moved                                      |
|-----------------------------------------|-------------------------------------------|-------------------------------------------------|
| Self-contained (own TIFF header / relative offsets) | Nikon type 3, Fujifilm, Panasonic, Apple, Olympus type 2 | Safe                       |
| Offsets relative to the main TIFF header | Canon, Sony, Pentax, Nikon types 1/2, Olympus type 1 | Breaks, unless the reader honours `OffsetSchema` |

In v1 we only *detect* the vendor, so we can report a severity: moving a
safe vendor's MakerNote produces an informational diagnostic, moving any
other produces a warning. Fixing up the internal offsets of the second
group (as ExifTool does) is out of scope for v1, and noted in
[`roadmap.md`](roadmap.md).

## Thumbnail

- `Thumbnail::Jpeg(bytes)`: written as a blob, with `0x0201`/`0x0202`
  set, `Compression = 6` in IFD1, and `XResolution`, `YResolution` and
  `ResolutionUnit` kept from the source (or added with defaults of 72
  dpi if missing, because they are mandatory in IFD1).
- `Thumbnail::Strips { … }`: round-tripped unchanged.
- If there is no thumbnail and IFD1 has no other entries, IFD1 is left
  out and IFD0's next-pointer is 0.
- `set_thumbnail` checks only that the bytes start with SOI and end with
  EOI. The caller is responsible for the size, which the spec recommends
  be 160×120.

## Required tags

The writer **does not** add tags silently. `Exif::validate()` returns
the list of violations, checked against each tag's `requirement` in the
tag table (`Mandatory` in CIPA tables 4–18, for example `ExifVersion`
and `ComponentsConfiguration`). Callers who want spec-conformant output
call `exif.fill_mandatory(&FillOptions)`, which sets `ExifVersion` to
`"0300"`, `FlashpixVersion` to `"0100"`, `ColorSpace` and the rest.

One exception: if the caller writes any tag that exists only in Exif 3.0
(type UTF-8 or a 3.0 tag id), and the model has `ExifVersion < 0300`,
the writer raises the version to `"0300"` and reports a diagnostic. If
it did not, readers that follow the declared version could ignore those
tags.

## Size limits

`Exif::encoded_len(&WriteOptions)` returns the exact output size without
writing anything, so a container can decide early whether the block fits
(see JPEG in [`containers.md`](containers.md#jpeg)). With
`shrink_to_fit`, the writer tries these in order:

1. the compact layout;
2. dropping the thumbnail;
3. giving up with `TooLarge`.

It never removes tags without being asked.
