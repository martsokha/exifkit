//! Layout planning and serialization.
//!
//! Planning decides where every IFD table and value goes before anything is
//! written, so serialization is a single forward pass and the exact output
//! size is known up front. The allocator places blocks in write order and
//! steps over reserved ranges: the anchored MakerNote, and (with a source)
//! the image data the model references by offset, which is copied verbatim.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

use super::value::{encode, put32};
use super::{MakerNotePolicy, WriteOptions};
use crate::error::{Error, Result};
use crate::ifd::IfdKind;
use crate::model::Exif;
use crate::tag::{Role, Tag};
use crate::tiff::{ByteOrder, Header, Type};
use crate::value::FromValue;

/// Where an entry's value comes from.
enum Data {
    /// Encoded bytes.
    Bytes(Vec<u8>),
    /// Offsets of child IFD tables, known after layout.
    Tables(Vec<IfdKind>),
    /// Offset of the thumbnail blob, known after layout.
    ThumbnailOffset,
    /// `OffsetSchema` for a moved MakerNote: `base + (new - original)`.
    Schema { base: i32, original: u32 },
}

/// What happens to `OffsetSchema`.
#[derive(Clone, Copy)]
enum Schema {
    /// The MakerNote stays put: keep the source's value.
    Keep(i32),
    /// The MakerNote moves from `original`: add the distance to `base`.
    Moved { base: i32, original: u32 },
}

struct Out {
    id: u16,
    ty: Type,
    count: u32,
    data: Data,
    /// Offset of out-of-line data; unused when the value fits inline.
    at: u64,
    /// Whether this is the anchored MakerNote.
    anchored: bool,
    /// The value's source offset, when it is unchanged and stays there.
    pin: Option<u64>,
}

impl Out {
    fn byte_len(&self) -> u64 {
        u64::from(self.count).saturating_mul(u64::from(self.ty.size()))
    }
}

struct Node {
    kind: IfdKind,
    entries: Vec<Out>,
    at: u64,
}

