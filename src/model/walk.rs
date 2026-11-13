//! The IFD walker: reads a TIFF block into an [`Exif`], following IFD
//! pointers with an explicit stack, a visited set and resource limits, so it
//! always terminates and memory stays linear in the input.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::{Entry, Exif, Ifd, ReadOptions, SourceLayout, default_parent, rank_of};
use crate::diagnostic::{Diagnostic, DiagnosticKind, Diagnostics};
use crate::ifd::IfdKind;
use crate::limits::Limit;
use crate::tag::{Namespace, Role, Tag};
use crate::tiff::{DataError, Header, RawEntry, RawIfd, Reader};
use crate::value::{FromValue, Value};

/// Bad entries after which the rest of an IFD is treated as garbage.
pub(super) const MAX_BAD_ENTRIES: usize = 10;

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
pub(super) struct Walker<'a, 'o> {
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
    pub(super) fn new(reader: Reader<'a>, options: &'o ReadOptions) -> Self {
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

    pub(super) fn run(mut self, ifd0: u32) -> (Exif, Diagnostics) {
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
            // One value is never bigger than a metadata block may be, however
            // large the file (and so the budget) is.
            let size = usize::try_from(len).unwrap_or(usize::MAX);
            if size > self.options.limits.max_metadata {
                self.report(
                    Diagnostic::new(DiagnosticKind::LimitExceeded(Limit::Metadata))
                        .in_ifd(kind)
                        .for_tag(tag)
                        .at(raw.offset()),
                );
                return None;
            }
            // Out-of-line value data is rewritten from the model, so its
            // source range may be reused. The MakerNote is the exception.
            if tag != Tag::MAKER_NOTE && len > 4 {
                self.account(at, len);
            }
            if !self.charge(size, kind) {
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
            let size = usize::try_from(len).unwrap_or(usize::MAX);
            if in_bounds && size > self.options.limits.max_metadata {
                // Bigger than any metadata block may be: keep the references.
                self.report(
                    Diagnostic::new(DiagnosticKind::LimitExceeded(Limit::Metadata))
                        .in_ifd(ifd.kind)
                        .at(at),
                );
            } else if in_bounds && !self.charge(size, ifd.kind) {
                // Out of budget (already reported): keep the references.
            } else {
                if in_bounds && let Some(bytes) = self.reader.bytes(at, len) {
                    self.thumbnail = Some(bytes.into_owned());
                    self.thumbnail_at = Some(at);
                    self.account(at, len);
                    return;
                }
                self.report(
                    Diagnostic::new(DiagnosticKind::InvalidThumbnail)
                        .in_ifd(ifd.kind)
                        .at(at),
                );
            }
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
