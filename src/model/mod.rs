//! The owned Exif model, and the reader that builds it from a TIFF block.
//!
//! See `docs/api.md#data-model` and `docs/architecture.md#safety-model`.

use alloc::vec::Vec;
use core::fmt;

use crate::diagnostic::{DiagnosticKind, Diagnostics};
use crate::error::{Error, Result};
use crate::ifd::IfdKind;
use crate::limits::Limits;
use crate::tag::{Namespace, Role, Tag};
use crate::tiff::{ByteOrder, Header, Reader};
use crate::value::{FromValue, Value, ValueError};

/// A tag and its value.
///
/// Equality compares the tag and value, not where the value sat in the
/// source.
#[derive(Debug, Clone)]
pub struct Entry {
    tag: Tag,
    value: Value,
    /// Offset of the value's data in the source block, if it was out of line.
    pub(crate) source: Option<u32>,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.tag == other.tag && self.value == other.value
    }
}

impl Eq for Entry {}

impl Entry {
    /// Creates an entry.
    #[must_use]
    pub const fn new(tag: Tag, value: Value) -> Self {
        Self {
            tag,
            value,
            source: None,
        }
    }

    /// Returns the tag.
    #[must_use]
    pub const fn tag(&self) -> Tag {
        self.tag
    }

    /// Returns the value.
    #[must_use]
    pub const fn value(&self) -> &Value {
        &self.value
    }

    /// Consumes the entry, returning its value.
    #[must_use]
    pub fn into_value(self) -> Value {
        self.value
    }
}

impl fmt::Display for Entry {
    /// Formats as `Name: value`, describing enumerated values
    /// (`Orientation: Rotate 90 CW`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(info) = self.tag.info()
            && let Ok(raw) = u16::from_value(&self.value)
            && let Some(desc) = info.describe(raw)
        {
            return write!(f, "{}: {desc}", self.tag);
        }
        write!(f, "{}: {}", self.tag, self.value)
    }
}

/// One IFD of the model. Entries are sorted by tag id.
///
/// Pointer tags (`ExifIFDPointer`, `SubIFDs`, …) are not entries: the model
/// holds the IFDs they point to, and the writer recomputes them.
///
/// Equality compares content (kind, parent, entries, duplicates), not the
/// offset the IFD was read from.
#[derive(Debug, Clone)]
pub struct Ifd {
    kind: IfdKind,
    parent: Option<IfdKind>,
    offset: Option<u32>,
    pub(crate) entries: Vec<Entry>,
    duplicates: Vec<Entry>,
}

impl PartialEq for Ifd {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.parent == other.parent
            && self.entries == other.entries
            && self.duplicates == other.duplicates
    }
}

impl Eq for Ifd {}

impl Ifd {
    /// Creates an empty IFD.
    #[must_use]
    pub const fn new(kind: IfdKind) -> Self {
        Self {
            kind,
            parent: default_parent(kind),
            offset: None,
            entries: Vec::new(),
            duplicates: Vec::new(),
        }
    }

    /// Returns which IFD this is.
    #[must_use]
    pub const fn kind(&self) -> IfdKind {
        self.kind
    }

    /// Returns the tag namespace of this IFD.
    #[must_use]
    pub const fn namespace(&self) -> Namespace {
        namespace_of(self.kind)
    }

    /// Returns the IFD whose pointer tag leads here (`ExifIFDPointer`,
    /// `SubIFDs`, …), or `None` for IFDs in the IFD0 chain.
    #[must_use]
    pub const fn parent(&self) -> Option<IfdKind> {
        self.parent
    }

    /// Returns the TIFF-relative offset this IFD was read from, if any.
    #[must_use]
    pub const fn offset(&self) -> Option<u32> {
        self.offset
    }

    /// Returns the entries, sorted by tag id.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Returns entries whose tag appeared more than once in the source IFD,
    /// after the first occurrence. They are kept so nothing is lost.
    #[must_use]
    pub fn duplicates(&self) -> &[Entry] {
        &self.duplicates
    }

