#![no_std]
#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod datetime;
mod diagnostic;
mod error;
mod gps;
mod limits;
mod model;
mod read;
mod text;
mod value;

pub mod container;
pub mod ifd;
pub mod tag;
pub mod tiff;

#[doc(hidden)]
pub mod prelude;

pub use crate::container::Format;
pub use crate::datetime::{DateTime, Offset, SubSec};
pub use crate::diagnostic::{Diagnostic, DiagnosticKind, Diagnostics, Severity};
pub use crate::error::{Error, Result};
pub use crate::gps::{Altitude, AltitudeDatum, Gps};
pub use crate::ifd::IfdKind;
pub use crate::limits::{Limit, Limits};
pub use crate::model::{Entry, Exif, Ifd, ReadOptions};
pub use crate::tag::{Namespace, Tag};
pub use crate::text::{Copyright, EncodedText, TextEncoding};
pub use crate::value::{FromValue, Rational, SRational, Value, ValueError};
