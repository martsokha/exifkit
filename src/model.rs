//! The owned Exif model, and the reader that builds it from a TIFF block.
//!
//! See `docs/api.md#data-model` and `docs/architecture.md#safety-model`.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt;

use crate::diagnostic::{Diagnostic, DiagnosticKind, Diagnostics};
use crate::error::{Error, Result};
use crate::ifd::IfdKind;
use crate::limits::{Limit, Limits};
use crate::tag::{Namespace, Role, Tag};
use crate::tiff::{ByteOrder, DataError, Header, RawEntry, RawIfd, Reader};
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
        Walker::new(reader, options).run(ifd0)
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

/// Bad entries after which the rest of an IFD is treated as garbage.
const MAX_BAD_ENTRIES: usize = 10;

/// An IFD waiting to be read.
#[derive(Debug, Clone, Copy)]
struct Task {
    kind: IfdKind,
    parent: Option<IfdKind>,
    offset: u32,
    depth: u8,
}

/// Walks the IFD graph with an explicit stack, a visited set and resource
/// limits, so it always terminates and memory stays linear in the input.
struct Walker<'a, 'o> {
    reader: Reader<'a>,
    options: &'o ReadOptions,
    diagnostics: Diagnostics,
    visited: BTreeMap<u32, IfdKind>,
    ifds: BTreeMap<IfdKind, Ifd>,
    stack: Vec<Task>,
    budget: usize,
    used: usize,
    stopped: bool,
    next_sub_ifd: u16,
    thumbnail: Option<Vec<u8>>,
    maker_note_offset: Option<u32>,
    offset_schema: Option<i32>,
    free: Vec<core::ops::Range<u32>>,
    maker_note_range: Option<core::ops::Range<u32>>,
    maker_note_ifd: Option<IfdKind>,
    /// Each IFD's first out-of-line MakerNote: `(offset, length)`.
    maker_note_candidates: BTreeMap<IfdKind, (u32, u32)>,
    thumbnail_at: Option<u32>,
    /// For each SubIFD number handed out: its parent and its index in the
    /// parent's `SubIFDs` list. Used to renumber SubIFDs canonically.
    sub_ifd_slots: BTreeMap<u16, (IfdKind, u16)>,
}

impl<'a, 'o> Walker<'a, 'o> {
    fn new(reader: Reader<'a>, options: &'o ReadOptions) -> Self {
        Self {
            reader,
            options,
            diagnostics: Diagnostics::default(),
            visited: BTreeMap::new(),
            ifds: BTreeMap::new(),
            stack: Vec::new(),
            budget: options.limits.alloc_budget(reader.len()),
            used: 0,
            stopped: false,
            next_sub_ifd: 0,
            thumbnail: None,
            maker_note_offset: None,
            offset_schema: None,
            free: Vec::new(),
            maker_note_range: None,
            maker_note_ifd: None,
            maker_note_candidates: BTreeMap::new(),
            thumbnail_at: None,
            sub_ifd_slots: BTreeMap::new(),
        }
    }

    /// Records a source range the model fully accounts for.
    fn account(&mut self, start: u32, len: u32) {
        let end = start.saturating_add(len);
        let limit = u32::try_from(self.reader.len()).unwrap_or(u32::MAX);
        if start < end && end <= limit {
            self.free.push(start..end);
        }
    }

    fn run(mut self, ifd0: u32) -> (Exif, Diagnostics) {
        self.stack.push(Task {
            kind: IfdKind::Primary,
            parent: None,
            offset: ifd0,
            depth: 0,
        });
        while let Some(task) = self.stack.pop() {
            if self.stopped {
                break;
            }
            self.visit(task);
        }
        self.renumber_sub_ifds();
        self.choose_maker_note();
        let exif = Exif {
            byte_order: self.reader.byte_order(),
            ifds: self.ifds.into_values().collect(),
            thumbnail: self.thumbnail,
            maker_note_offset: self.maker_note_offset,
            offset_schema: self.offset_schema,
            layout: Some(SourceLayout {
                len: u32::try_from(self.reader.len()).unwrap_or(u32::MAX),
                free: merge(self.free),
                maker_note: self.maker_note_range,
                maker_note_ifd: self.maker_note_ifd,
                thumbnail: self.thumbnail_at,
            }),
        };
        (exif, self.diagnostics)
    }