    /// Returns the entry for `tag`, if present.
    #[must_use]
    pub fn get(&self, tag: Tag) -> Option<&Entry> {
        self.entries
            .binary_search_by_key(&tag, |e| e.tag)
            .ok()
            .and_then(|i| self.entries.get(i))
    }

    /// Returns the number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the IFD has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Inserts an entry, replacing and returning any entry with the same tag.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidEntry`] if the tag belongs to another namespace (for
    /// example a GPS tag in IFD0), or is one the writer computes itself:
    /// IFD pointers, image-data offsets and lengths, and `OffsetSchema`.
    pub fn insert(&mut self, entry: Entry) -> Result<Option<Entry>> {
        let tag = entry.tag;
        if tag.namespace() != self.namespace() {
            return Err(Error::InvalidEntry {
                tag,
                reason: "tag belongs to another IFD's namespace",
            });
        }
        if tag
            .info()
            .is_some_and(|info| info.role != Role::Value && info.role != Role::MakerNote)
        {
            return Err(Error::InvalidEntry {
                tag,
                reason: "tag is computed by the writer",
            });
        }
        Ok(match self.entries.binary_search_by_key(&tag, |e| e.tag) {
            Ok(i) => self
                .entries
                .get_mut(i)
                .map(|slot| core::mem::replace(slot, entry)),
            Err(i) => {
                self.entries.insert(i, entry);
                None
            }
        })
    }

    /// Removes and returns the entry for `tag`. Duplicates of the tag are
    /// removed too.
    pub fn remove(&mut self, tag: Tag) -> Option<Entry> {
        self.duplicates.retain(|e| e.tag != tag);
        let index = self.entries.binary_search_by_key(&tag, |e| e.tag).ok()?;
        Some(self.entries.remove(index))
    }

    /// Drops the duplicate entries kept from the source. The writer does
    /// this anyway, since a TIFF IFD holds each tag once.
    pub fn clear_duplicates(&mut self) {
        self.duplicates.clear();
    }

    /// Keeps only the entries for which `keep` returns `true`.
    pub fn retain(&mut self, mut keep: impl FnMut(&Entry) -> bool) {
        self.entries.retain(&mut keep);
        self.duplicates.retain(keep);
    }

    /// Sorts entries by tag id and moves repeated tags to `duplicates`,
    /// keeping the first occurrence. Returns the duplicated tags.
    pub(crate) fn normalize(&mut self) -> Vec<Tag> {
        self.entries.sort_by_key(|e| e.tag);
        let mut kept: Vec<Entry> = Vec::with_capacity(self.entries.len());
        let mut repeated = Vec::new();
        for entry in self.entries.drain(..) {
            if kept.last().is_some_and(|last| last.tag == entry.tag) {
                repeated.push(entry.tag);
                self.duplicates.push(entry);
            } else {
                kept.push(entry);
            }
        }
        self.entries = kept;
        repeated
    }
}

/// A stable order for IFD kinds that are not SubIFDs.
const fn rank_of(kind: IfdKind) -> u8 {
    match kind {
        IfdKind::Primary => 0,
        IfdKind::Exif => 1,
        IfdKind::Interop => 2,
        IfdKind::Gps => 3,
        IfdKind::Thumbnail => 4,
        IfdKind::Chain(_) => 5,
        IfdKind::SubIfd(_) => 6,
    }
}

/// The parent an IFD of this kind normally has.
const fn default_parent(kind: IfdKind) -> Option<IfdKind> {
    match kind {
        IfdKind::Primary | IfdKind::Thumbnail | IfdKind::Chain(_) => None,
        IfdKind::Exif | IfdKind::Gps | IfdKind::SubIfd(_) => Some(IfdKind::Primary),
        IfdKind::Interop => Some(IfdKind::Exif),
    }
}