pub(super) struct Plan<'a> {
    order: ByteOrder,
    nodes: Vec<Node>,
    thumbnail: Option<(&'a [u8], u64)>,
    copies: Vec<(u64, &'a [u8])>,
    maker_note_at: Option<u64>,
    len: u64,
}

/// A pointer tag to write in a parent IFD: its id and the IFDs it lists.
type PointerTag = (u16, Vec<IfdKind>);

/// Tag pairs that reference data by offset and length.
const DATA_PAIRS: [(Tag, Tag); 3] = [
    (Tag::STRIP_OFFSETS, Tag::STRIP_BYTE_COUNTS),
    (Tag::TILE_OFFSETS, Tag::TILE_BYTE_COUNTS),
    (
        Tag::JPEG_INTERCHANGE_FORMAT,
        Tag::JPEG_INTERCHANGE_FORMAT_LENGTH,
    ),
];

/// Write order: IFD0's subtree first, the thumbnail and chain last, so the
/// thumbnail blob (written after all tables) sits at the end.
fn rank(kind: IfdKind) -> (u8, u16) {
    match kind {
        IfdKind::Primary => (0, 0),
        IfdKind::Exif => (1, 0),
        IfdKind::Interop => (2, 0),
        IfdKind::Gps => (3, 0),
        IfdKind::SubIfd(n) => (4, n),
        IfdKind::Thumbnail => (5, 0),
        IfdKind::Chain(n) => (6, n),
    }
}

/// The IFD whose pointer tag leads to `kind` in the output.
fn parent_of(exif: &Exif, kind: IfdKind) -> Option<IfdKind> {
    match kind {
        IfdKind::Primary => None,
        IfdKind::Exif | IfdKind::Gps => Some(IfdKind::Primary),
        IfdKind::Interop => Some(IfdKind::Exif),
        IfdKind::Thumbnail => Some(IfdKind::Primary),
        IfdKind::Chain(2) => Some(IfdKind::Thumbnail),
        IfdKind::Chain(n) => Some(IfdKind::Chain(n.saturating_sub(1))),
        IfdKind::SubIfd(_) => Some(
            exif.ifd(kind)
                .and_then(|ifd| ifd.parent())
                .unwrap_or(IfdKind::Primary),
        ),
    }
}

/// The ranges of `source` the model's offset/length pairs reference.
fn data_ranges(exif: &Exif, source: &[u8], skip_thumbnail_refs: bool) -> Result<Vec<Range<u64>>> {
    let mut ranges = Vec::new();
    let source_len = u64::try_from(source.len()).unwrap_or(u64::MAX);
    for ifd in exif.ifds() {
        for (offsets_tag, lengths_tag) in DATA_PAIRS {
            if skip_thumbnail_refs
                && ifd.kind() == IfdKind::Thumbnail
                && offsets_tag == Tag::JPEG_INTERCHANGE_FORMAT
            {
                continue;
            }
            let (Some(offsets), Some(lengths)) = (ifd.get(offsets_tag), ifd.get(lengths_tag))
            else {
                continue;
            };
            let (Ok(offsets), Ok(lengths)) = (
                Vec::<u32>::from_value(offsets.value()),
                Vec::<u32>::from_value(lengths.value()),
            ) else {
                continue;
            };
            for (offset, length) in offsets.into_iter().zip(lengths) {
                let range = u64::from(offset)..u64::from(offset).saturating_add(u64::from(length));
                if range.is_empty() || range.end > source_len {
                    // Dangling in the source too: keep the entry, nothing to copy.
                    continue;
                }
                if range.start < Header::LEN as u64 {
                    return Err(Error::Malformed {
                        offset: range.start,
                        reason: "image data overlaps the TIFF header",
                    });
                }
                ranges.push(range);
            }
        }
    }
    Ok(ranges)
}

/// The parts of `[Header::LEN, len)` not covered by `free` (sorted, merged)
/// or `also_free`: the source bytes the writer must keep in place.
fn kept_ranges(free: &[Range<u32>], also_free: Option<&Range<u32>>, len: u32) -> Vec<Range<u64>> {
    let mut covered: Vec<Range<u64>> = free
        .iter()
        .chain(also_free)
        .map(|r| u64::from(r.start)..u64::from(r.end))
        .collect();
    covered.sort_by_key(|r| r.start);
    let mut kept = Vec::new();
    let mut at = Header::LEN as u64;
    for range in covered {
        if range.start > at {
            kept.push(at..range.start);
        }
        at = at.max(range.end);
    }
    if at < u64::from(len) {
        kept.push(at..u64::from(len));
    }
    kept
}

/// Whether the offset/length pair `tag` belongs to references data outside
/// `source` (or, for an offset without its length, starts outside it).
fn dangles(ifd: &crate::model::Ifd, tag: Tag, source: &[u8]) -> bool {
    let Some((offsets_tag, lengths_tag)) =
        DATA_PAIRS.into_iter().find(|(o, l)| *o == tag || *l == tag)
    else {
        return false;
    };
    let source_len = u64::try_from(source.len()).unwrap_or(u64::MAX);
    let Some(offsets) = ifd
        .get(offsets_tag)
        .and_then(|e| Vec::<u32>::from_value(e.value()).ok())
    else {
        return false;
    };
    let lengths = ifd
        .get(lengths_tag)
        .and_then(|e| Vec::<u32>::from_value(e.value()).ok());
    offsets.iter().enumerate().any(|(i, &offset)| {
        let length = lengths
            .as_ref()
            .and_then(|l| l.get(i))
            .copied()
            .unwrap_or(0);
        u64::from(offset).saturating_add(u64::from(length)) > source_len
            || u64::from(offset) >= source_len
    })
}

/// `at` if the source holds exactly `bytes` there (past the header): the
/// value is unchanged and can stay where it was.
fn pinned_at(source: Option<&[u8]>, at: Option<u32>, bytes: &[u8]) -> Option<u64> {
    let at = at?;
    let start = usize::try_from(at).ok()?;
    if start < Header::LEN || bytes.len() <= 4 {
        return None;
    }
    let end = start.checked_add(bytes.len())?;
    (source?.get(start..end)? == bytes).then_some(u64::from(at))
}

fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Places blocks in order, stepping over reserved ranges.
struct Allocator {
    pos: u64,
    reserved: Vec<Range<u64>>,
}

impl Allocator {
    fn alloc(&mut self, len: u64) -> u64 {
        let mut at = self.pos.saturating_add(self.pos & 1);
        // Reserved ranges are sorted; one pass suffices because `at` only grows.
        for range in &self.reserved {
            if overlaps(&(at..at.saturating_add(len.max(1))), range) {
                at = range.end.saturating_add(range.end & 1);
            }
        }
        self.pos = at.saturating_add(len);
        at
    }

    fn end(&self) -> u64 {
        self.reserved.iter().map(|r| r.end).fold(self.pos, u64::max)
    }
}

impl<'a> Plan<'a> {
    pub(super) fn new(exif: &'a Exif, options: &WriteOptions<'a>) -> Result<Self> {
        let order = options.byte_order.unwrap_or(exif.byte_order());
        let thumbnail = exif.thumbnail();

        // Which IFDs to write: the model's, plus every parent they need.
        let mut kinds: BTreeSet<(u8, u16)> = BTreeSet::new();
        let mut by_rank: BTreeMap<(u8, u16), IfdKind> = BTreeMap::new();
        let mut add = |kind: IfdKind, kinds: &mut BTreeSet<(u8, u16)>| {
            let mut next = Some(kind);
            while let Some(k) = next {
                if !kinds.insert(rank(k)) {
                    break;
                }
                by_rank.insert(rank(k), k);
                next = parent_of(exif, k);
            }
        };
        add(IfdKind::Primary, &mut kinds);
        for ifd in exif.ifds() {
            add(ifd.kind(), &mut kinds);
        }
        if thumbnail.is_some() {
            add(IfdKind::Thumbnail, &mut kinds);
        }

        // With the source this model was read from, every byte the model
        // does not account for (MakerNote data, image strips, vendor data
        // referenced by offset, padding) stays in place. A source the model
        // was not read from only keeps the image data it references.
        let layout = options.source.and_then(|source| {
            exif.layout
                .as_ref()
                .filter(|l| usize::try_from(l.len).ok() == Some(source.len()))
        });
        let layout_source = layout.and(options.source);
        let has_data_refs = exif.ifds().iter().any(|ifd| {
            ifd.entries().iter().any(|e| {
                e.tag()
                    .info()
                    .is_some_and(|i| matches!(i.role, Role::DataOffset | Role::DataLength))
                    && !(thumbnail.is_some() && ifd.kind() == IfdKind::Thumbnail)
            })
        });
        let mut reserved = match (options.source, layout) {
            (Some(_), Some(layout)) => {
                kept_ranges(&layout.free, layout.maker_note.as_ref(), layout.len)
            }
            (Some(source), None) => data_ranges(exif, source, thumbnail.is_some())?,
            (None, _) if has_data_refs => {
                return Err(Error::Unsupported {
                    what: "image data referenced by offset (strips, tiles or an unextracted \
                           thumbnail) needs WriteOptions::source",
                });
            }
            (None, _) => Vec::new(),
        };
        let copies: Vec<(u64, &'a [u8])> = match options.source {
            Some(source) => reserved
                .iter()
                .filter_map(|r| {
                    let start = usize::try_from(r.start).ok()?;
                    let end = usize::try_from(r.end).ok()?;
                    Some((r.start, source.get(start..end)?))
                })
                .collect(),
            None => Vec::new(),
        };

        // The MakerNote the reader tracked, in the IFD it came from. Other
        // MakerNote entries (duplicates elsewhere) are ordinary values.
        let tracked = exif.layout.as_ref().and_then(|l| l.maker_note_ifd);
        let maker_note = tracked
            .or_else(|| {
                // A model built by hand: its MakerNote has no original offset,
                // so it is only located, never anchored.
                [IfdKind::Exif, IfdKind::Primary].into_iter().find(|&kind| {
                    exif.ifd(kind)
                        .is_some_and(|ifd| ifd.get(Tag::MAKER_NOTE).is_some())
                })
            })
            .and_then(|kind| Some((kind, exif.ifd(kind)?.get(Tag::MAKER_NOTE)?)));
        let maker_note = maker_note.filter(|_| options.maker_note != MakerNotePolicy::Drop);
        let maker_note_len = maker_note
            .and_then(|(_, e)| encode(e.value(), order))
            .map_or(0, |(ty, count, _)| {
                u64::from(count).saturating_mul(u64::from(ty.size()))
            });
        let original = exif.maker_note_offset().filter(|_| tracked.is_some());
        let anchor_range =
            original.map(|m| u64::from(m)..u64::from(m).saturating_add(maker_note_len));
        let can_anchor = maker_note.is_some()
            && maker_note_len > 4
            && anchor_range.as_ref().is_some_and(|r| {
                r.start >= Header::LEN as u64
                    && r.end <= u64::from(u32::MAX)
                    && !reserved.iter().any(|res| overlaps(r, res))
            });
        let anchored = match options.maker_note {
            MakerNotePolicy::Anchor => can_anchor,
            MakerNotePolicy::RequireAnchored => {
                if !can_anchor && maker_note.is_some() && original.is_some() {
                    return Err(Error::Unsupported {
                        what: "the MakerNote cannot keep its original offset",
                    });
                }
                can_anchor
            }
            MakerNotePolicy::Move | MakerNotePolicy::Drop => false,
        };
        if anchored && let Some(range) = &anchor_range {
            reserved.push(range.clone());
        }
        reserved.sort_by_key(|r| r.start);

        // OffsetSchema: unchanged when anchored, updated when moved, and
        // dropped when there is no MakerNote it could describe.
        let schema = match (maker_note, original, anchored) {
            _ if maker_note_len <= 4 => None,
            (Some(_), Some(_), true) => exif.offset_schema().map(Schema::Keep),
            (Some(_), Some(m), false) => Some(Schema::Moved {
                base: exif.offset_schema().unwrap_or(0),
                original: m,
            }),
            _ => None,
        };
        if schema.is_some() {
            add(IfdKind::Exif, &mut kinds);
        }

        // Pointer entries: parent -> (tag, children).
        let written: Vec<IfdKind> = kinds
            .iter()
            .filter_map(|r| by_rank.get(r).copied())
            .collect();
        let mut pointers: BTreeMap<(u8, u16), Vec<PointerTag>> = BTreeMap::new();
        for &kind in &written {
            let tag = match kind {
                IfdKind::Exif => Tag::EXIF_IFD_POINTER,
                IfdKind::Gps => Tag::GPS_INFO_IFD_POINTER,
                IfdKind::Interop => Tag::INTEROPERABILITY_IFD_POINTER,
                IfdKind::SubIfd(_) => Tag::SUB_IFDS,
                _ => continue,
            };
            let Some(parent) = parent_of(exif, kind) else {
                continue;
            };
            let list = pointers.entry(rank(parent)).or_default();
            match list.iter_mut().find(|(id, _)| *id == tag.id()) {
                Some((_, children)) => children.push(kind),
                None => list.push((tag.id(), vec![kind])),
            }
        }

        // Build each node's entries.
        let mut nodes = Vec::with_capacity(written.len());
        for &kind in &written {
            let mut entries = Vec::new();
            if let Some(ifd) = exif.ifd(kind) {
                for entry in ifd.entries() {
                    let tag = entry.tag();
                    let role = tag.info().map_or(Role::Value, |i| i.role);
                    let managed_thumbnail = thumbnail.is_some()
                        && kind == IfdKind::Thumbnail
                        && (tag == Tag::JPEG_INTERCHANGE_FORMAT
                            || tag == Tag::JPEG_INTERCHANGE_FORMAT_LENGTH);
                    if matches!(role, Role::Pointer(_) | Role::Computed) || managed_thumbnail {
                        continue;
                    }
                    // A data reference that already pointed outside the source
                    // would point at unrelated bytes in the output: drop it.
                    if let Some(source) = options.source
                        && matches!(role, Role::DataOffset | Role::DataLength)
                        && dangles(ifd, tag, source)
                    {
                        continue;
                    }
                    let is_maker_note =
                        maker_note.is_some_and(|(k, _)| k == kind) && tag == Tag::MAKER_NOTE;
                    if tag == Tag::MAKER_NOTE && options.maker_note == MakerNotePolicy::Drop {
                        continue;
                    }
                    // Unknown types cannot be written: their size is unknown.
                    let Some((ty, count, bytes)) = encode(entry.value(), order) else {
                        continue;
                    };
                    // An unchanged value keeps its source offset, so anything
                    // that points into it (an embedded MakerNote, vendor data)
                    // stays valid.
                    let pin = pinned_at(layout_source, entry.source, &bytes);
                    entries.push(Out {
                        id: tag.id(),
                        ty,
                        count,
                        data: Data::Bytes(bytes),
                        at: 0,
                        anchored: is_maker_note && anchored,
                        pin,
                    });
                }
            }
            for (id, children) in pointers.remove(&rank(kind)).unwrap_or_default() {
                entries.push(Out {
                    id,
                    ty: Type::Long,
                    count: u32::try_from(children.len()).unwrap_or(u32::MAX),
                    data: Data::Tables(children),
                    at: 0,
                    anchored: false,
                    pin: None,
                });
            }
            if kind == IfdKind::Thumbnail
                && let Some(jpeg) = thumbnail
            {
                let len = u32::try_from(jpeg.len()).map_err(|_| Error::TooLarge {
                    size: jpeg.len(),
                    max: u32::MAX as usize,
                })?;
                entries.push(Out {
                    id: Tag::JPEG_INTERCHANGE_FORMAT.id(),
                    ty: Type::Long,
                    count: 1,
                    data: Data::ThumbnailOffset,
                    at: 0,
                    anchored: false,
                    pin: None,
                });
                let mut bytes = Vec::with_capacity(4);
                put32(&mut bytes, order, len);
                entries.push(Out {
                    id: Tag::JPEG_INTERCHANGE_FORMAT_LENGTH.id(),
                    ty: Type::Long,
                    count: 1,
                    data: Data::Bytes(bytes),
                    at: 0,
                    anchored: false,
                    pin: None,
                });
            }
            if kind == IfdKind::Exif
                && let Some(schema) = schema
            {
                let data = match schema {
                    Schema::Moved { base, original } => Data::Schema { base, original },
                    Schema::Keep(value) => {
                        let mut bytes = Vec::with_capacity(4);
                        put32(&mut bytes, order, value.cast_unsigned());
                        Data::Bytes(bytes)
                    }
                };
                entries.push(Out {
                    id: Tag::OFFSET_SCHEMA.id(),
                    ty: Type::SLong,
                    count: 1,
                    data,
                    at: 0,
                    anchored: false,
                    pin: None,
                });
            }
            entries.sort_by_key(|e| e.id);
            nodes.push(Node {
                kind,
                entries,
                at: 0,
            });
        }

        // Pinned values and an unchanged thumbnail keep their offsets.
        for node in &nodes {
            for entry in &node.entries {
                if let Some(at) = entry.pin {
                    reserved.push(at..at.saturating_add(entry.byte_len()));
                }
            }
        }
        let thumbnail_pin = thumbnail.and_then(|jpeg| {
            let at = layout.and_then(|l| l.thumbnail)?;
            pinned_at(layout_source, Some(at), jpeg)
        });
        if let (Some(at), Some(jpeg)) = (thumbnail_pin, thumbnail) {
            reserved.push(at..at.saturating_add(u64::try_from(jpeg.len()).unwrap_or(u64::MAX)));
        }
        reserved.sort_by_key(|r| r.start);

        // Layout.
        let mut allocator = Allocator {
            pos: Header::LEN as u64,
            reserved,
        };
        let mut maker_note_at = None;
        for node in &mut nodes {
            let count = u64::try_from(node.entries.len()).unwrap_or(u64::MAX);
            node.at = allocator.alloc(count.saturating_mul(12).saturating_add(6));
            let holds_maker_note = maker_note.is_some_and(|(k, _)| k == node.kind);
            for (index, entry) in node.entries.iter_mut().enumerate() {
                let len = entry.byte_len();
                if len > 4 {
                    entry.at = match (entry.anchored, &anchor_range, entry.pin) {
                        (true, Some(range), _) => range.start,
                        (_, _, Some(at)) => at,
                        _ => allocator.alloc(len),
                    };
                }
                if holds_maker_note && entry.id == Tag::MAKER_NOTE.id() {
                    // A MakerNote of 4 bytes or less sits in its entry's field.
                    let field = node
                        .at
                        .saturating_add(2)
                        .saturating_add(12u64.saturating_mul(index as u64))
                        .saturating_add(8);
                    maker_note_at = Some(if len > 4 { entry.at } else { field });
                }
            }
        }
        let thumbnail = thumbnail.map(|jpeg| {
            let at = thumbnail_pin
                .unwrap_or_else(|| allocator.alloc(u64::try_from(jpeg.len()).unwrap_or(u64::MAX)));
            (jpeg, at)
        });
        let len = allocator.end();
        if len > u64::from(u32::MAX) {
            return Err(Error::TooLarge {
                size: usize::try_from(len).unwrap_or(usize::MAX),
                max: u32::MAX as usize,
            });
        }
        Ok(Self {
            order,
            nodes,
            thumbnail,
            copies,
            maker_note_at,
            len,
        })
    }

    pub(super) fn len(&self) -> usize {
        usize::try_from(self.len).unwrap_or(usize::MAX)
    }

    pub(super) fn serialize(&self) -> Result<Vec<u8>> {
        let mut out = vec![0u8; self.len()];
        let order = self.order;
        let header = Header::new(
            order,
            self.table(IfdKind::Primary).unwrap_or(Header::LEN as u32),
        );
        put(&mut out, 0, &header.to_bytes())?;
        for &(at, bytes) in &self.copies {
            put(&mut out, at, bytes)?;
        }
        for node in &self.nodes {
            let mut table =
                Vec::with_capacity(node.entries.len().saturating_mul(12).saturating_add(6));
            let count = u16::try_from(node.entries.len()).map_err(|_| Error::TooLarge {
                size: node.entries.len(),
                max: usize::from(u16::MAX),
            })?;
            table.extend_from_slice(&match order {
                ByteOrder::LittleEndian => count.to_le_bytes(),
                ByteOrder::BigEndian => count.to_be_bytes(),
            });
            for entry in &node.entries {
                let bytes = self.bytes(entry)?;
                let id = entry.id;
                let ty = entry.ty.to_u16();
                table.extend_from_slice(&match order {
                    ByteOrder::LittleEndian => [id.to_le_bytes(), ty.to_le_bytes()].concat(),
                    ByteOrder::BigEndian => [id.to_be_bytes(), ty.to_be_bytes()].concat(),
                });
                put32(&mut table, order, entry.count);
                if bytes.len() <= 4 {
                    let mut field = [0u8; 4];
                    field
                        .get_mut(..bytes.len())
                        .ok_or(layout_error())?
                        .copy_from_slice(&bytes);
                    table.extend_from_slice(&field);
                } else {
                    put32(
                        &mut table,
                        order,
                        u32::try_from(entry.at).map_err(|_| layout_error())?,
                    );
                    put(&mut out, entry.at, &bytes)?;
                }
            }
            put32(&mut table, order, self.next_of(node.kind).unwrap_or(0));
            put(&mut out, node.at, &table)?;
        }
        if let Some((jpeg, at)) = self.thumbnail {
            put(&mut out, at, jpeg)?;
        }
        Ok(out)
    }

    fn table(&self, kind: IfdKind) -> Option<u32> {
        let node = self.nodes.iter().find(|n| n.kind == kind)?;
        u32::try_from(node.at).ok()
    }

    /// The next IFD in the IFD0 chain.
    fn next_of(&self, kind: IfdKind) -> Option<u32> {
        let next = match kind {
            IfdKind::Primary => IfdKind::Thumbnail,
            IfdKind::Thumbnail => IfdKind::Chain(2),
            IfdKind::Chain(n) => IfdKind::Chain(n.checked_add(1)?),
            _ => return None,
        };
        self.table(next)
    }

    fn bytes(&self, entry: &Out) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        match &entry.data {
            Data::Bytes(raw) => bytes.clone_from(raw),
            Data::Tables(children) => {
                for child in children {
                    put32(
                        &mut bytes,
                        self.order,
                        self.table(*child).ok_or(layout_error())?,
                    );
                }
            }
            Data::ThumbnailOffset => {
                let at = self.thumbnail.map_or(0, |(_, at)| at);
                put32(
                    &mut bytes,
                    self.order,
                    u32::try_from(at).map_err(|_| layout_error())?,
                );
            }
            Data::Schema { base, original } => {
                let new = self.maker_note_at.ok_or(layout_error())?;
                let moved = i64::try_from(new).unwrap_or(i64::MAX) - i64::from(*original);
                let schema = i64::from(*base).saturating_add(moved);
                let schema = i32::try_from(schema).map_err(|_| Error::TooLarge {
                    size: usize::try_from(new).unwrap_or(usize::MAX),
                    max: i32::MAX as usize,
                })?;
                put32(&mut bytes, self.order, schema.cast_unsigned());
            }
        }
        Ok(bytes)
    }
}

const fn layout_error() -> Error {
    Error::Malformed {
        offset: 0,
        reason: "internal layout error",
    }
}

fn put(out: &mut [u8], at: u64, bytes: &[u8]) -> Result<()> {
    let start = usize::try_from(at).map_err(|_| layout_error())?;
    let end = start.checked_add(bytes.len()).ok_or(layout_error())?;
    out.get_mut(start..end)
        .ok_or(layout_error())?
        .copy_from_slice(bytes);
    Ok(())
}
