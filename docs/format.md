# Exif format reference

Status: draft

A working summary of the parts of CIPA DC-008 (Exif 3.0) and TIFF 6.0
that the crate implements. This is not a replacement for the spec. Each
rule below cites where it comes from, and real-world deviations are
marked **quirk**. For how Exif is embedded in each image format, see
[`containers.md`](containers.md).

## 1. Byte layout

An Exif block is a complete TIFF structure. All offsets inside it are
measured from the first byte of the TIFF header ("TIFF-relative"), not
from the start of the file or of the containing segment.

```
offset  size  field
0       2     byte order: "II" (0x4949, little-endian) or "MM" (0x4D4D, big-endian)
2       2     magic: 42
4       4     offset of IFD0
```

- **BigTIFF** (magic 43, 8-byte offsets) is not valid Exif. The crate
  rejects it with `Error::Unsupported`. Plain TIFF files that happen to be
  BigTIFF are out of scope for v1.
- **quirk:** Some files have the wrong byte-order mark for their content
  or garbage after the header. We trust the byte-order mark and validate
  everything else as we go.

### 1.1 IFD (Image File Directory)

```
2           entry count N
12 × N      entries
4           offset of the next IFD (0 = end of chain)
```

Each entry is 12 bytes:

```
2   tag id (u16)
2   type   (u16)
4   count  (u32), the number of values, not bytes
4   value if size(type) × count ≤ 4, otherwise the offset to the value
```

Rules:

- When a value fits in 4 bytes, it is stored inline in the value field,
  left-justified, and the unused bytes should be zero (TIFF 6.0 p.15).
- Entries **must** be sorted by tag id in ascending order (TIFF 6.0
  p.15). The writer always sorts them. The reader accepts unsorted IFDs
  and reports a diagnostic.
- Value offsets should be word-aligned (even) (TIFF 6.0 p.15). The
  writer aligns to 2 bytes. **quirk:** odd offsets are common, so the
  reader accepts them.
- Duplicate tag ids in one IFD: the reader keeps the first, reports a
  diagnostic, and keeps the raw duplicate so a round-trip preserves it.
- **quirk:** Sub-IFDs (Exif, GPS, Interop) often have no 4-byte
  next-pointer, or end exactly at the end of the buffer. A truncated or
  garbage next-pointer in a sub-IFD is ignored.

### 1.2 Value types

| Id  | Name      | Size | Rust                | Notes                                                                       |
|-----|-----------|------|---------------------|-----------------------------------------------------------------------------|
| 1   | BYTE      | 1    | `u8`                |                                                                             |
| 2   | ASCII     | 1    | bytes → `str`       | NUL-terminated, and the count includes the NUL. See §3                      |
| 3   | SHORT     | 2    | `u16`               |                                                                             |
| 4   | LONG      | 4    | `u32`               |                                                                             |
| 5   | RATIONAL  | 8    | `Rational { u32, u32 }` | Numerator, then denominator. Stored exactly, never as `f64`             |
| 6   | SBYTE     | 1    | `i8`                |                                                                             |
| 7   | UNDEFINED | 1    | `[u8]`              | Meaning depends on the tag                                                  |
| 8   | SSHORT    | 2    | `i16`               |                                                                             |
| 9   | SLONG     | 4    | `i32`               |                                                                             |
| 10  | SRATIONAL | 8    | `SRational { i32, i32 }` |                                                                        |
| 11  | FLOAT     | 4    | `f32`               | IEEE 754                                                                    |
| 12  | DOUBLE    | 8    | `f64`               |                                                                             |
| 13  | IFD       | 4    | `u32` offset        | TIFF/EP and DNG. Treated like LONG that points to a sub-IFD                 |
| 129 | UTF-8     | 1    | bytes → `str`       | **Exif 3.0.** NUL-terminated like ASCII                                     |

- **Unknown types** (anything else, including the BigTIFF types 16–18):
  the reader **must skip the entry** and keep going (TIFF 6.0 p.16:
  readers should ignore types they do not expect). We keep the raw entry
  as `Value::Unknown { type_id, count, bytes }`, with `bytes` filled only
  when `count × size` can be computed and the data is in bounds. It is
  never a fatal error.
- **Type mismatch:** when a known tag has a type the spec does not allow
  for it, the reader keeps the value **as stored** and reports a
  diagnostic. Conversion happens only at typed access, losslessly, or
  returns an error. See [`api.md`](api.md).

## 2. IFD graph