pub(crate) const fn namespace_of(kind: IfdKind) -> Namespace {
    match kind {
        IfdKind::Gps => Namespace::Gps,
        IfdKind::Interop => Namespace::Interop,
        _ => Namespace::Main,
    }
}

/// Options for reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReadOptions {
    /// Fail on the first diagnostic of [`Severity::Error`](crate::Severity::Error)
    /// instead of returning a partial result. Default: `false`.
    pub strict: bool,
    /// Resource caps. Default: [`Limits::DEFAULT`].
    pub limits: Limits,
    /// Extract the IFD1 JPEG thumbnail. Default: `true`.
    pub load_thumbnail: bool,
}

impl ReadOptions {
    /// The default options: lenient, default limits, thumbnail loaded.
    pub const DEFAULT: Self = Self {
        strict: false,
        limits: Limits::DEFAULT,
        load_thumbnail: true,
    };

    /// Sets [`ReadOptions::strict`].
    #[must_use]
    pub const fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Sets [`ReadOptions::limits`].
    #[must_use]
    pub const fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Sets [`ReadOptions::load_thumbnail`].
    #[must_use]
    pub const fn load_thumbnail(mut self, load: bool) -> Self {
        self.load_thumbnail = load;
        self
    }
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Exif metadata: a tree of IFDs, a thumbnail and the MakerNote location.
///
/// Equality compares content: byte order, IFDs and thumbnail. Where things
/// sat in the source (IFD offsets, the MakerNote offset, `OffsetSchema`) is
/// layout, not content, and is ignored.
#[derive(Debug, Clone)]
pub struct Exif {
    pub(crate) byte_order: ByteOrder,
    pub(crate) ifds: Vec<Ifd>,
    pub(crate) thumbnail: Option<Vec<u8>>,
    pub(crate) maker_note_offset: Option<u32>,
    pub(crate) offset_schema: Option<i32>,
    pub(crate) layout: Option<SourceLayout>,
}

/// What the reader learned about where things sat in the source block.
///
/// `free` lists the byte ranges the model fully accounts for (IFD tables,
/// value data, the extracted thumbnail): the writer may reuse them. Every
/// other byte (MakerNote data, image strips, vendor data referenced by
/// offset, padding) is kept in place when writing with the source.
#[derive(Debug, Clone, Default)]
pub(crate) struct SourceLayout {
    pub(crate) len: u32,
    pub(crate) free: Vec<core::ops::Range<u32>>,
    pub(crate) maker_note: Option<core::ops::Range<u32>>,
    /// The IFD holding the MakerNote that `maker_note` and
    /// [`Exif::maker_note_offset`] describe.
    pub(crate) maker_note_ifd: Option<IfdKind>,
    pub(crate) thumbnail: Option<u32>,
}

impl PartialEq for Exif {
    fn eq(&self, other: &Self) -> bool {
        self.byte_order == other.byte_order
            && self.ifds == other.ifds
            && self.thumbnail == other.thumbnail
    }
}

impl Eq for Exif {}

impl Default for Exif {
    fn default() -> Self {
        Self::new()
    }
}

impl Exif {
    /// Creates empty metadata in little-endian byte order.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            byte_order: ByteOrder::LittleEndian,
            ifds: Vec::new(),
            thumbnail: None,
            maker_note_offset: None,
            offset_schema: None,
            layout: None,
        }
    }

    /// Reads a raw TIFF block (the payload after `Exif\0\0` in JPEG, or the
    /// whole of a TIFF file) with the default [`ReadOptions`].
    ///
    /// Problems that do not prevent reading are returned as [`Diagnostics`]
    /// alongside a best-effort result.
    ///
    /// # Errors
    ///
    /// Fails if the TIFF header is invalid.
    pub fn from_tiff(data: &[u8]) -> Result<(Self, Diagnostics)> {
        Self::from_tiff_with(data, &ReadOptions::DEFAULT)
    }

    /// Reads a raw TIFF block with the given options.
    ///
    /// # Errors
    ///
    /// Fails if the TIFF header is invalid, or in strict mode on the first
    /// diagnostic of [`Severity::Error`](crate::Severity::Error).
    pub fn from_tiff_with(data: &[u8], options: &ReadOptions) -> Result<(Self, Diagnostics)> {
        let (exif, diagnostics) = Self::from_tiff_lenient(data, options)?;
        check_strict(&diagnostics, options)?;
        Ok((exif, diagnostics))
    }

    /// Reads a TIFF block, ignoring [`ReadOptions::strict`].
    pub(crate) fn from_tiff_lenient(
        data: &[u8],
        options: &ReadOptions,
    ) -> Result<(Self, Diagnostics)> {
        let header = Header::parse(data)?;
        let reader = Reader::new(data, header.byte_order);
        Ok(Self::from_reader(reader, header.ifd0_offset, options))
    }

    /// Walks the IFDs behind `reader`, starting at IFD0, ignoring
    /// [`ReadOptions::strict`].
    pub(crate) fn from_reader(
        reader: Reader<'_>,
        ifd0: u32,
        options: &ReadOptions,
    ) -> (Self, Diagnostics) {
        walk::Walker::new(reader, options).run(ifd0)
    }

    /// Returns the byte order.
    #[must_use]
    pub const fn byte_order(&self) -> ByteOrder {
        self.byte_order
    }

    /// Returns the IFDs, sorted by kind.
    #[must_use]
    pub fn ifds(&self) -> &[Ifd] {
        &self.ifds
    }

    /// Returns the IFD of the given kind, if present.
    #[must_use]
    pub fn ifd(&self, kind: IfdKind) -> Option<&Ifd> {
        self.ifds
            .binary_search_by_key(&kind, |ifd| ifd.kind)
            .ok()
            .and_then(|i| self.ifds.get(i))
    }

    /// Returns the entry for `tag`.
    ///
    /// Looks in the IFD the spec places the tag in first, then in every other
    /// IFD of the same namespace, since real files misplace tags.
    #[must_use]
    pub fn entry(&self, tag: Tag) -> Option<&Entry> {
        if let Some(info) = tag.info()
            && let Some(entry) = self.ifd(info.ifd).and_then(|ifd| ifd.get(tag))
        {
            return Some(entry);
        }
        self.ifds
            .iter()
            .filter(|ifd| ifd.namespace() == tag.namespace())
            .find_map(|ifd| ifd.get(tag))
    }

    /// Returns the value of `tag` converted to `T`, or `None` if absent.
    ///
    /// # Errors
    ///
    /// Fails if the stored value cannot be converted to `T` losslessly.
    pub fn get<T: FromValue>(&self, tag: Tag) -> Result<Option<T>, ValueError> {
        self.entry(tag).map(|e| T::from_value(&e.value)).transpose()
    }

    /// Iterates over every entry with the IFD it is in.
    pub fn entries(&self) -> impl Iterator<Item = (IfdKind, &Entry)> {
        self.ifds
            .iter()
            .flat_map(|ifd| ifd.entries.iter().map(move |e| (ifd.kind, e)))
    }

    /// Returns the IFD1 JPEG thumbnail, if present and loaded.
    #[must_use]
    pub fn thumbnail(&self) -> Option<&[u8]> {
        self.thumbnail.as_deref()
    }

    /// Returns the TIFF-relative offset of the MakerNote data in the source,
    /// used to keep the MakerNote in place when writing.
    #[must_use]
    pub const fn maker_note_offset(&self) -> Option<u32> {
        self.maker_note_offset
    }

    /// Returns the source's `OffsetSchema` (0xEA1D): how far an earlier
    /// writer moved the MakerNote. The writer keeps it consistent.
    #[must_use]
    pub const fn offset_schema(&self) -> Option<i32> {
        self.offset_schema
    }

    /// Sets the byte order used when writing.
    pub fn set_byte_order(&mut self, order: ByteOrder) {
        self.byte_order = order;
    }

    /// Returns the IFD of the given kind, creating it if it is missing.
    pub fn ifd_mut(&mut self, kind: IfdKind) -> &mut Ifd {
        let index = match self.ifds.binary_search_by_key(&kind, |ifd| ifd.kind) {
            Ok(i) => i,
            Err(i) => {
                self.ifds.insert(i, Ifd::new(kind));
                i
            }
        };
        // The index was just found or inserted, so it is in bounds.
        #[allow(clippy::indexing_slicing)]
        &mut self.ifds[index]
    }

    /// Removes and returns the IFD of the given kind.
    pub fn remove_ifd(&mut self, kind: IfdKind) -> Option<Ifd> {
        let index = self.ifds.binary_search_by_key(&kind, |ifd| ifd.kind).ok()?;
        Some(self.ifds.remove(index))
    }

    /// Inserts an entry into the IFD the spec places its tag in (IFD0 for
    /// unknown tags of the main namespace), replacing any previous value.
    ///
    /// # Errors
    ///
    /// As [`Ifd::insert`].
    pub fn insert(&mut self, entry: Entry) -> Result<Option<Entry>> {
        let tag = entry.tag;
        let kind = tag.info().map_or(
            match tag.namespace() {
                Namespace::Gps => IfdKind::Gps,
                Namespace::Interop => IfdKind::Interop,
                _ => IfdKind::Primary,
            },
            |info| info.ifd,
        );
        self.ifd_mut(kind).insert(entry)
    }

    /// Removes `tag` from every IFD of its namespace, returning the first
    /// value found.
    pub fn remove(&mut self, tag: Tag) -> Option<Entry> {
        let mut removed = None;
        for ifd in self
            .ifds
            .iter_mut()
            .filter(|ifd| ifd.namespace() == tag.namespace())
        {
            let entry = ifd.remove(tag);
            removed = removed.or(entry);
        }
        removed
    }

    /// Sets or removes the IFD1 JPEG thumbnail. Setting one creates IFD1 if
    /// it is missing, since the thumbnail is referenced from there; removing
    /// one leaves IFD1 and its other tags alone.
    pub fn set_thumbnail(&mut self, jpeg: Option<Vec<u8>>) {
        if jpeg.is_some() {
            self.ifd_mut(IfdKind::Thumbnail);
        }
        self.thumbnail = jpeg;
    }
}

