# Containers

Status: draft

How each format stores Exif, how we find it and how we replace it. The
same rule applies to every format: **change only what you must, and
copy every other byte through untouched.**

All container code obeys the [safety model](architecture.md#safety-model).
Every length read from the file is checked against the remaining input,
and every loop has an upper bound.

## Implementation status

Reading is implemented for JPEG, TIFF, PNG (including `png-legacy`) and
WebP in `src/container/`. HEIF and JPEG XL are detected but return
`Error::Unsupported` until M3. Each scanner works over an internal
`Input` trait, implemented for byte slices and for `Read + Seek` streams,
so `Exif::read` and `Exif::read_from` share one code path and fuzzing
checks that they agree. `container::locate` exposes where the payload is
(`Located { format, exif: Option<Range<u64>>, encoding }`) for callers
who want the raw bytes.

What we do differently from this design:

- **JPEG:** an APP1 that declares more bytes than the file has is read up
  to the end of the file, with `TruncatedContainer` (Severity::Error).
- **PNG:** CRCs are checked only for the chunk that carries Exif, which
  is a warning on mismatch. When an `eXIf` chunk and a legacy raw profile
  are both present, the `eXIf` chunk wins (`DuplicateExif`).
- **TIFF streams:** `read_from` reads a TIFF file whole, because the
  parse layer works on a slice. Seeking only to the IFDs is a later
  optimisation.

## Summary

| Format     | Where Exif lives                                        | Prefix before TIFF header        | Max payload        | v1 read | v1 write |
|------------|---------------------------------------------------------|----------------------------------|--------------------|---------|----------|
| JPEG       | APP1 segment                                            | `Exif\0\0` (6 bytes)             | 65 527 bytes TIFF  | ✓       | ✓        |
| TIFF / raw | The file **is** the TIFF structure                      | n/a                              | 4 GiB              | ✓       | TIFF ✓, raw ✗ |
| PNG        | `eXIf` chunk (legacy: `zTXt`/`iTXt`/`tEXt` raw profile) | n/a                              | 2³¹−1              | ✓       | ✓ (`eXIf` only) |
| WebP       | `EXIF` chunk in RIFF, requires `VP8X`                   | n/a (**quirk:** sometimes `Exif\0\0`) | 4 GiB          | ✓       | ✓        |
| HEIF/AVIF  | `Exif` item in `meta`; data in `mdat`/`idat` via `iloc` | 4-byte BE offset to TIFF header  | 4 GiB              | ✓       | ✓ (M3)   |
| JPEG XL    | `Exif` box (or `brob` wrapping it) in the BMFF container | 4-byte BE offset to TIFF header | n/a                | ✓       | ✓ (M3)   |

Format detection (`detect`) uses magic bytes only: `FF D8 FF`;
`II*\0` / `MM\0*`; `\x89PNG\r\n\x1a\n`; `RIFF????WEBP`; `????ftyp` +
brand (`heic`, `heix`, `mif1`, `msf1`, `avif`, `avis`, …); `\0\0\0\x0CJXL
\r\n\x87\n` (container) or `FF 0A` (bare codestream, which cannot hold
Exif).

## JPEG

**Read**

- Walk the markers from SOI. Read each segment's 2-byte length and skip
  it. **Stop at SOS** (0xFFDA), because metadata never comes after the
  scan in practice. Skip fill bytes (`FF FF …`). RST0–7, SOI and EOI
  have no length field.
- The first APP1 whose payload starts with `Exif\0\0` is the Exif block.
  **quirk:** also accept `Exif\0\xFF` and `Exif` followed directly by the
  TIFF header, and report a diagnostic.
- If there is more than one Exif APP1: use the first and report a
  diagnostic. The others are preserved on write, unless the caller sets
  `WriteOptions::dedupe_exif`, which drops them.
- Other things we may meet: XMP APP1 (`http://ns.adobe.com/xap/1.0/\0`),
  Extended XMP APP1, ICC APP2 (`ICC_PROFILE\0`), MPF APP2 (`MPF\0`) and
  Photoshop APP13. We preserve all of them unchanged and expose them as
  `Located::siblings` for callers who want the raw bytes.

**Write**

- Replace the payload of the existing Exif APP1 in place. If there is
  none, insert a new APP1 **right after SOI, or right after APP0 JFIF**
  if one is there (JFIF requires APP0 to come first). The Exif spec says
  "immediately after SOI", so for files with JFIF we break that rule and
  keep JFIF's.
- **Size limit:** a segment length is a u16 that counts its own 2 bytes,
  so the payload can be at most 65 533 bytes. Subtracting the 6-byte
  `Exif\0\0` prefix leaves at most 65 527 bytes of TIFF. Above that, we
  first try a compact layout and dropping the thumbnail, **if**
  `WriteOptions::shrink_to_fit` allows it. Otherwise we return
  `Error::TooLarge`. We never write a truncated or wrapped length. There
  is no standard way to split Exif across several APP1 segments, and we
  will not invent one.
- **MPF caution:** MPF (APP2) stores the offsets of the secondary images
  (for example a depth map or a gain map) relative to its own position
  in the file. When the Exif APP1 changes size, everything after it
  moves, but those offsets are relative to the MPF marker, which moves by
  the same amount, so they stay valid as long as the MPF segment comes
  after the Exif segment. If an MPF segment comes *before* the Exif APP1
  (rare), we report a diagnostic and shift its stored offsets by the
  size change. This needs a minimal MPF index parser, `container/jpeg/mpf.rs`.

## TIFF (and TIFF-based raw files)

**Read:** The file is the TIFF structure itself, so the Exif reader runs
directly on the file through `Read + Seek`, without reading the image
strips or tiles. SubIFDs (0x014A) are walked so that DNG and NEF
previews are found. Raw files (DNG, CR2, NEF, ARW, ORF, RW2, PEF) are
read this way. Formats that only *look* like TIFF need a small adapter:
RW2 uses magic 0x55, ORF uses `IIRO`/`MMOR`, and CR3 is ISOBMFF, not
TIFF, so it is out of scope for v1.

**Write (TIFF only):** use the **append** strategy. Write the changed
IFDs (and their value data) to the end of the file, then update the
IFD0 offset in the header, or update the pointer from the parent IFD.
Strips, tiles and untouched IFDs are never moved, so their offsets stay
valid. The old IFDs become unreferenced space. `WriteOptions::compact`
rewrites the whole file instead (a later milestone). We refuse to write
raw formats (`Error::Unsupported`) until there is a test corpus, because
vendor IFDs often contain absolute offsets that we do not track.

## PNG

**Read**

- Check the signature, then walk the chunks: a 4-byte length (at most
  2³¹−1), the type, the data and a CRC. Verifying the CRC is optional
  (`ReadOptions::verify_crc`); a mismatch produces a diagnostic.
- Use the `eXIf` chunk. The PNG 3rd edition allows it either before or
  after IDAT, but at most once. **quirk:** some writers put `eXIf` after
  IDAT. We read it there too.
- **Legacy (feature `png-legacy`):** look for `zTXt`/`iTXt`/`tEXt` with
  the keyword `Raw profile type exif` or `Raw profile type APP1`. The
  text is `"\nexif\n<len>\n<hex lines>"`, and the hex decodes to
  `Exif\0\0` + TIFF data. Compressed chunks are inflated with
  `miniz_oxide`, with the output capped by `Limits`.

**Write**

- Replace the existing `eXIf` chunk, or insert one **before the first
  IDAT** (`PngPlacement::BeforeIdat`, the default and the most
  compatible placement), then recompute its CRC. The CRC32 table is
  computed in a `const` block, so no dependency is needed.
- Legacy raw-profile chunks that contain Exif are **removed** when we
  write `eXIf` (`WriteOptions::remove_legacy`, on by default). Otherwise
  readers disagree about which copy wins.
- XMP in `iTXt` `XML:com.adobe.xmp` may repeat Exif fields
  (`exif:`/`tiff:` namespaces). We **do not** edit XMP in v1. We document
  the precedence problem and expose the XMP blob as a sibling, so a
  caller can handle it. (`little_exif` strips the `exif:` attributes
  from XMP. We may add a similar opt-in helper once there is an XMP
  companion crate.)

## WebP

**Read:** The file is `RIFF <size> WEBP`, then a sequence of chunks
(FourCC, u32 LE size, data padded to an even length). Use the `EXIF`
chunk. **quirk:** some writers prefix it with `Exif\0\0`, which we
detect and skip.

**Write**

- The `EXIF` chunk is only allowed in the extended format (`VP8X`). The
  chunk order is: `VP8X`, `ICCP`, `ANIM`, the image data (`ALPH`/`VP8 `/
  `VP8L`/`ANMF`…), then `EXIF`, then `XMP `.
- A simple-format file (with a plain `VP8 ` or `VP8L` chunk) has to be
  converted to extended. That means writing a `VP8X` chunk with the
  canvas size, which we read from the bitstream header:
  - `VP8 ` (lossy): the keyframe header at byte 6 holds 14-bit width
    and height.
  - `VP8L` (lossless): after the 0x2F signature byte come 14-bit
    width−1 and height−1.
  - If there is an `ALPH` chunk, also set the alpha flag.

  `little_exif` cannot convert lossy files, but we can.
- Set the EXIF flag (bit 3) in `VP8X`, place the chunk after the image
  data and before `XMP `, keep the padding, and update the RIFF size.
  Removing the Exif chunk clears the flag. We do **not** convert back to
  the simple format.

## HEIF / HEIC / AVIF (ISOBMFF)

**Read**

- Parse the top-level boxes: `ftyp`, then `meta` (a FullBox) containing
  `hdlr` (`pict`), `iinf` (`infe` v2/v3 entries), `iloc` (v0–2;
  `offset_size`, `length_size`, `base_offset_size` and `index_size` can
  each be 0, 4 or 8), `iref` and `idat`.
- Find the `infe` with `item_type == "Exif"`, then its `iloc` extents.
  `construction_method` 0 means a file offset and 1 means an offset into
  `idat`. Construction method 2 (item offset) is unsupported, and we
  report a diagnostic.
- Join the extents. The payload starts with a **u32 BE
  `exif_tiff_header_offset`**, followed by that many bytes (usually
  `Exif\0\0` or nothing) and then the TIFF header.
- Use `iref` `cdsc` to find which image the Exif describes. If there are
  several Exif items, prefer the one linked to the primary item (`pitm`).

**Write (milestone M3)**

- Strategy: **append.** Write the new payload in a new `mdat` box at the
  end of the file, change the Exif item's `iloc` entry to a single
  extent pointing at it, and leave the old bytes as unreferenced space.
  The `meta` box's own size does not change unless the `iloc` field
  widths have to grow. If they do, `meta` gets bigger, and every
  absolute `iloc` offset pointing past it must shift by the size change
  (including the offsets of other items, and `base_offset`s). The layout
  planner handles that shift.
- If there is no Exif item, add an `infe` (with a new item id), an
  `iloc` entry and an `iref cdsc` link to the primary item. This changes
  the sizes of `iinf`, `iloc`, `iref` and `meta`, so all of them must be
  re-serialized, with the same offset-shift rule as above.
- The ISOBMFF reader is shared with JPEG XL and could later read video
  (MOV/MP4).

## JPEG XL

**Read:** Only the container form (ISOBMFF-style boxes starting with the
`JXL ` signature box) can hold Exif. A bare codestream cannot. Look for
an `Exif` box, whose payload is the u32 BE TIFF header offset followed
by the TIFF data, as in HEIF. If instead there is a `brob` box whose
first 4 bytes are `Exif`, Brotli-decompress the rest (feature
`jxl-brob`, with the output capped by `Limits`).

**Write (M3):** Replace or insert an `Exif` box before the first
`jxlc`/`jxlp` box. We always write it uncompressed. If the source had a
`brob`, we replace it with a plain `Exif` box. There are no offsets to
fix up (JXL boxes do not cross-reference each other), so this is a pure
splice.