```
TIFF header
 └─ IFD0 (primary image)                   ──next──▶ IFD1 (thumbnail) ──next──▶ (rare: IFD2…)
     ├─ 0x8769 ExifIFDPointer ─▶ Exif IFD
     │                            ├─ 0xA005 InteroperabilityIFDPointer ─▶ Interop IFD
     │                            └─ 0x927C MakerNote (UNDEFINED blob, vendor format)
     ├─ 0x8825 GPSInfoIFDPointer ─▶ GPS IFD
     └─ 0x014A SubIFDs (LONG/IFD × n) ─▶ SubIFD[0..n]   (DNG / TIFF-EP raw data)
IFD1
 ├─ 0x0201 JPEGInterchangeFormat        ┐ compressed thumbnail
 └─ 0x0202 JPEGInterchangeFormatLength  ┘
```

- Tag ids are **only unique within a namespace.** IFD0, IFD1, the Exif
  IFD and SubIFDs share one id space (`Namespace::Main`): TIFF and Exif
  ids never collide, and tags do turn up in the "wrong" one of those
  IFDs. GPS tags start at 0x0000 and Interop tags at 0x0001, which overlap
  Main ids, so each has its own namespace. A tag is therefore identified
  by `(Namespace, u16)`, and the IFD it was found in is kept alongside.
- **quirk:** pointer tags sometimes appear in the "wrong" IFD (an Exif
  pointer in IFD1, or Interop in IFD1). The reader follows any pointer
  tag wherever it is and records which parent it came from.
- **quirk:** some files point two different parents at the same IFD.
  Each offset is parsed once. A second reference to it becomes a shared
  link, not a second parse.

### 2.1 Thumbnail

IFD1 describes the thumbnail. In Exif it is almost always a JPEG
(`Compression = 6`), referenced by `0x0201`/`0x0202`. The thumbnail
bytes are **data, not a tag value**. The model holds them as
`Thumbnail::Jpeg(Vec<u8>)`, and the writer re-places them and recomputes
both tags. Uncompressed strip thumbnails (`StripOffsets` and
`StripByteCounts` in IFD1) are rare. They are read as opaque strips and
round-tripped unchanged.

### 2.2 MakerNote