    /// Renumbers SubIFDs densely, in tree order: by parent, then by index in
    /// the parent's `SubIFDs` list. Numbers no longer depend on file layout
    /// or on pointers that failed, so they survive a write and read back.
    fn renumber_sub_ifds(&mut self) {
        let path = |mut kind: IfdKind| {
            // Parents always got their number before their children, so this
            // walks strictly downwards; the bound is just a backstop.
            let mut path = Vec::new();
            for _ in 0..=u16::MAX {
                let IfdKind::SubIfd(n) = kind else {
                    let chain = if let IfdKind::Chain(n) = kind { n } else { 0 };
                    path.push((rank_of(kind), chain));
                    break;
                };
                let Some(&(parent, index)) = self.sub_ifd_slots.get(&n) else {
                    break;
                };
                path.push((u8::MAX, index));
                kind = parent;
            }
            path.reverse();
            path
        };
        let mut subs: Vec<(Vec<(u8, u16)>, u16)> = self
            .ifds
            .keys()
            .filter_map(|kind| match kind {
                IfdKind::SubIfd(n) => Some((path(*kind), *n)),
                _ => None,
            })
            .collect();
        subs.sort();
        let renumbered: BTreeMap<u16, u16> = subs
            .iter()
            .enumerate()
            .filter_map(|(new, (_, old))| Some((*old, u16::try_from(new).ok()?)))
            .collect();
        let remap = |kind: IfdKind| match kind {
            IfdKind::SubIfd(n) => renumbered.get(&n).map_or(kind, |&m| IfdKind::SubIfd(m)),
            other => other,
        };
        let ifds = core::mem::take(&mut self.ifds);
        for (kind, mut ifd) in ifds {
            let kind = remap(kind);
            ifd.kind = kind;
            ifd.parent = ifd.parent.map(remap);
            self.ifds.insert(kind, ifd);
        }
        // Everything else that names a SubIFD must follow.
        let candidates = core::mem::take(&mut self.maker_note_candidates);
        self.maker_note_candidates = candidates.into_iter().map(|(k, v)| (remap(k), v)).collect();
    }

    /// Picks the MakerNote the writer anchors: the Exif IFD's, else the one
    /// in the first IFD in canonical order. The choice depends only on the
    /// model, never on where IFDs sat in the file, so it survives a write.
    fn choose_maker_note(&mut self) {
        let order = |kind: &IfdKind| {
            let inner = match *kind {
                IfdKind::Chain(n) | IfdKind::SubIfd(n) => n,
                _ => 0,
            };
            (u8::from(*kind != IfdKind::Exif), rank_of(*kind), inner)
        };
        let chosen = self
            .maker_note_candidates
            .iter()
            .filter(|(kind, _)| self.ifds.contains_key(kind))
            .min_by_key(|(kind, _)| order(kind))
            .map(|(&kind, &(at, len))| (kind, at, len));
        if let Some((kind, at, len)) = chosen {
            self.maker_note_ifd = Some(kind);
            self.maker_note_offset = Some(at);
            self.maker_note_range = Some(at..at.saturating_add(len));
        }
    }