/// In strict mode, turns the first error diagnostic into an [`Error`].
pub(crate) fn check_strict(diagnostics: &Diagnostics, options: &ReadOptions) -> Result<()> {
    if !options.strict {
        return Ok(());
    }
    match diagnostics.first_error() {
        None => Ok(()),
        Some(first) => Err(match first.kind {
            DiagnosticKind::LimitExceeded(limit) => Error::LimitExceeded(limit),
            kind => Error::Malformed {
                offset: first.offset.unwrap_or(0),
                reason: kind.reason(),
            },
        }),
    }
}

mod walk;

#[cfg(test)]
mod tests;

/// Builds models directly, for unit tests of the typed helpers.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::{Entry, Exif, Ifd};
    use crate::tag::Tag;
    use crate::value::Value;

    /// An [`Exif`] holding each value in the IFD the spec places its tag in.
    pub(crate) fn exif_with(values: &[(Tag, Value)]) -> Exif {
        let mut exif = Exif::new();
        for (tag, value) in values {
            let kind = tag.info().map_or(crate::IfdKind::Primary, |i| i.ifd);
            let index = match exif.ifds.binary_search_by_key(&kind, Ifd::kind) {
                Ok(i) => i,
                Err(i) => {
                    exif.ifds.insert(i, Ifd::new(kind));
                    i
                }
            };
            let ifd = &mut exif.ifds[index];
            ifd.entries.push(Entry::new(*tag, value.clone()));
            ifd.normalize();
        }
        exif
    }
}
