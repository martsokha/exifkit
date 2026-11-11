//! Typed writing: [`IntoValue`] and [`Exif::set`]. See `docs/api.md#typed-access`.

use alloc::string::String;
use alloc::vec::Vec;

use crate::datetime::{DateTime, Offset, SubSec};
use crate::error::{Error, Result};
use crate::ifd::IfdKind;
use crate::model::{Entry, Exif};
use crate::tag::{StringKind, Tag, TagInfo, Version};
use crate::text::{Copyright, EncodedText, TextEncoding};
use crate::tiff::{ByteOrder, Type};
use crate::value::{Rational, SRational, Value, ValueError};

/// Converts a Rust value into the [`Value`] a tag expects.
///
/// The tag's table entry picks the field type: an integer becomes the
/// tag's preferred integer type (or a RATIONAL `n/1` for rational-only
/// tags), and text becomes ASCII when it is ASCII and the tag allows it,
/// else UTF-8. [`Exif::set`] then checks the result against the table.
pub trait IntoValue {
    /// Converts the value for `tag`.
    ///
    /// # Errors
    ///
    /// Returns a [`ValueError`] if the value cannot be represented in any
    /// type the tag allows.
    fn into_value(self, tag: Tag) -> Result<Value, ValueError>;
}

fn types(tag: Tag) -> &'static [Type] {
    tag.info().map_or(&[], |info| info.types)
}

/// Unsigned integers: the first allowed integer type they all fit in.
fn unsigned(tag: Tag, values: &[u64]) -> Result<Value, ValueError> {
    let fits = |max: u64| values.iter().all(|&v| v <= max);
    let allowed = types(tag);
    // An unknown tag gets the smallest of SHORT and LONG.
    let candidates: &[Type] = if allowed.is_empty() {
        &[Type::Short, Type::Long]
    } else {
        allowed
    };
    let narrow = |v: &u64| u32::try_from(*v).unwrap_or(u32::MAX);
    for &ty in candidates {
        let value = match ty {
            Type::Byte if fits(u8::MAX.into()) => Value::Byte(
                values
                    .iter()
                    .map(|&v| u8::try_from(v).unwrap_or(0))
                    .collect(),
            ),
            // Single-byte UNDEFINED enums (FileSource, SceneType).
            Type::Undefined if fits(u8::MAX.into()) => Value::Undefined(
                values
                    .iter()
                    .map(|&v| u8::try_from(v).unwrap_or(0))
                    .collect(),
            ),
            Type::Short if fits(u16::MAX.into()) => Value::Short(
                values
                    .iter()
                    .map(|&v| u16::try_from(v).unwrap_or(0))
                    .collect(),
            ),
            Type::Long if fits(u32::MAX.into()) => Value::Long(values.iter().map(narrow).collect()),
            Type::Rational if fits(u32::MAX.into()) => {
                Value::Rational(values.iter().map(|v| Rational::new(narrow(v), 1)).collect())
            }
            _ => continue,
        };
        return Ok(value);
    }
    let integer = candidates.iter().any(|t| {
        matches!(
            t,
            Type::Byte | Type::Undefined | Type::Short | Type::Long | Type::Rational
        )
    });
    Err(if integer {
        ValueError::OutOfRange
    } else {
        ValueError::WrongType {
            found: Some(Type::Long),
        }
    })
}