    fn report(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// Charges `bytes` against the allocation budget. Once the budget is
    /// exhausted the walk stops.
    fn charge(&mut self, bytes: usize, kind: IfdKind) -> bool {
        self.used = self.used.saturating_add(bytes);
        if self.used > self.budget {
            if !self.stopped {
                self.report(
                    Diagnostic::new(DiagnosticKind::LimitExceeded(Limit::Alloc)).in_ifd(kind),
                );
            }
            self.stopped = true;
            return false;
        }
        true
    }

    fn visit(&mut self, task: Task) {
        let Task {
            kind,
            parent,
            offset,
            depth,
        } = task;
        let limits = self.options.limits;

        if depth > limits.max_depth {
            self.report(
                Diagnostic::new(DiagnosticKind::LimitExceeded(Limit::Depth))
                    .in_ifd(kind)
                    .at(offset),
            );
            return;
        }
        if self.ifds.contains_key(&kind) {
            // Two pointers to the same IFD (a shared link) are fine; two
            // pointers to different IFDs of one kind are not.
            if self.visited.get(&offset) != Some(&kind) {
                self.report(
                    Diagnostic::new(DiagnosticKind::DuplicateIfd)
                        .in_ifd(kind)
                        .at(offset),
                );
            }
            return;
        }
        if self.visited.contains_key(&offset) {
            self.report(
                Diagnostic::new(DiagnosticKind::Cycle)
                    .in_ifd(kind)
                    .at(offset),
            );
            return;
        }
        if self.ifds.len() >= usize::from(limits.max_ifds) {
            self.report(Diagnostic::new(DiagnosticKind::LimitExceeded(Limit::Ifds)).in_ifd(kind));
            self.stopped = true;
            return;
        }
        let raw = (offset >= Header::LEN as u32)
            .then(|| RawIfd::parse(self.reader, offset))
            .flatten();
        let Some(raw) = raw else {
            self.report(
                Diagnostic::new(DiagnosticKind::InvalidIfdOffset)
                    .in_ifd(kind)
                    .at(offset),
            );
            return;
        };
        self.visited.insert(offset, kind);
        let table = 2u32
            .saturating_add(12u32.saturating_mul(u32::from(raw.len())))
            .saturating_add(4);
        self.account(offset, table);
        if raw.is_truncated() {
            self.report(
                Diagnostic::new(DiagnosticKind::Truncated)
                    .in_ifd(kind)
                    .at(offset),
            );
        }

        let mut ifd = Ifd::new(kind);
        ifd.parent = parent;
        ifd.offset = Some(offset);
        let mut thumbnail = ThumbnailRefs::default();
        let mut previous: Option<u16> = None;
        let mut unsorted = false;

        let mut bad_entries = 0usize;
        for (index, entry) in raw.entries().enumerate() {
            if self.stopped {
                break;
            }
            if entry.ty().is_none() {
                // **quirk** (as ExifTool): an invalid type in the first entry
                // means the offset does not point at a real IFD, except for
                // Sony ILCE models, whose IFDs can start with an empty entry.
                if index == 0 && !self.is_sony_ilce() {
                    self.report(
                        Diagnostic::new(DiagnosticKind::CorruptIfd)
                            .in_ifd(kind)
                            .at(offset),
                    );
                    return;
                }
                // Type 0 later on is zero padding, not data.
                if entry.type_id == 0 {
                    continue;
                }
            }
            if index >= usize::from(limits.max_entries) {
                self.report(
                    Diagnostic::new(DiagnosticKind::LimitExceeded(Limit::Entries))
                        .in_ifd(kind)
                        .at(offset),
                );
                break;
            }
            if !self.charge(size_of::<Entry>(), kind) {
                break;
            }
            unsorted |= previous.is_some_and(|p| p > entry.tag);
            previous = Some(entry.tag);
            let before = self.diagnostics.len();
            self.entry(&mut ifd, &mut thumbnail, entry, depth);
            if self
                .diagnostics
                .since(before)
                .iter()
                .any(|d| d.kind.is_bad_entry())
            {
                bad_entries = bad_entries.saturating_add(1);
            }
            // **quirk** (as ExifTool): this many bad entries means the rest
            // of the IFD is garbage; keep what came before.
            if bad_entries > MAX_BAD_ENTRIES {
                self.report(
                    Diagnostic::new(DiagnosticKind::CorruptIfd)
                        .in_ifd(kind)
                        .at(offset),
                );
                break;
            }
        }

        if unsorted {
            self.report(
                Diagnostic::new(DiagnosticKind::UnsortedEntries)
                    .in_ifd(kind)
                    .at(offset),
            );
        }
        self.thumbnail(&mut ifd, thumbnail);
        for tag in ifd.normalize() {
            self.report(
                Diagnostic::new(DiagnosticKind::DuplicateTag)
                    .in_ifd(kind)
                    .for_tag(tag),
            );
        }
        if let Some(next) = raw.next()
            && let Some(next_kind) = next_in_chain(kind)
        {
            self.stack.push(Task {
                kind: next_kind,
                parent: None,
                offset: next,
                depth,
            });
        }
        self.ifds.insert(kind, ifd);
    }

    /// Whether IFD0 says this is a Sony ILCE camera.
    fn is_sony_ilce(&self) -> bool {
        self.ifds
            .get(&IfdKind::Primary)
            .and_then(|ifd| ifd.get(Tag::MODEL))
            .and_then(|entry| entry.value().text())
            .is_some_and(|model| model.starts_with("ILCE"))
    }

    /// Handles one raw entry: follows pointers, sets aside thumbnail
    /// references, and decodes and checks everything else.
    fn entry(
        &mut self,
        ifd: &mut Ifd,
        thumbnail: &mut ThumbnailRefs,
        raw: RawEntry<'a>,
        depth: u8,
    ) {
        let kind = ifd.kind;
        let tag = Tag::new(ifd.namespace(), raw.tag);

        if let Some(info) = tag.info()
            && let Role::Pointer(target) = info.role
        {
            self.pointer(kind, tag, raw, target, depth);
            return;
        }

        let Some(value) = self.decode(kind, tag, raw) else {
            return;
        };

        if kind == IfdKind::Thumbnail && self.options.load_thumbnail {
            if tag == Tag::JPEG_INTERCHANGE_FORMAT {
                thumbnail.offset = Some((u32::from_value(&value).ok(), Entry::new(tag, value)));
                return;
            }
            if tag == Tag::JPEG_INTERCHANGE_FORMAT_LENGTH {
                thumbnail.length = Some((u32::from_value(&value).ok(), Entry::new(tag, value)));
                return;
            }
        }
        // OffsetSchema is layout (how far the MakerNote moved), kept apart
        // from content so the writer can keep it consistent.
        // The model never holds tags the writer computes. A malformed
        // OffsetSchema (not one SLONG) describes no valid move: it is
        // reported and dropped, like a broken pointer.
        if tag == Tag::OFFSET_SCHEMA {
            match i32::from_value(&value) {
                Ok(schema) => self.offset_schema = Some(schema),
                Err(_) => self.check(kind, tag, &value, raw.count),
            }
            return;
        }
        // Each IFD's first out-of-line MakerNote (the one a duplicate check
        // keeps) is a candidate; `choose_maker_note` picks one after the walk.
        if tag == Tag::MAKER_NOTE
            && !ifd.entries.iter().any(|e| e.tag == Tag::MAKER_NOTE)
            && let Ok(len) = raw.byte_len()
            && len > 4
            && let Ok(at) = raw.data_offset()
        {
            self.maker_note_candidates.entry(kind).or_insert((at, len));
        }
        self.check(kind, tag, &value, raw.count);
        let mut entry = Entry::new(tag, value);
        entry.source = raw
            .byte_len()
            .ok()
            .filter(|&len| len > 4)
            .and_then(|_| raw.data_offset().ok());
        ifd.entries.push(entry);
    }

    /// Decodes an entry's value, reporting why when it cannot.
    fn decode(&mut self, kind: IfdKind, tag: Tag, raw: RawEntry<'a>) -> Option<Value> {
        let Some(ty) = raw.ty() else {
            self.report(
                Diagnostic::new(DiagnosticKind::UnknownType(raw.type_id))
                    .in_ifd(kind)
                    .for_tag(tag)
                    .at(raw.offset()),
            );
            return Some(Value::Unknown {
                type_id: raw.type_id,
                count: raw.count,
                field: raw.field,
            });
        };
        // Bounds and budget come first: from a stream, the bytes are only
        // read once they are known to fit.
        if let (Ok(len), Ok(at)) = (raw.byte_len(), raw.data_offset())
            && u64::from(at).saturating_add(u64::from(len))
                <= u64::try_from(self.reader.len()).unwrap_or(u64::MAX)
        {
            // Out-of-line value data is rewritten from the model, so its
            // source range may be reused. The MakerNote is the exception.
            if tag != Tag::MAKER_NOTE && len > 4 {
                self.account(at, len);
            }
            if !self.charge(usize::try_from(len).unwrap_or(usize::MAX), kind) {
                return None;
            }
        }
        match raw.data() {
            Ok(data) => Some(Value::decode(ty, &data, self.reader.byte_order())),
            Err(DataError::OutOfBounds { offset }) => {
                self.report(
                    Diagnostic::new(DiagnosticKind::OutOfBounds)
                        .in_ifd(kind)
                        .for_tag(tag)
                        .at(offset),
                );
                None
            }
            Err(DataError::TooLarge | DataError::UnknownType(_)) => {
                self.report(
                    Diagnostic::new(DiagnosticKind::ValueTooLarge)
                        .in_ifd(kind)
                        .for_tag(tag)
                        .at(raw.offset()),
                );
                None
            }
        }
    }

    /// Reports spec deviations of a decoded value. The value is kept as is.
    fn check(&mut self, kind: IfdKind, tag: Tag, value: &Value, count: u32) {
        let diagnostic = |k| Diagnostic::new(k).in_ifd(kind).for_tag(tag);
        if let Value::Utf8(_) = value
            && value.text().is_none()
        {
            self.report(diagnostic(DiagnosticKind::InvalidUtf8));
        }
        let Some(info) = tag.info() else {
            return;
        };
        if let Some(ty) = value.ty()
            && !info.types.contains(&ty)
        {
            self.report(diagnostic(DiagnosticKind::TypeMismatch { found: ty }));
        }
        if !info.count.allows(count) {
            self.report(diagnostic(DiagnosticKind::CountMismatch { found: count }));
        }
        // IFD0, IFD1 and SubIFDs share TIFF tags; only the Exif IFD is distinct.
        if tag.namespace() == Namespace::Main
            && (info.ifd == IfdKind::Exif) != (kind == IfdKind::Exif)
        {
            self.report(diagnostic(DiagnosticKind::MisplacedTag {
                expected: info.ifd,
            }));
        }
    }

    /// Follows a pointer tag. Pointers are structure, not entries.
    fn pointer(&mut self, kind: IfdKind, tag: Tag, raw: RawEntry<'a>, target: IfdKind, depth: u8) {
        let invalid = |at| {
            Diagnostic::new(DiagnosticKind::InvalidPointer)
                .in_ifd(kind)
                .for_tag(tag)
                .at(at)
        };
        let Some(value) = self.decode(kind, tag, raw) else {
            return;
        };
        // **quirk:** some writers store pointers as SHORT.
        let offsets: Vec<u32> = match Vec::<u32>::from_value(&value) {
            Ok(offsets) if !offsets.is_empty() => offsets,
            _ => {
                self.report(invalid(raw.offset()));
                return;
            }
        };
        let depth = depth.saturating_add(1);
        if let IfdKind::SubIfd(_) = target {
            // Per parent, not per tag: the writer merges duplicate SubIFDs
            // tags into one, which must read back with the same children.
            let existing = self
                .sub_ifd_slots
                .values()
                .filter(|(parent, _)| *parent == kind)
                .count();
            let max = usize::from(self.options.limits.max_sub_ifds).saturating_sub(existing);
            if offsets.len() > max {
                self.report(
                    Diagnostic::new(DiagnosticKind::LimitExceeded(Limit::SubIfds))
                        .in_ifd(kind)
                        .for_tag(tag),
                );
            }
            for offset in offsets.into_iter().take(max) {
                let Some(next) = self.next_sub_ifd.checked_add(1) else {
                    break;
                };
                // Index among all of this parent's SubIFDs, across every
                // SubIFDs tag it has (a duplicate tag adds a second list).
                let index = self
                    .sub_ifd_slots
                    .values()
                    .filter(|(parent, _)| *parent == kind)
                    .count();
                let index = u16::try_from(index).unwrap_or(u16::MAX);
                self.sub_ifd_slots.insert(self.next_sub_ifd, (kind, index));
                self.stack.push(Task {
                    kind: IfdKind::SubIfd(self.next_sub_ifd),
                    parent: Some(kind),
                    offset,
                    depth,
                });
                self.next_sub_ifd = next;
            }
        } else {
            if offsets.len() != 1 {
                self.report(
                    Diagnostic::new(DiagnosticKind::CountMismatch { found: raw.count })
                        .in_ifd(kind)
                        .for_tag(tag),
                );
            }
            if let Some(&offset) = offsets.first() {
                // Exif, GPS and Interop IFDs have one canonical parent, which
                // the writer uses; a pointer found elsewhere (a quirk) does
                // not change that. Only SubIFDs hang off different IFDs.
                self.stack.push(Task {
                    kind: target,
                    parent: default_parent(target),
                    offset,
                    depth,
                });
            }
        }
    }

    /// Extracts the IFD1 JPEG thumbnail. If it cannot be extracted, the
    /// reference tags are kept as ordinary entries so nothing is lost.
    fn thumbnail(&mut self, ifd: &mut Ifd, refs: ThumbnailRefs) {
        let ThumbnailRefs { offset, length } = refs;
        if let (Some((Some(at), _)), Some((Some(len), _))) = (&offset, &length) {
            let (at, len) = (*at, *len);
            // Bounds and budget first, so a stream reads only what fits.
            let in_bounds = u64::from(at).saturating_add(u64::from(len))
                <= u64::try_from(self.reader.len()).unwrap_or(u64::MAX);
            if in_bounds {
                if !self.charge(usize::try_from(len).unwrap_or(usize::MAX), ifd.kind) {
                    return;
                }
                if let Some(bytes) = self.reader.bytes(at, len) {
                    self.thumbnail = Some(bytes.into_owned());
                    self.thumbnail_at = Some(at);
                    self.account(at, len);
                    return;
                }
            }
            self.report(
                Diagnostic::new(DiagnosticKind::InvalidThumbnail)
                    .in_ifd(ifd.kind)
                    .at(at),
            );
        }
        ifd.entries.extend(offset.map(|(_, e)| e));
        ifd.entries.extend(length.map(|(_, e)| e));
    }
}

/// The IFD1 thumbnail reference tags, held back until the IFD is done.
#[derive(Default)]
struct ThumbnailRefs {
    offset: Option<(Option<u32>, Entry)>,
    length: Option<(Option<u32>, Entry)>,
}

/// Sorts and merges ranges.
fn merge(mut ranges: Vec<core::ops::Range<u32>>) -> Vec<core::ops::Range<u32>> {
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<core::ops::Range<u32>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// The kind of the IFD that follows `kind` in the IFD0 chain.
fn next_in_chain(kind: IfdKind) -> Option<IfdKind> {
    match kind {
        IfdKind::Primary => Some(IfdKind::Thumbnail),
        IfdKind::Thumbnail => Some(IfdKind::Chain(2)),
        IfdKind::Chain(n) => n.checked_add(1).map(IfdKind::Chain),
        _ => None,
    }
}

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
