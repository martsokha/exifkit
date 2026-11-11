//! Non-fatal problems found while reading. See `docs/api.md#diagnostics`.

use alloc::vec::Vec;
use core::fmt;

use crate::ifd::IfdKind;
use crate::limits::Limit;
use crate::tag::Tag;
use crate::tiff::Type;

/// How serious a [`Diagnostic`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// The data deviates from the spec, but nothing was lost.
    Warning,
    /// Some data could not be read and is missing from the result.
    /// With [`ReadOptions::strict`](crate::ReadOptions::strict) this fails the read.
    Error,
}

/// What a [`Diagnostic`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DiagnosticKind {
    /// An entry has a field type this crate does not know. It is kept as
    /// [`Value::Unknown`](crate::Value::Unknown).
    UnknownType(u16),
    /// A known tag has a type the spec does not allow. The value is kept as
    /// stored.
    TypeMismatch {
        /// The stored type.
        found: Type,
    },
    /// A known tag has a count the spec does not allow. The value is kept.
    CountMismatch {
        /// The stored count.
        found: u32,
    },
    /// A tag was found in an IFD where the spec does not place it.
    MisplacedTag {
        /// The IFD the spec places it in.
        expected: IfdKind,
    },
    /// An entry's data lies outside the TIFF block. The entry is dropped.
    OutOfBounds,
    /// An entry's `count × size` overflows. The entry is dropped.
    ValueTooLarge,
    /// An IFD's entries are not sorted by tag id.
    UnsortedEntries,
    /// A tag appears twice in one IFD. The first is used; the others are kept
    /// as duplicates.
    DuplicateTag,
    /// A UTF-8 value is not valid UTF-8.
    InvalidUtf8,
    /// An IFD runs past the end of the data. The entries that fit are kept.
    Truncated,
    /// An IFD offset is out of bounds. The IFD is missing.
    InvalidIfdOffset,
    /// An IFD is garbage: its first entry has an invalid type, or it has
    /// too many bad entries. In the first case the IFD is missing; in the
    /// second, the entries after the bad run are.
    CorruptIfd,
    /// An IFD offset points at an IFD that was already read. It is skipped.
    Cycle,
    /// Two pointers name the same kind of IFD (for example two Exif IFDs).
    /// The first wins.
    DuplicateIfd,
    /// A pointer tag has an unusable value. The child IFD is missing.
    InvalidPointer,
    /// The thumbnail data is out of bounds. The thumbnail is missing.
    InvalidThumbnail,
    /// A resource limit was hit. Data beyond it is missing.
    LimitExceeded(Limit),
    /// The file holds more than one Exif block. The first (for PNG, the
    /// `eXIf` chunk) is used; the others are ignored.
    DuplicateExif,
    /// The Exif block has a non-standard header (for example a PNG `eXIf`
    /// chunk starting with JPEG's `Exif\0\0`). It was read anyway.
    NonStandardExifHeader,
    /// A PNG `eXIf` chunk comes after the image data. Valid since PNG 3rd
    /// edition, but some readers ignore it.
    ExifAfterImageData,
    /// A PNG chunk's CRC does not match its contents.
    CrcMismatch,
    /// The container ends before a structure it declares. The Exif data may
    /// be incomplete.
    TruncatedContainer,
}

impl DiagnosticKind {
    /// Returns how serious this kind of problem is.
    #[must_use]
    pub const fn severity(self) -> Severity {
        match self {
            Self::UnknownType(_)
            | Self::TypeMismatch { .. }
            | Self::CountMismatch { .. }
            | Self::MisplacedTag { .. }
            | Self::UnsortedEntries
            | Self::DuplicateTag
            | Self::InvalidUtf8
            | Self::DuplicateExif
            | Self::NonStandardExifHeader
            | Self::ExifAfterImageData
            | Self::CrcMismatch => Severity::Warning,
            Self::OutOfBounds
            | Self::ValueTooLarge
            | Self::Truncated
            | Self::InvalidIfdOffset
            | Self::CorruptIfd
            | Self::Cycle
            | Self::DuplicateIfd
            | Self::InvalidPointer
            | Self::InvalidThumbnail
            | Self::LimitExceeded(_)
            | Self::TruncatedContainer => Severity::Error,
        }
    }

    /// Whether this kind means an entry could not be read properly.
    pub(crate) const fn is_bad_entry(self) -> bool {
        matches!(
            self,
            Self::UnknownType(_) | Self::OutOfBounds | Self::ValueTooLarge | Self::InvalidPointer
        )
    }