/// Signed integers: the first allowed signed type they all fit in.
fn signed(tag: Tag, values: &[i64]) -> Result<Value, ValueError> {
    let fits = |min: i64, max: i64| values.iter().all(|&v| (min..=max).contains(&v));
    let allowed = types(tag);
    let candidates: &[Type] = if allowed.is_empty() {
        &[Type::SShort, Type::SLong]
    } else {
        allowed
    };
    let narrow = |v: &i64| i32::try_from(*v).unwrap_or(0);
    for &ty in candidates {
        let value = match ty {
            Type::SByte if fits(i8::MIN.into(), i8::MAX.into()) => Value::SByte(
                values
                    .iter()
                    .map(|&v| i8::try_from(v).unwrap_or(0))
                    .collect(),
            ),
            Type::SShort if fits(i16::MIN.into(), i16::MAX.into()) => Value::SShort(
                values
                    .iter()
                    .map(|&v| i16::try_from(v).unwrap_or(0))
                    .collect(),
            ),
            Type::SLong if fits(i32::MIN.into(), i32::MAX.into()) => {
                Value::SLong(values.iter().map(narrow).collect())
            }
            Type::SRational if fits(i32::MIN.into(), i32::MAX.into()) => Value::SRational(
                values
                    .iter()
                    .map(|v| SRational::new(narrow(v), 1))
                    .collect(),
            ),
            _ => continue,
        };
        return Ok(value);
    }
    // Non-negative values may still fit an unsigned tag.
    if values.iter().all(|&v| v >= 0) {
        let unsigned_values: Vec<u64> = values.iter().map(|&v| v.unsigned_abs()).collect();
        return unsigned(tag, &unsigned_values);
    }
    Err(ValueError::WrongType {
        found: Some(Type::SLong),
    })
}

macro_rules! unsigned_impls {
    ($($t:ty),*) => {$(
        impl IntoValue for $t {
            fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
                unsigned(tag, &[u64::from(self)])
            }
        }
        impl IntoValue for &[$t] {
            fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
                unsigned(tag, &self.iter().map(|&v| u64::from(v)).collect::<Vec<_>>())
            }
        }
        impl IntoValue for Vec<$t> {
            fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
                self.as_slice().into_value(tag)
            }
        }
        impl<const N: usize> IntoValue for [$t; N] {
            fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
                self.as_slice().into_value(tag)
            }
        }
    )*};
}

macro_rules! signed_impls {
    ($($t:ty),*) => {$(
        impl IntoValue for $t {
            fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
                signed(tag, &[i64::from(self)])
            }
        }
        impl IntoValue for &[$t] {
            fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
                signed(tag, &self.iter().map(|&v| i64::from(v)).collect::<Vec<_>>())
            }
        }
        impl IntoValue for Vec<$t> {
            fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
                self.as_slice().into_value(tag)
            }
        }
        impl<const N: usize> IntoValue for [$t; N] {
            fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
                self.as_slice().into_value(tag)
            }
        }
    )*};
}

unsigned_impls!(u8, u16, u32);
signed_impls!(i8, i16, i32);

macro_rules! direct_impls {
    ($($t:ty => $variant:ident),*) => {$(
        impl IntoValue for $t {
            fn into_value(self, _: Tag) -> Result<Value, ValueError> {
                Ok(Value::$variant(alloc::vec![self]))
            }
        }
        impl IntoValue for &[$t] {
            fn into_value(self, _: Tag) -> Result<Value, ValueError> {
                Ok(Value::$variant(self.to_vec()))
            }
        }
        impl IntoValue for Vec<$t> {
            fn into_value(self, _: Tag) -> Result<Value, ValueError> {
                Ok(Value::$variant(self))
            }
        }
        impl<const N: usize> IntoValue for [$t; N] {
            fn into_value(self, _: Tag) -> Result<Value, ValueError> {
                Ok(Value::$variant(self.to_vec()))
            }
        }
    )*};
}

// FLOAT and DOUBLE only: a float never becomes a RATIONAL implicitly, since
// that would round. Use `Rational::new` for that.
direct_impls!(Rational => Rational, SRational => SRational, f32 => Float, f64 => Double);

