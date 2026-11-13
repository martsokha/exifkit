//! Writing an [`Exif`] back into a TIFF block. See `docs/writing.md`.

mod file;
mod plan;
#[cfg(test)]
mod tests;
mod value;

use alloc::vec::Vec;

pub use self::file::remove_exif;
use crate::error::Result;
use crate::model::Exif;
use crate::tiff::ByteOrder;

/// What to do with the MakerNote, whose internal offsets often assume it
/// stays where the camera wrote it. See `docs/writing.md#makernote`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum MakerNotePolicy {
    /// Keep the MakerNote at its original offset. If that is impossible, move
    /// it and record the move in `OffsetSchema`.
    #[default]
    Anchor,
    /// Keep the MakerNote at its original offset, or fail.
    RequireAnchored,
    /// Pack the MakerNote with everything else and record the move in
    /// `OffsetSchema`. Gives the smallest output.
    Move,
    /// Leave the MakerNote out.
    Drop,
}

/// Options for writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct WriteOptions<'a> {
    /// Byte order of the output. `None` keeps [`Exif::byte_order`].
    pub byte_order: Option<ByteOrder>,
    /// What to do with the MakerNote.
    pub maker_note: MakerNotePolicy,
    /// The TIFF block the model was read from. With it, image data the model
    /// only references by offset (strips, tiles, an unextracted thumbnail) is
    /// kept at its original offsets and copied over. Without it, such
    /// references cannot be written.
    pub source: Option<&'a [u8]>,
    /// Caps for decoding the file's own Exif block, which
    /// [`Exif::write_into`] and [`Exif::write_stream`] use as the source:
    /// [`Limits::max_metadata`](crate::Limits::max_metadata) bounds how much
    /// of it is read and decoded.
    pub limits: crate::limits::Limits,
}

impl<'a> WriteOptions<'a> {
    /// The default options: keep the byte order, anchor the MakerNote, no source.
    pub const DEFAULT: Self = Self {
        byte_order: None,
        maker_note: MakerNotePolicy::Anchor,
        source: None,
        limits: crate::limits::Limits::DEFAULT,
    };

    /// Sets [`WriteOptions::byte_order`].
    #[must_use]
    pub const fn byte_order(mut self, order: ByteOrder) -> Self {
        self.byte_order = Some(order);
        self
    }

    /// Sets [`WriteOptions::maker_note`].
    #[must_use]
    pub const fn maker_note(mut self, policy: MakerNotePolicy) -> Self {
        self.maker_note = policy;
        self
    }

    /// Sets [`WriteOptions::limits`].
    #[must_use]
    pub const fn limits(mut self, limits: crate::limits::Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Sets [`WriteOptions::source`].
    #[must_use]
    pub const fn source(mut self, source: &'a [u8]) -> Self {
        self.source = Some(source);
        self
    }
}

impl Default for WriteOptions<'_> {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Exif {
    /// Writes the metadata as a TIFF block (what goes after `Exif\0\0` in a
    /// JPEG), with the default [`WriteOptions`].
    ///
    /// Reading the result back gives an equal model, except for what cannot
    /// be written: entries of unknown type (their size is unknown) and
    /// duplicate tags (a TIFF IFD holds each tag once).
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`](crate::Error::Unsupported) if the model
    ///   references image data by offset and no source is given.
    /// - [`Error::TooLarge`](crate::Error::TooLarge) above 4 GiB.
    /// - With [`MakerNotePolicy::RequireAnchored`], if the MakerNote cannot
    ///   stay where it was.
    pub fn to_tiff(&self) -> Result<Vec<u8>> {
        self.to_tiff_with(&WriteOptions::DEFAULT)
    }

    /// Writes the metadata as a TIFF block.
    ///
    /// # Errors
    ///
    /// As [`Exif::to_tiff`], plus
    /// [`Error::Malformed`](crate::Error::Malformed) if offsets the model
    /// keeps point outside `options.source`.
    pub fn to_tiff_with(&self, options: &WriteOptions<'_>) -> Result<Vec<u8>> {
        plan::Plan::new(self, options)?.serialize()
    }

    /// The exact size [`Exif::to_tiff_with`] would produce, without writing.
    ///
    /// # Errors
    ///
    /// As [`Exif::to_tiff_with`].
    pub fn encoded_len(&self, options: &WriteOptions<'_>) -> Result<usize> {
        Ok(plan::Plan::new(self, options)?.len())
    }
}