    /// A short, static description, used for [`Error::Malformed`](crate::Error::Malformed).
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::UnknownType(_) => "unknown field type",
            Self::TypeMismatch { .. } => "field type not allowed for tag",
            Self::CountMismatch { .. } => "value count not allowed for tag",
            Self::MisplacedTag { .. } => "tag in unexpected IFD",
            Self::OutOfBounds => "value out of bounds",
            Self::ValueTooLarge => "value size overflows",
            Self::UnsortedEntries => "IFD entries not sorted",
            Self::DuplicateTag => "duplicate tag",
            Self::InvalidUtf8 => "invalid UTF-8",
            Self::Truncated => "truncated IFD",
            Self::InvalidIfdOffset => "IFD offset out of bounds",
            Self::CorruptIfd => "corrupt IFD",
            Self::Cycle => "IFD cycle",
            Self::DuplicateIfd => "duplicate IFD pointer",
            Self::InvalidPointer => "invalid IFD pointer",
            Self::InvalidThumbnail => "thumbnail out of bounds",
            Self::LimitExceeded(_) => "resource limit exceeded",
            Self::DuplicateExif => "more than one Exif block",
            Self::NonStandardExifHeader => "non-standard Exif header",
            Self::ExifAfterImageData => "Exif after image data",
            Self::CrcMismatch => "chunk CRC mismatch",
            Self::TruncatedContainer => "truncated file",
        }
    }
}

/// A non-fatal problem found while reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Diagnostic {
    /// What went wrong.
    pub kind: DiagnosticKind,
    /// The IFD it happened in, if any.
    pub ifd: Option<IfdKind>,
    /// The tag it concerns, if any.
    pub tag: Option<Tag>,
    /// The byte offset involved, if any. From [`Exif::read`](crate::Exif::read)
    /// and friends it is relative to the start of the file, except inside a
    /// decoded legacy PNG text chunk, where it is relative to the decoded TIFF
    /// block. From [`Exif::from_tiff`](crate::Exif::from_tiff) it is relative
    /// to the TIFF block.
    pub offset: Option<u64>,
}

impl Diagnostic {
    /// Creates a diagnostic.
    #[must_use]
    pub const fn new(kind: DiagnosticKind) -> Self {
        Self {
            kind,
            ifd: None,
            tag: None,
            offset: None,
        }
    }

    /// Sets the IFD.
    #[must_use]
    pub const fn in_ifd(mut self, ifd: IfdKind) -> Self {
        self.ifd = Some(ifd);
        self
    }

    /// Sets the tag.
    #[must_use]
    pub const fn for_tag(mut self, tag: Tag) -> Self {
        self.tag = Some(tag);
        self
    }

    /// Sets the offset.
    #[must_use]
    pub fn at(mut self, offset: impl Into<u64>) -> Self {
        self.offset = Some(offset.into());
        self
    }

    /// Returns how serious this diagnostic is.
    #[must_use]
    pub const fn severity(&self) -> Severity {
        self.kind.severity()
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind.reason())?;
        match self.kind {
            DiagnosticKind::UnknownType(id) => write!(f, " ({id})")?,
            DiagnosticKind::TypeMismatch { found } => write!(f, " ({found})")?,
            DiagnosticKind::CountMismatch { found } => write!(f, " ({found})")?,
            DiagnosticKind::MisplacedTag { expected } => write!(f, " (expected {expected:?})")?,
            DiagnosticKind::LimitExceeded(limit) => write!(f, " ({limit})")?,
            _ => {}
        }
        if let Some(tag) = self.tag {
            write!(f, ", tag {tag}")?;
        }
        if let Some(ifd) = self.ifd {
            write!(f, ", in {ifd:?}")?;
        }
        if let Some(offset) = self.offset {
            write!(f, ", at offset {offset}")?;
        }
        Ok(())
    }
}

/// The diagnostics collected while reading.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics(Vec<Diagnostic>);

impl Diagnostics {
    pub(crate) fn push(&mut self, diagnostic: Diagnostic) {
        self.0.push(diagnostic);
    }

    pub(crate) fn extend(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    /// Moves every offset by `base`, turning TIFF-block offsets into file
    /// offsets.
    pub(crate) fn rebase(&mut self, base: u64) {
        for offset in self.0.iter_mut().filter_map(|d| d.offset.as_mut()) {
            *offset = offset.saturating_add(base);
        }
    }

    /// The diagnostics pushed after the first `start`.
    pub(crate) fn since(&self, start: usize) -> &[Diagnostic] {
        self.0.get(start..).unwrap_or_default()
    }

    /// Returns the number of diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether there are no diagnostics.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the first diagnostic of [`Severity::Error`], if any.
    #[must_use]
    pub fn first_error(&self) -> Option<&Diagnostic> {
        self.0.iter().find(|d| d.severity() == Severity::Error)
    }

    /// Iterates over the diagnostics, in the order they were found.
    pub fn iter(&self) -> core::slice::Iter<'_, Diagnostic> {
        self.0.iter()
    }

    /// Returns whether any diagnostic has the given kind.
    #[must_use]
    pub fn contains(&self, kind: DiagnosticKind) -> bool {
        self.0.iter().any(|d| d.kind == kind)
    }
}

impl<'a> IntoIterator for &'a Diagnostics {
    type IntoIter = core::slice::Iter<'a, Diagnostic>;
    type Item = &'a Diagnostic;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl IntoIterator for Diagnostics {
    type IntoIter = alloc::vec::IntoIter<Diagnostic>;
    type Item = Diagnostic;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
