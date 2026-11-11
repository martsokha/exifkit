# Public API

Status: draft

These signatures are a target, not a commitment. Names and shapes can
change during implementation, but the principles in
[`architecture.md`](architecture.md#goals) do not.

## Quick tour

```rust
use exifkit::{Exif, Tag, Value};
use exifkit::tag::Orientation;

// Read from any supported container (format auto-detected).
let bytes = std::fs::read("photo.jpg")?;
let (mut exif, diags) = Exif::read(&bytes)?;        // diags: non-fatal issues
for d in &diags { eprintln!("warning: {d}"); }

// Typed access
let orientation: Option<Orientation> = exif.get(Tag::ORIENTATION)?;
let taken = exif.date_time_original()?;            // Option<exifkit::DateTime>
let (lat, lon) = exif.gps().coordinates().unwrap_or_default();

// Raw access
if let Some(entry) = exif.entry(Tag::MAKE) {
    println!("{} = {}", entry.tag(), entry.value()); // Display shows the interpreted value
}

// Edit
exif.set(Tag::ARTIST, "Jane Doe")?;                 // ASCII or UTF-8 chosen automatically
exif.set(Tag::ORIENTATION, Orientation::Normal)?;
exif.remove_gps();
exif.remove(Tag::MAKER_NOTE);

// Write back into the same file format, touching only the metadata segment
let out: Vec<u8> = exif.write_into(&bytes)?;
std::fs::write("photo.jpg", out)?;
```

Streaming from disk, for large TIFF/raw/HEIF files where only the Exif
part is needed:

```rust
let file = std::fs::File::open("big.dng")?;
let (exif, _) = Exif::read_from(std::io::BufReader::new(file))?;  // R: Read + Seek
```

When the caller already has the raw TIFF block (for example from
`image::ImageDecoder::exif_metadata()`):

```rust
let (exif, _) = Exif::from_tiff(&blob)?;
let blob: Vec<u8> = exif.to_tiff()?;
```

## Data model

```rust
pub struct Exif {
    byte_order: ByteOrder,          // preserved on write unless changed
    ifds: Vec<Ifd>,                 // tree flattened; parent links by IfdId
    thumbnail: Option<Thumbnail>,
    maker_note: Option<MakerNote>,
    source: Option<SourceLayout>,   // original offsets, for preserve-layout writing
}

#[non_exhaustive]
pub enum IfdKind {
    Primary,          // IFD0
    Thumbnail,        // IFD1
    Chain(u16),       // IFD2+ (multi-page TIFF), rare in Exif
    Exif,
    Gps,
    Interop,
    SubIfd(u16),      // index into 0x014A
}

#[non_exhaustive]
pub enum Namespace { Main, Gps, Interop }   // id spaces; Main is shared by IFD0, IFD1, Exif IFD, SubIFDs

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Tag { ns: Namespace, id: u16 }
impl Tag {
    pub const IMAGE_DESCRIPTION: Tag = …;  // generated, one per known tag
    pub const fn new(ns: Namespace, id: u16) -> Tag;
    pub fn info(self) -> Option<&'static TagInfo>;
    pub fn name(self) -> Option<&'static str>;           // CIPA name
    pub fn from_name(name: &str) -> Option<Tag>;         // CIPA name or ExifTool alias
    pub fn default_ifd(self) -> IfdKind;
}

pub struct Entry { tag: Tag, value: Value }

#[non_exhaustive]
pub enum Value {
    Byte(Vec<u8>),
    Ascii(Vec<u8>),          // exact stored bytes, NUL included; text() stops at the first NUL
    Short(Vec<u16>),
    Long(Vec<u32>),
    Rational(Vec<Rational>),
    SByte(Vec<i8>),
    Undefined(Vec<u8>),
    SShort(Vec<i16>),
    SLong(Vec<i32>),
    SRational(Vec<SRational>),
    Float(Vec<f32>),
    Double(Vec<f64>),
    Ifd(Vec<u32>),           // type 13; resolved to child IFDs in the model
    Utf8(Vec<u8>),           // type 129, exact bytes; invalid UTF-8 → diagnostic, text() is None
    Unknown { type_id: u16, count: u32, field: [u8; 4] },  // size unknown, so only the raw field
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Rational { pub num: u32, pub den: u32 }   // never normalized implicitly
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct SRational { pub num: i32, pub den: i32 }
// to_f64() -> Option<f64> (None if den == 0), approx(f64, max_den) constructor, Display "1/250"
```

Implemented in M2 (PR 2): `Exif::to_tiff[_with]`, `Exif::encoded_len`,
`WriteOptions`, `MakerNotePolicy`, and editing through `Exif::insert`,
`remove`, `ifd_mut`, `remove_ifd`, `set_thumbnail`, `set_byte_order` and
`Ifd::insert`, `remove`, `retain`, `clear_duplicates`. A typed `set` with
validation (`IntoValue`) is still to come.

Implemented in M2 (PR 1): `Exif::read[_with]`, `Exif::read_from[_with]`,
`container::{detect, locate, Format, Located, Encoding}`.

Implemented in M1: `Exif`, `Ifd`, `Entry`, `Value`, `Rational`,
`SRational`, `ReadOptions`, `Exif::from_tiff[_with]`, `get`, `entry`,
`entries`, `thumbnail` (as `Option<&[u8]>`), `maker_note_offset`,
`date_time*`, and `gps()`. The `Thumbnail`/`MakerNote` types and the
`SourceLayout` arrive with the writer (M2).

Design notes:

- **Text keeps its exact bytes.** `Ascii` and `Utf8` hold the bytes as
  stored, NUL terminator and padding included, so an unedited value
  writes back byte for byte. `Value::ascii("x")` and `Value::utf8("x")`
  add the NUL for new values.
- **The tag and its value are separate**, so a lookup never needs a dummy
  value, and any tag can hold any type when the file says so.
- **Pointer and offset tags are not entries.** The model owns the
  structure: `ExifIFDPointer`, `GPSInfoIFDPointer`, `Interop`,
  `SubIFDs`, `JPEGInterchangeFormat*`, `StripOffsets` and
  `StripByteCounts` in a TIFF container, and `OffsetSchema`. The writer
  computes them. They are visible read-only through `Exif::raw_pointer`,
  for debugging.
- **IFDs keep entries sorted by id**, and `insert` preserves the order.
  Duplicates from the input are kept in `Ifd::duplicates`, so they are
  out of the way but not lost.

## Typed access

```rust
pub trait FromValue: Sized { fn from_value(v: &Value, tag: Tag) -> Result<Self, ValueError>; }
pub trait IntoValue { fn into_value(self, tag: Tag) -> Result<Value, ValueError>; }

impl Exif {
    pub fn get<T: FromValue>(&self, tag: Tag) -> Result<Option<T>, ValueError>;
    pub fn set<T: IntoValue>(&mut self, tag: Tag, v: T) -> Result<Option<Value>, Error>;
    pub fn set_raw(&mut self, ifd: IfdKind, tag: Tag, v: Value) -> Option<Value>; // no validation
    pub fn remove(&mut self, tag: Tag) -> Option<Value>;
    pub fn entry(&self, tag: Tag) -> Option<&Entry>;
    pub fn ifd(&self, kind: IfdKind) -> Option<&Ifd>;
    pub fn ifd_mut(&mut self, kind: IfdKind) -> &mut Ifd;   // creates it if missing
    pub fn entries(&self) -> impl Iterator<Item = (IfdKind, &Entry)>;
}
```

- `get` looks in the tag's default IFD first, then in the others (to
  handle the misplaced-tag quirk). `ifd(kind).get(tag)` looks in one IFD
  only.