/// Text: ASCII when it is ASCII and the tag allows ASCII, else UTF-8. An
/// UNDEFINED-only tag (such as `ExifVersion`) gets the bytes as they are.
impl IntoValue for &str {
    fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
        let info = tag.info();
        let kind = info.and_then(TagInfo::string_kind);
        if kind.is_none() && info.is_some_and(|i| i.types.contains(&Type::Undefined)) {
            return Ok(Value::Undefined(self.as_bytes().to_vec()));
        }
        // NUL terminates an ASCII value; text cannot contain it.
        if self.contains('\0') {
            return Err(ValueError::Invalid);
        }
        match (kind, self.is_ascii()) {
            (Some(StringKind::Ascii | StringKind::AsciiOrUtf8) | None, true) => {
                if info.is_some() && kind.is_none() {
                    return Err(ValueError::WrongType {
                        found: Some(Type::Ascii),
                    });
                }
                Ok(Value::ascii(self))
            }
            (Some(StringKind::AsciiOrUtf8 | StringKind::Utf8) | None, _) => {
                if info.is_some() && kind.is_none() {
                    return Err(ValueError::WrongType {
                        found: Some(Type::Utf8),
                    });
                }
                Ok(Value::utf8(self))
            }
            // Non-ASCII text for an ASCII-only tag (a serial number, say).
            (Some(StringKind::Ascii), false) => Err(ValueError::WrongType {
                found: Some(Type::Utf8),
            }),
        }
    }
}

impl IntoValue for String {
    fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
        self.as_str().into_value(tag)
    }
}

impl IntoValue for &String {
    fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
        self.as_str().into_value(tag)
    }
}

/// A [`Value`] passes through unchanged; [`Exif::set`] still checks it.
impl IntoValue for Value {
    fn into_value(self, _: Tag) -> Result<Value, ValueError> {
        Ok(self)
    }
}

/// `"YYYY:MM:DD HH:MM:SS"`. The sub-second and offset parts go in their own
/// tags: use [`Exif::set_date_time_original`] and friends to write all three.
impl IntoValue for DateTime {
    fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
        let valid = (1..=12).contains(&self.month)
            && (1..=31).contains(&self.day)
            && self.hour <= 23
            && self.minute <= 59
            && self.second <= 60
            && self.year <= 9999;
        if !valid {
            return Err(ValueError::Invalid);
        }
        let text = alloc::format!(
            "{:04}:{:02}:{:02} {:02}:{:02}:{:02}",
            self.year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second
        );
        text.as_str().into_value(tag)
    }
}

/// `"+HH:MM"`, for the `OffsetTime*` tags.
impl IntoValue for Offset {
    fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
        alloc::format!("{self}").as_str().into_value(tag)
    }
}

/// The digits as recorded, for the `SubSecTime*` tags.
impl IntoValue for SubSec {
    fn into_value(self, tag: Tag) -> Result<Value, ValueError> {
        alloc::format!("{self}").as_str().into_value(tag)
    }
}

/// The 8-byte character-code prefix, then the text.
impl IntoValue for EncodedText {
    fn into_value(self, _: Tag) -> Result<Value, ValueError> {
        let prefix: &[u8; 8] = match self.encoding() {
            TextEncoding::Ascii => b"ASCII\0\0\0",
            TextEncoding::Unicode => b"UNICODE\0",
            TextEncoding::Jis => b"JIS\0\0\0\0\0",
            TextEncoding::Undefined => &[0; 8],
            TextEncoding::Invalid => return Ok(Value::Undefined(self.data().to_vec())),
        };
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(self.data());
        Ok(Value::Undefined(bytes))
    }
}

/// `"photographer\0editor\0"`; an absent photographer is a single space
/// when there is an editor, as the spec writes it.
impl IntoValue for Copyright {
    fn into_value(self, _: Tag) -> Result<Value, ValueError> {
        let photographer = self.photographer.as_deref().unwrap_or("");
        let editor = self.editor.as_deref();
        if photographer.contains('\0') || editor.is_some_and(|e| e.contains('\0')) {
            return Err(ValueError::Invalid);
        }
        if !photographer.is_ascii() || editor.is_some_and(|e| !e.is_ascii()) {
            return Err(ValueError::WrongType {
                found: Some(Type::Utf8),
            });
        }
        let mut bytes = Vec::new();
        match editor {
            Some(editor) => {
                bytes.extend_from_slice(
                    if photographer.is_empty() {
                        " "
                    } else {
                        photographer
                    }
                    .as_bytes(),
                );
                bytes.push(0);
                bytes.extend_from_slice(editor.as_bytes());
            }
            None => bytes.extend_from_slice(photographer.as_bytes()),
        }
        bytes.push(0);
        Ok(Value::Ascii(bytes))
    }
}

