#![no_std]
#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod diagnostic;
mod error;
mod limits;
mod model;
mod read;
mod typed;
mod value;
mod write;

pub mod container;
pub mod ifd;
pub mod tag;
pub mod tiff;

#[doc(hidden)]
pub mod prelude;

pub use crate::container::Format;
pub use crate::diagnostic::{Diagnostic, DiagnosticKind, Diagnostics, Severity};
pub use crate::error::{Error, Result};
pub use crate::ifd::IfdKind;
pub use crate::limits::{Limit, Limits};
pub use crate::model::{Entry, Exif, Ifd, ReadOptions};
pub use crate::tag::{Namespace, Tag};
pub use crate::typed::datetime::{DateTime, Offset, SubSec};
pub use crate::typed::gps::{Altitude, AltitudeDatum, Gps};
pub use crate::typed::set::IntoValue;
pub use crate::typed::text::{Copyright, EncodedText, TextEncoding};
pub use crate::value::{FromValue, Rational, SRational, Value, ValueError};
pub use crate::write::{MakerNotePolicy, WriteOptions, remove_exif};