- `FromValue` conversions are **lossless or an error**. A SHORT or a
  LONG becomes `u32`. A LONG becomes `u16` only if it fits. A RATIONAL
  becomes `f64` only through the explicit `Rational::to_f64`.
- `set` checks the value against `TagInfo`: allowed types, count, string
  kind and enum range. If the value fails the check, `set` returns
  `Error::InvalidValue`. `set_raw` skips the checks, for tools that need
  to write non-conforming data on purpose.
- Built-in impls cover integers, `f32`/`f64` (FLOAT/DOUBLE tags only),
  `Rational`, `&str`/`String`, `Vec<…>`/`[…; N]`, the generated enums,
  `DateTime`, `EncodedText` and `Copyright`.

Convenience accessors (generated or hand-written; kept small):

```rust
exif.orientation()           -> Option<Orientation>
exif.date_time_original()    -> Result<Option<DateTime>, ValueError>  // merges OffsetTimeOriginal + SubSecTimeOriginal
exif.set_date_time_original(DateTime)   // writes all three tags consistently
exif.gps()  / exif.gps_mut() -> Gps<'_> / GpsMut<'_>
exif.thumbnail()             -> Option<&Thumbnail>
exif.set_thumbnail(Option<Thumbnail>)
exif.maker_note()            -> Option<&MakerNote>
```

