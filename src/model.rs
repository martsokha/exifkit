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
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    tag: Tag,
    value: Value,
}

impl Entry {
    /// Creates an entry.
    #[must_use]
    pub const fn new(tag: Tag, value: Value) -> Self {
        Self { tag, value }
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
#[derive(Debug, Clone, PartialEq)]
pub struct Ifd {
    kind: IfdKind,
    offset: Option<u32>,
    entries: Vec<Entry>,
    duplicates: Vec<Entry>,
}

impl Ifd {
    /// Creates an empty IFD.
    #[must_use]
    pub const fn new(kind: IfdKind) -> Self {
        Self {
            kind,
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

    /// Sorts entries by tag id and moves repeated tags to `duplicates`,
    /// keeping the first occurrence. Returns the duplicated tags.
    fn normalize(&mut self) -> Vec<Tag> {
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

const fn namespace_of(kind: IfdKind) -> Namespace {
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
#[derive(Debug, Clone, PartialEq)]
pub struct Exif {
    byte_order: ByteOrder,
    ifds: Vec<Ifd>,
    thumbnail: Option<Vec<u8>>,
    maker_note_offset: Option<u32>,
}

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
        let header = Header::parse(data)?;
        let reader = Reader::new(data, header.byte_order);
        let (exif, diagnostics) = Walker::new(reader, options).run(header.ifd0_offset);
        if options.strict
            && let Some(first) = diagnostics.first_error()
        {
            return Err(match first.kind {
                DiagnosticKind::LimitExceeded(limit) => Error::LimitExceeded(limit),
                kind => Error::Malformed {
                    offset: u64::from(first.offset.unwrap_or(0)),
                    reason: kind.reason(),
                },
            });
        }
        Ok((exif, diagnostics))
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
}

/// Bad entries after which the rest of an IFD is treated as garbage.
const MAX_BAD_ENTRIES: usize = 10;

/// An IFD waiting to be read.
#[derive(Debug, Clone, Copy)]
struct Task {
    kind: IfdKind,
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
        }
    }

    fn run(mut self, ifd0: u32) -> (Exif, Diagnostics) {
        self.stack.push(Task {
            kind: IfdKind::Primary,
            offset: ifd0,
            depth: 0,
        });
        while let Some(task) = self.stack.pop() {
            if self.stopped {
                break;
            }
            self.visit(task);
        }
        let exif = Exif {
            byte_order: self.reader.byte_order(),
            ifds: self.ifds.into_values().collect(),
            thumbnail: self.thumbnail,
            maker_note_offset: self.maker_note_offset,
        };
        (exif, self.diagnostics)
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
        if raw.is_truncated() {
            self.report(
                Diagnostic::new(DiagnosticKind::Truncated)
                    .in_ifd(kind)
                    .at(offset),
            );
        }

        let mut ifd = Ifd::new(kind);
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
        if tag == Tag::MAKER_NOTE && raw.byte_len().is_ok_and(|len| len > 4) {
            self.maker_note_offset = raw.data_offset().ok();
        }
        self.check(kind, tag, &value, raw.count);
        ifd.entries.push(Entry::new(tag, value));
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
        match raw.data() {
            Ok(data) => self
                .charge(data.len(), kind)
                .then(|| Value::decode(ty, data, self.reader.byte_order())),
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
            let max = usize::from(self.options.limits.max_sub_ifds);
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
                self.stack.push(Task {
                    kind: IfdKind::SubIfd(self.next_sub_ifd),
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
                self.stack.push(Task {
                    kind: target,
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
            if let Some(bytes) = self.reader.bytes(at, len) {
                if self.charge(bytes.len(), ifd.kind) {
                    self.thumbnail = Some(bytes.to_vec());
                }
                return;
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
