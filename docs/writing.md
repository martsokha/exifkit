# Writing

Status: implemented (layout, MakerNote, thumbnail); draft (required tags, size limits)

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
   The model cannot hold them: `Ifd::insert` rejects them with
   `Error::InvalidEntry`.
3. Byte order stays the same as the source, unless the caller sets
   `WriteOptions::byte_order`.
4. Values are written exactly as the model holds them. No numeric
   conversion happens on write.
5. **Reading the output gives back an equal model** (`==`), apart from
   what cannot be written (see "What a round trip keeps"). This is
   fuzzed, and checked on the whole corpus.

## Layout

Status: implemented (M2). The design first had two modes, Preserve and
Compact. What we built is simpler and safer: one planner, and how much it
keeps in place depends on whether it has the block the model was read
from.

```rust
let bytes = exif.to_tiff()?;                                   // no source: compact
let bytes = exif.to_tiff_with(&WriteOptions::DEFAULT.source(&original))?; // minimal diff
```

The planner decides every offset before writing anything, so serializing
is one forward pass and `encoded_len` is exact. An allocator places IFD
tables and values in write order:

```
header(8) → IFD0 → Exif → Interop → GPS → SubIFDs → IFD1 → IFD2… → thumbnail
```

It steps over **reserved ranges**, and everything below is a way of
filling that list.

### Without a source: compact

Everything is packed in order. The MakerNote is still anchored at its
original offset when the model knows it (see below). Image data the model
only references by offset (strips, tiles, an unextracted thumbnail)
cannot be relocated, so `to_tiff` returns `Error::Unsupported` and asks
for the source.

### With the source: minimal diff

The reader records which bytes of the block the model fully accounts for:
IFD tables, out-of-line values, and the extracted thumbnail. It also
records each value's offset. When writing with that same block:

- **Bytes the model does not account for stay where they are.** That
  covers MakerNote data, image strips and tiles, vendor data referenced
  by offset (Sony `SR2Private`, a Nikon MakerNote embedded in
  `DNGPrivateData`), and padding. They are copied from the source.
- **Unchanged values stay at their original offsets.** Data that points
  into them stays valid, even when we don't know it is a pointer.
- **Only new and changed values, and IFD tables, use free space.** Free
  space is the accounted ranges whose content is no longer there.
- **Removed data does not leak.** Free space that nothing reuses is
  written as zeros, so a removed description or GPS block is gone from
  the output, not just unreferenced.

If the source was not the block the model was read from (the length
differs), only the image data the model references is kept in place.

Verified: on 122 real files, after an edit that forces relocations (a
320-byte `ImageDescription`), ExifTool reads every EXIF and MakerNotes
tag exactly as in the original (`tests/corpus.rs`,
`exiftool_sees_only_the_edit_in_rewritten_files`). That includes Canon,
Nikon (including inside DNG), Sony, Olympus, Fujifilm, Pentax and
Panasonic MakerNotes.

## MakerNote

```rust
#[non_exhaustive]
pub enum MakerNotePolicy {
    Anchor,          // default: keep the original offset, else move + OffsetSchema
    RequireAnchored, // keep the original offset, or fail
    Move,            // pack it with everything else + OffsetSchema (smallest)
    Drop,            // leave it out
}
```

- **Which MakerNote.** The reader tracks one MakerNote: the first
  out-of-line one, in the IFD it came from (normally the Exif IFD).
  Other MakerNote entries, such as duplicates elsewhere, are ordinary
  values.
- **Anchoring.** The tracked MakerNote is reserved at its original offset
  when the offset is past the header and doesn't collide with data kept
  from the source. This works without the source too.
- **`OffsetSchema` (0xEA1D, SLONG, Exif IFD)** is layout, not content.
  The reader keeps it apart from the entries
  (`Exif::offset_schema`), and the writer rewrites it:
  - anchored: kept unchanged;
  - moved: `previous + (new − original)`, so repeated moves accumulate;
  - dropped, or no MakerNote out of line: omitted.

How much a move hurts depends on the vendor:

| Kind | Examples | When moved |
|---|---|---|
| Self-contained (own TIFF header or relative offsets) | Nikon type 3, Fujifilm, Panasonic, Apple, Olympus type 2 | Safe |
| Offsets relative to the main TIFF header | Canon, Sony, Pentax, Nikon types 1/2, Olympus type 1 | Breaks, unless the reader honours `OffsetSchema` |

Fixing up the internal offsets of the second group (as ExifTool does) is
still out of scope; see [`roadmap.md`](roadmap.md).

## What a round trip keeps

Reading what was written gives an **equal model** (`==` compares content,
not layout). Three things cannot be written, and are dropped:

- **Entries of unknown type:** their size, and so their data, is unknown.
- **Duplicate tags:** a TIFF IFD holds each tag once, so the first is
  kept.
- **Data references that already pointed outside the source** (strips,
  tiles, an unextracted thumbnail), when writing with the source. Written
  back unchanged, they would point at unrelated bytes in the output.

The structure is canonical:
- Exif and GPS hang off IFD0, and Interop hangs off Exif. Parents that
  are needed but missing are created (an empty Exif IFD for an Interop
  IFD, IFD1 for a thumbnail).
- SubIFDs keep their parent, and are numbered by tree position, so the
  numbering survives a round trip.
- Writing what was read back gives the same bytes again.

All of these properties are fuzzed by the `roundtrip` target.

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