## Bulk operations

```rust
exif.retain(|ifd, entry| …);                 // generic filter
exif.strip(Strip::Gps | Strip::Personal);    // presets: Gps, Personal (Artist, owner/serials,
                                             // Photographer, ImageEditor…), MakerNote, Thumbnail, All
exif.prepare_for_edit(EditKind::Pixels);     // apply DC-008 Annex H policy (format.md §7)
```

`Strip::Personal` is defined by a `privacy` flag on each row of the tag
table, so the set is documented and can be reviewed.

## Diagnostics

Reading is lenient by default and returns `(Exif, Diagnostics)`.

```rust
pub struct Diagnostic { pub kind: DiagnosticKind, pub ifd: Option<IfdKind>, pub tag: Option<Tag>, pub offset: Option<u64> }  // file-relative from Exif::read*
#[non_exhaustive]
pub enum DiagnosticKind {
    UnknownType(u16), TypeMismatch { expected: &'static [Type], found: Type },
    CountMismatch { expected: Count, found: u32 }, OutOfBounds, Cycle, DepthLimit,
    UnsortedEntries, DuplicateTag, InvalidUtf8, BadDateTime, Truncated, MisplacedTag,
    CorruptIfd, LimitExceeded(Limit), …
}
```

`ReadOptions { strict: bool, limits: Limits, load_thumbnail: bool,
load_maker_note: bool }`. With `strict`, the first diagnostic of
severity `Error` becomes a hard error.

## Errors

```rust
#[non_exhaustive]
pub enum Error {
    Io(std::io::Error),                         // std only
    UnknownFormat,                              // container not recognized
    Unsupported { what: &'static str },         // e.g. BigTIFF, WebP animation edge case
    NoExif,                                     // container fine, no Exif present (read_* only;
                                                //   `Exif::read_or_default` maps to empty)
    InvalidHeader,
    Malformed { offset: u64, reason: &'static str },  // strict mode / container corruption
    LimitExceeded(Limit),
    InvalidValue { tag: Tag, reason: ValueError },
    TooLarge { container: Format, size: usize, max: usize }, // e.g. JPEG APP1 > 65533
}
```

No `String` payloads, so the error is cheap, matchable and `no_std`
friendly. `Display` writes the human-readable message.

## Container-level API

For callers who want control over the container:

```rust
pub enum Format { Jpeg, Tiff, Png, WebP, Heif, Jxl }
pub fn detect(prefix: &[u8]) -> Option<Format>;          // first 32 bytes are enough

pub struct Located { pub format: Format, pub range: Range<u64>, pub tiff_offset: u64, /* … */ }
pub fn locate<R: Read + Seek>(r: R) -> Result<Option<Located>, Error>;

// Writing
pub struct WriteOptions { pub layout: Layout, pub png_placement: PngPlacement, /* … */ }
impl Exif {
    pub fn write_into(&self, original: &[u8]) -> Result<Vec<u8>, Error>;
    pub fn write_into_with(&self, original: &[u8], opts: &WriteOptions) -> Result<Vec<u8>, Error>;
    pub fn write_stream<R: Read + Seek, W: Write>(&self, src: R, dst: W, opts: &WriteOptions) -> Result<(), Error>;
}
pub fn remove_exif(original: &[u8]) -> Result<Vec<u8>, Error>;   // delete the segment entirely
```

There is no `write_to_file(path)` that overwrites in place. Writing to a
temporary file and renaming it is the caller's decision, and the docs
show how. This avoids the "half-written JPEG after a crash" failure.