impl EncodedText {
    /// Encodes text for `UserComment` and the GPS text tags: with the ASCII
    /// prefix when it is ASCII, else as UTF-16 (`UNICODE`) in `order`, which
    /// should be the [`Exif::byte_order`] it is stored in.
    #[must_use]
    pub fn new(text: &str, order: ByteOrder) -> Self {
        if text.is_ascii() {
            return Self::from_parts(TextEncoding::Ascii, text.as_bytes().to_vec(), order);
        }
        let mut data = Vec::with_capacity(text.len().saturating_mul(2));
        for unit in text.encode_utf16() {
            data.extend_from_slice(&match order {
                ByteOrder::LittleEndian => unit.to_le_bytes(),
                ByteOrder::BigEndian => unit.to_be_bytes(),
            });
        }
        Self::from_parts(TextEncoding::Unicode, data, order)
    }
}

/// Checks a value against the tag table: an allowed type, an allowed count
/// and, for enumerations, a defined value. Unknown tags are not checked.
pub(crate) fn check(tag: Tag, value: &Value) -> Result<(), ValueError> {
    let Some(info) = tag.info() else {
        return Ok(());
    };
    let ty = value.ty();
    if !ty.is_some_and(|ty| info.types.contains(&ty)) {
        return Err(ValueError::WrongType { found: ty });
    }
    let count = u32::try_from(value.count()).unwrap_or(u32::MAX);
    if !info.count.allows(count) {
        return Err(ValueError::CountNotAllowed {
            found: value.count(),
        });
    }
    if !info.values.is_empty() {
        let raw = match value {
            Value::Short(v) => v.first().copied(),
            Value::Byte(v) | Value::Undefined(v) => v.first().copied().map(u16::from),
            _ => None,
        };
        if raw.is_some_and(|raw| info.describe(raw).is_none()) {
            return Err(ValueError::OutOfRange);
        }
    }
    if let Value::Utf8(_) = value
        && value.text().is_none()
    {
        return Err(ValueError::InvalidText);
    }
    Ok(())
}

/// The `ExifVersion` the model has to declare for a value: 3.0 for UTF-8
/// text and for tags introduced in 3.0 or later.
fn needs_v3(tag: Tag, value: &Value) -> bool {
    matches!(value, Value::Utf8(_))
        || tag
            .info()
            .and_then(|info| info.since)
            .is_some_and(|since| since >= Version::new(3, 0))
}

impl Exif {
    /// Sets a tag to a typed value, in the IFD the spec places it in, and
    /// returns the previous value.
    ///
    /// The value is converted with [`IntoValue`] and checked against the
    /// tag table: allowed type, count, and for enumerations a defined
    /// value. Text needing UTF-8 and tags introduced in Exif 3.0 raise
    /// `ExifVersion` to `"0300"` (adding it if missing), since older
    /// readers do not expect them. Unknown tags are stored unchecked. To
    /// store a non-conforming value on purpose, use [`Exif::insert`].
    ///
    /// ```
    /// use exifkit::tag::Orientation;
    /// use exifkit::{Exif, Tag};
    ///
    /// let mut exif = Exif::new();
    /// exif.set(Tag::ARTIST, "Jane Doe")?;
    /// exif.set(Tag::ORIENTATION, Orientation::Rotate180)?;
    /// exif.set(Tag::X_RESOLUTION, 300u32)?; // RATIONAL 300/1
    /// assert!(exif.set(Tag::ORIENTATION, 42u16).is_err()); // not a defined value
    /// # Ok::<(), exifkit::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidValue`] if the value does not fit the tag.
    /// - [`Error::InvalidEntry`] for tags the writer computes (pointers,
    ///   data offsets and lengths).
    pub fn set<T: IntoValue>(&mut self, tag: Tag, value: T) -> Result<Option<Value>> {
        let invalid = |error| Error::InvalidValue { tag, error };
        let value = value.into_value(tag).map_err(invalid)?;
        check(tag, &value).map_err(invalid)?;
        let v3 = tag != Tag::EXIF_VERSION && needs_v3(tag, &value);
        let previous = self.insert(Entry::new(tag, value))?;
        if v3 {
            self.require_version_3();
        }
        Ok(previous.map(Entry::into_value))
    }

