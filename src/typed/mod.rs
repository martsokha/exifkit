//! Typed access on top of the raw model: dates and times, GPS positions,
//! encoded text, and [`IntoValue`](set::IntoValue) with [`Exif::set`](crate::Exif::set).
//! Everything public here is re-exported at the crate root.

pub(crate) mod datetime;
pub(crate) mod gps;
pub(crate) mod set;
pub(crate) mod text;
