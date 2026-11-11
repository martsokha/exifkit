//! Error type. See `docs/api.md#errors`.

use core::fmt;

use crate::limits::Limit;

/// A specialized [`Result`](core::result::Result) type for this crate.
pub type Result<T, E = Error> = core::result::Result<T, E>;

/// Errors returned by this crate.
///
/// Variants carry no `String` payloads, so the type is cheap, matchable and
/// usable without `std`.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// An I/O error from the underlying reader or writer.
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    Io(std::io::Error),
    /// The container format was not recognized.
    UnknownFormat,
    /// The input uses a feature this crate does not support.
    Unsupported {
        /// What is unsupported, e.g. `"BigTIFF"`.
        what: &'static str,
    },
    /// The container is valid but holds no Exif data.
    NoExif,
    /// The TIFF header (byte order, magic or IFD0 offset) is invalid.
    InvalidHeader,
    /// The input is structurally broken in a way that cannot be recovered.
    Malformed {
        /// Byte offset of the problem, with the same base as
        /// [`Diagnostic::offset`](crate::Diagnostic::offset).
        offset: u64,
        /// A short description of what is wrong.
        reason: &'static str,
    },
    /// A configured resource limit was exceeded.
    LimitExceeded(Limit),
    /// An entry cannot be stored where it was put.
    InvalidEntry {
        /// The entry's tag.
        tag: crate::tag::Tag,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// A value does not fit its tag: a type, count or enumeration value
    /// the tag table does not allow. See [`Exif::set`](crate::Exif::set).
    InvalidValue {
        /// The tag being set.
        tag: crate::tag::Tag,
        /// What is wrong with the value.
        error: crate::value::ValueError,
    },
    /// The encoded Exif block does not fit in the target container.
    TooLarge {
        /// Size of the encoded block, in bytes.
        size: usize,
        /// Largest size the container allows, in bytes.
        max: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "std")]
            Self::Io(err) => write!(f, "i/o error: {err}"),
            Self::UnknownFormat => f.write_str("unrecognized container format"),
            Self::Unsupported { what } => write!(f, "unsupported: {what}"),
            Self::NoExif => f.write_str("no exif data found"),
            Self::InvalidHeader => f.write_str("invalid tiff header"),
            Self::Malformed { offset, reason } => {
                write!(f, "malformed data at offset {offset}: {reason}")
            }
            Self::LimitExceeded(limit) => write!(f, "limit exceeded: {limit}"),
            Self::InvalidEntry { tag, reason } => write!(f, "invalid entry {tag}: {reason}"),
            Self::InvalidValue { tag, error } => write!(f, "invalid value for {tag}: {error}"),
            Self::TooLarge { size, max } => {
                write!(
                    f,
                    "exif block is {size} bytes, container allows at most {max}"
                )
            }
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            #[cfg(feature = "std")]
            Self::Io(err) => Some(err),
            Self::InvalidValue { error, .. } => Some(error),
            _ => None,
        }
    }
}

#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}