`0x927C` is an UNDEFINED blob in a vendor-specific format. Many
MakerNotes contain IFD-like structures with offsets measured from the
TIFF header (Canon, older Nikon, Sony, Pentax, Olympus type 1) or from
the MakerNote itself (Nikon type 3, Olympus type 2, Fujifilm, Panasonic,
Apple). Moving the blob therefore breaks the first group. How the writer
handles this is described in [`writing.md`](writing.md#makernote).

Microsoft's `OffsetSchema` tag (0xEA1D, SLONG, in the Exif IFD) records
how far a MakerNote has moved. ExifTool and Windows both read it. We
write it whenever we move a MakerNote.

## 3. Strings and text

- **ASCII** (type 2): 7-bit text with a NUL terminator, and the count
  includes the NUL. **quirk:** files often have a missing NUL, several
  NULs, trailing spaces, or bytes above 0x7F (usually Latin-1 or
  Shift-JIS). The reader removes trailing NULs and padding. `Value`
  keeps the exact bytes, and `as_str()` is lossy only when the caller
  asks for it.
- **UTF-8** (type 129, Exif 3.0): the spec lets ASCII-type tags hold
  UTF-8 strings. The tag table records, for each tag, whether it is
  `Ascii`, `AsciiOrUtf8` or `Utf8`. `Utf8` means UTF-8 is mandatory, as for
  `DevelopmentTypeDescription` in Exif 3.1; those tags are always written
  as type 129, even when the text is pure ASCII. For the other kinds,
  when writing a string tag:
  - if the text is pure ASCII → write ASCII (best compatibility);
  - otherwise, if the tag allows UTF-8 → write UTF-8;
  - otherwise → `Error::InvalidValue`.
- **`Copyright` (0x8298):** `"photographer\0editor\0"`. Exposed as two
  parts.
- **`UserComment` (0x9286), `GPSProcessingMethod`, `GPSAreaInformation`:**
  UNDEFINED data that starts with an 8-byte character-code prefix:
  `"ASCII\0\0\0"`, `"JIS\0\0\0\0\0"`, `"UNICODE\0"` (UCS-2 in the TIFF
  byte order; **quirk:** sometimes in the opposite order, which is
  detected with a BOM or heuristic) or `"\0"×8` (undefined). Exposed as
  `EncodedText { encoding, bytes }` with a lossy `to_string()`.
- **Version tags** (`ExifVersion` 0x9000, `FlashpixVersion` 0xA000,
  `InteroperabilityVersion`, and `GPSVersionID` as BYTE×4): four
  characters, no NUL, for example `"0300"`.

## 4. Dates and times

| Tag                                          | Format                    | Notes                                   |
|----------------------------------------------|---------------------------|-----------------------------------------|
| `DateTime` 0x0132 (IFD0)                     | `"YYYY:MM:DD HH:MM:SS\0"` | 20 bytes. File modification time        |
| `DateTimeOriginal` 0x9003                    | same                      | Capture time                            |
| `DateTimeDigitized` 0x9004                   | same                      |                                         |
| `OffsetTime` / `…Original` / `…Digitized` 0x9010–0x9012 | `"+HH:MM\0"`   | Exif 2.31. UTC offset for each of the above |
| `SubSecTime` / `…Original` / `…Digitized` 0x9290–0x9292 | ASCII digits   | Fraction of a second                    |
| `GPSDateStamp` 0x001D + `GPSTimeStamp` 0x0007 | `"YYYY:MM:DD"` + 3 RATIONAL | Always UTC                             |

- An unknown component is written as spaces, for example
  `"    :  :     :  :  "`, and parses as `None`.
- **quirk:** separators other than `:`, `T` between date and time, a
  missing seconds field, and `0000:00:00 00:00:00` all occur in real
  files. The parser accepts them and reports a diagnostic.
- The crate provides a dependency-free `DateTime { date, time, subsec,
  offset }`, with optional conversions to `jiff` and `chrono` behind
  feature flags.

## 5. GPS

- Latitude/longitude: 3 × RATIONAL (degrees, minutes, seconds), with a
  `Ref` of `"N"`/`"S"` or `"E"`/`"W"` (ASCII, count 2).
- `GPSAltitudeRef` (BYTE, DC-008-2026 §4.6.7.1.6): 0 = at or above the
  ellipsoidal surface, 1 = below it, 2 = at or above sea level, 3 = below
  sea level. **The meaning of 0 and 1 depends on the file's version.**
  Up to Exif 2.32 they meant sea level; Exif 3.0 redefined them as
  ellipsoidal height, because GNSS receivers report that, and added 2 and
  3 for sea level. So `Gps::altitude()` returns `Altitude { meters,
  datum: Ellipsoid | SeaLevel }` and reads `ExifVersion` to choose the
  datum: before `"0300"`, 0/1 mean sea level. Writing sea-level data
  always uses 2/3 and requires `ExifVersion ≥ "0300"`. **quirk:** the tag
  is sometimes stored as ASCII `"0"`/`"1"`. The reader accepts this.
- Helpers: `Gps::coordinates() -> Option<(f64, f64)>` returns decimal
  degrees, and `Gps::set_coordinates(lat, lon)` encodes them with a
  configurable precision (default: seconds as `x/10000`).

## 6. Tag set

The crate generates its tag table from a data file (see
[`architecture.md`](architecture.md#tag-table)). Each row records:

```
namespace, id, spec name, types allowed, count (fixed n | any | n×m),
default, requirement (mandatory/recommended/optional per CIPA tables),
string kind (Ascii/AsciiOrUtf8/Utf8), enum values (for SHORT enums),
spec version introduced, whether the writer manages it (offsets, lengths)
```

Coverage target for v1:

- **TIFF baseline and extension tags** that appear in IFD0 and IFD1 (TIFF 6.0).
- **All Exif IFD, GPS IFD and Interop IFD tags in DC-008 3.0.** This
  includes the 2.3 additions (`SensitivityType`,
  `RecommendedExposureIndex`, `CameraOwnerName` 0xA430,
  `BodySerialNumber` 0xA431, `LensSpecification` 0xA432, `LensMake`,
  `LensModel`, `LensSerialNumber`, `Gamma` 0xA500), the 2.31 additions
  (`OffsetTime*`, `Temperature` 0x9400 through `CameraElevationAngle`
  0x9405), the 2.32 additions (`CompositeImage` 0xA460–0xA462) and the
  3.0 additions:

  | Id     | Name                      |
  |--------|---------------------------|
  | 0xA436 | `ImageTitle`              |
  | 0xA437 | `Photographer`            |
  | 0xA438 | `ImageEditor`             |
  | 0xA439 | `CameraFirmware`          |
  | 0xA43A | `RAWDevelopingSoftware`   |
  | 0xA43B | `ImageEditingSoftware`    |
  | 0xA43C | `MetadataEditingSoftware` |

- **The Exif 3.1 additions (DC-008-2026):** 7 new tags and the new
  `LightSource` values 25–34. See [§8](#8-exif-31-changes).
- **Common non-CIPA tags:** `SubIFDs` 0x014A, `XMLPacket` 0x02BC (XMP
  stored in TIFF), `IPTC-NAA` 0x83BB, `InterColorProfile` 0x8773,
  `Rating` 0x4746, `RatingPercent` 0x4749, `OffsetSchema` 0xEA1D,
  `Padding` 0xEA1C, and the Windows `XP*` tags 0x9C9B–0x9C9F (UCS-2).
- Every other tag is still read and written faithfully. It just has no
  name.

Names follow **the CIPA spec**, for example `DateTimeDigitized` rather
than `CreateDate` and `PixelXDimension` rather than `ExifImageWidth`.
Each table entry also stores its ExifTool name as an alias, so lookup by
either name works.

## 7. Post-processing rules (DC-008 Annex H)

Exif 3.0 Annex H describes what editing software should do with each
tag after it modifies an image. Examples: update `DateTime`; set
`ImageEditor` and `ImageEditingSoftware`; remove or recompute
`PixelXDimension`/`PixelYDimension` and the thumbnail when the image
pixels change.

We encode this as a per-tag `on_edit` policy in the tag table:
`Keep | Update | Remove | Recompute`. `Exif::prepare_for_edit(EditKind)`
applies it. The caller opts in, and nothing is applied automatically.

## 8. Exif 3.1 changes

CIPA DC-008-2026 (Exif 3.1) was published on 2026-01-30 as a revision
of DC-008-2024. ExifTool added its tags in 13.56 (2026-04-15).

Everything in this section is checked against the CIPA PDF: types and
counts against Table 18, values against the definitions in §4.6.6.4.3
and §4.6.6.7.47–52, and the original-preservation rules against Annex
K.2.13.

### New tags (all in the Exif IFD)

| Id     | Name                              | Type      | Count | Values                                                                 |
|--------|-----------------------------------|-----------|-------|------------------------------------------------------------------------|
| 0x9287 | `LearningOptOutIn`                | UNDEFINED | any   | A SHORT set count n ≥ 1, then n `(usage, intention)` SHORT pairs: 2 + 4n bytes. Usage: 0 all/unspecified, 1 non-generative AI/ML training, 2 generative AI/ML training, 3 data mining, 4 input to a foundation model (inference). Intention: 0 opt-out, 1 opt-in, 2 unspecified. Other values are reserved |
| 0xA40D | `DevelopmentType`                 | SHORT     | 1     | A bit field. High byte, the capture-device development characteristic: 0x01 faithful reproduction, 0x02 not identical but no extreme difference, 0x04 extreme difference (size, shape or subject altered). Low byte, the difference from the factory default: 0x01 not different, 0x02 different, 0x04 unknown |
| 0xA40E | `DevelopmentTypeDescription`      | **UTF-8** | any   | Free text. UTF-8 is mandatory, and the count includes the NUL          |
| 0xA40F | `DistortionCorrection`            | SHORT     | 1     | 0 not applied, 1 applied                                               |
| 0xA410 | `ChromaticAberrationCorrection`   | SHORT     | 1     | 0 not applied, 1 applied                                               |
| 0xA411 | `ShadingCorrection`               | SHORT     | 1     | 0 not applied, 1 applied (vignetting)                                  |
| 0xA412 | `NoiseReduction`                  | SHORT     | 1     | 0 not applied, 1 low, 2 normal, 3 high strength                        |

**`LearningOptOutIn` layout and rules (§4.6.6.4.3, Figures 16–17).** The
UNDEFINED payload is made of 16-bit SHORT fields, in the TIFF block's byte
order:

```
SHORT  n                      number of sets, n ≥ 1
SHORT  usage[0]  (= 0)        ┐ set 1
SHORT  intention[0]           ┘
…
SHORT  usage[n-1]             ┐ set n
SHORT  intention[n-1]         ┘
```

The byte count must be exactly 2 + 4n. Note that these fields are
SHORTs, not bytes; some secondary sources say otherwise. The tag is **invalid** unless both of these
hold:

1. The first set has usage 0 ("all / individual usage not specified").
2. No usage value appears twice.

When specific usages (1–4) are listed, the intention given for usage 0
applies to every other usage, including ones added in future versions.
Example (Figure 17): n = 3, `[(0, Unspecified), (1, OptIn), (4, OptOut)]`
means opt-in for
non-generative training, opt-out for use as foundation-model input, and
unspecified otherwise. The reader validates both rules and reports a
diagnostic on failure, and `LearningPolicy::new` refuses to build an
invalid value.

A payload whose length is not 2 + 4n, or whose n is 0, is kept as raw
`Undefined` bytes with a diagnostic, so it still round-trips.

**`GainControl`:** Table 18 lists it as RATIONAL, but its definition
(§4.6.6.7.41) says SHORT, as every earlier version did. The table is
wrong, and we use SHORT.

These tags need some typed API work:

- `LearningOptOutIn` becomes a dedicated type, `LearningPolicy(Vec<(Usage,
  Intention)>)`, with both enums `#[non_exhaustive]` and an `Other(u16)`
  variant. It also gets a privacy/consent flag in the tag table, so that
  `Strip::Personal` **keeps** it: removing an opt-out would be the wrong
  default.
- `DevelopmentType` becomes `DevelopmentType { characteristic: u8,
  default_diff: u8 }`, with flag constants for both bytes.
- DC-008-2026 adds the **original preservation image**: an unaltered MPF
  image stored next to an AI-processed primary image (Annex K). In
  `Exif::validate()` its rules (K.2.13) are an opt-in profile,
  `Profile::OriginalPreservation`:
  - **Mandatory tags:** `DateTimeOriginal`, `OffsetTimeOriginal`,
    `SubSecTimeOriginal`, `DevelopmentType`, `DevelopmentTypeDescription`,
    `CompositeImage` and `ImageUniqueID`.
  - **Recommended tags:** `Make`, `Model`, `Software` and
    `CameraFirmware`, plus `RAWDevelopingSoftware` if software did the
    RAW development.
  - The high byte of `DevelopmentType` must be 0x01 or 0x02, so 0x04 is
    forbidden.
  - `DevelopmentTypeDescription` may be just the NUL, but only when
    `DevelopmentType` is `0x0101` (faithful reproduction, factory
    default).
  - `ImageUniqueID` must equal that of the Baseline MP primary image.
  - The image must never be edited. It is either kept unchanged,
    including its Exif tags, or deleted entirely. So the writer refuses to
    modify an image marked as an original preservation image unless the
    caller forces it.

### New `LightSource` values

| Value | Meaning                   |
|-------|---------------------------|
| 25    | Daylight light source (D 5700–7100K)     |
| 26    | Day white light source (N 4600–5500K)    |
| 27    | Cool white light source (W 3800–4500K)   |
| 28    | White light source (WW 3250–3800K)       |
| 29    | Warm white light source (L 2600–3250K)   |
| 30    | Daylight LED              |
| 31    | Day white LED             |
| 32    | Cool white LED            |
| 33    | White LED                 |
| 34    | Warm white LED            |

### Other changes

- **`ExifVersion` stays `"0300"`** (§4.6.6.1.1: "shall be recorded like
  \"0300\""). No version value identifies Exif 3.1.
  A 3.1 file can only be recognized by the presence of the new tags. So
  the writer never writes `"0310"`. The version bump rule in
  [`writing.md`](writing.md#required-tags) treats 3.1 tags like 3.0 tags
  and raises older versions to `"0300"`.
- **Annex K (MPF)** was revised following a 2025 revision of the MPF
  specification, and now holds the original preservation image rules
  above. Our MPF code only shifts offsets, so it is unaffected. Reading
  the original preservation image itself is part of the later MPF read
  API.

### Backward compatibility

Exif 3.1 is **backward compatible at the format level**. It is purely
additive:

- No new value types, no structural changes, and no changes to existing
  tag definitions that we know of.
- The new tags use ids that were unused in the Exif IFD, and every new tag
  is optional. A reader that
  ignores unknown tags, as TIFF requires, reads a 3.1 file correctly and
  simply skips the new tags.
- `LightSource` 25–34 are new values of an existing enum. Older readers
  show them as "unknown" or "other". This is why our generated enums have
  an `Other(u16)` variant.
- One catch: `DevelopmentTypeDescription` *must* be UTF-8 (type 129),
  the type Exif 3.0 introduced. A conforming reader that predates 3.0
  skips the entry. A non-conforming reader that fails on unknown types,
  such as `little_exif`, loses the whole Exif IFD, not just that tag.
  That is the same breakage any UTF-8 tag already causes; 3.1 just makes
  one tag UTF-8 by definition.

One related catch is older and comes from 3.0: the `GPSAltitudeRef`
values 0/1 changed meaning from sea level to ellipsoidal height (see §5).
Files that declare `ExifVersion < "0300"` must be read with the old
meaning.

What this means for us:

- Writing a 3.1 tag never makes a file unreadable for a conforming older
  reader.
- We cannot detect 3.1 support from the version field.
- Reading needs no special handling beyond the tag table.