    /// Raises `ExifVersion` to `"0300"` if it is lower or missing.
    fn require_version_3(&mut self) {
        let current = self
            .ifd(IfdKind::Exif)
            .and_then(|ifd| ifd.get(Tag::EXIF_VERSION))
            .and_then(|e| match e.value() {
                Value::Undefined(v) => Some(v.clone()),
                _ => None,
            });
        if current.is_none_or(|v| v.as_slice() < b"0300".as_slice()) {
            let _ = self.insert(Entry::new(
                Tag::EXIF_VERSION,
                Value::Undefined(b"0300".to_vec()),
            ));
        }
    }

    fn set_date_time_parts(
        &mut self,
        value: DateTime,
        date_time: Tag,
        subsec: Tag,
        offset: Tag,
    ) -> Result<()> {
        // Convert everything first, so a failure changes nothing.
        let invalid = |tag| move |error| Error::InvalidValue { tag, error };
        let main = value.into_value(date_time).map_err(invalid(date_time))?;
        let subsec_value = value
            .subsec
            .map(|s| s.into_value(subsec).map_err(invalid(subsec)))
            .transpose()?;
        let offset_value = value
            .offset
            .map(|o| o.into_value(offset).map_err(invalid(offset)))
            .transpose()?;
        self.set(date_time, main)?;
        match subsec_value {
            Some(v) => drop(self.set(subsec, v)?),
            None => drop(self.remove(subsec)),
        }
        match offset_value {
            Some(v) => drop(self.set(offset, v)?),
            None => drop(self.remove(offset)),
        }
        Ok(())
    }

    /// Sets `DateTimeOriginal`, `SubSecTimeOriginal` and `OffsetTimeOriginal`
    /// together; a part that is `None` removes its tag, so the three agree.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidValue`] if a field is out of range (month 13, say);
    /// nothing is changed then.
    pub fn set_date_time_original(&mut self, value: DateTime) -> Result<()> {
        self.set_date_time_parts(
            value,
            Tag::DATE_TIME_ORIGINAL,
            Tag::SUB_SEC_TIME_ORIGINAL,
            Tag::OFFSET_TIME_ORIGINAL,
        )
    }

    /// Sets `DateTimeDigitized`, `SubSecTimeDigitized` and
    /// `OffsetTimeDigitized` together, as [`Exif::set_date_time_original`].
    ///
    /// # Errors
    ///
    /// As [`Exif::set_date_time_original`].
    pub fn set_date_time_digitized(&mut self, value: DateTime) -> Result<()> {
        self.set_date_time_parts(
            value,
            Tag::DATE_TIME_DIGITIZED,
            Tag::SUB_SEC_TIME_DIGITIZED,
            Tag::OFFSET_TIME_DIGITIZED,
        )
    }

    /// Sets `DateTime`, `SubSecTime` and `OffsetTime` together, as
    /// [`Exif::set_date_time_original`].
    ///
    /// # Errors
    ///
    /// As [`Exif::set_date_time_original`].
    pub fn set_date_time(&mut self, value: DateTime) -> Result<()> {
        self.set_date_time_parts(value, Tag::DATE_TIME, Tag::SUB_SEC_TIME, Tag::OFFSET_TIME)
    }
}

#[cfg(test)]
mod tests;
