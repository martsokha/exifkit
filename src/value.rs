//! Tag values. See `docs/api.md#data-model`.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::tiff::{ByteOrder, Type};

/// An unsigned rational: numerator over denominator, stored exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    /// Numerator.
    pub num: u32,
    /// Denominator.
    pub den: u32,
}

impl Rational {
    /// Creates a rational. It is not normalized.
    #[must_use]
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    /// Returns the value as `f64`, or `None` if the denominator is zero.
    #[must_use]
    pub fn to_f64(self) -> Option<f64> {
        (self.den != 0).then(|| f64::from(self.num) / f64::from(self.den))
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

/// A signed rational: numerator over denominator, stored exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SRational {
    /// Numerator.
    pub num: i32,
    /// Denominator.
    pub den: i32,
}

impl SRational {
    /// Creates a rational. It is not normalized.
    #[must_use]
    pub const fn new(num: i32, den: i32) -> Self {
        Self { num, den }
    }

    /// Returns the value as `f64`, or `None` if the denominator is zero.
    #[must_use]
    pub fn to_f64(self) -> Option<f64> {
        (self.den != 0).then(|| f64::from(self.num) / f64::from(self.den))
    }
}

impl fmt::Display for SRational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

/// The value of an IFD entry, exactly as stored.
///
/// Text values (`Ascii`, `Utf8`) hold the stored bytes including any NUL
/// terminator, so they round-trip byte for byte. Use [`Value::text`] for the
/// string itself.
///
/// Equality means "the same stored value": floats compare bit for bit, so a
/// NaN equals itself and `0.0` differs from `-0.0`. That keeps `==` an
/// equivalence ([`Eq`]), which round-trip checks rely on.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Value {
    /// BYTE values.
    Byte(Vec<u8>),
    /// ASCII text, as stored.
    Ascii(Vec<u8>),
    /// SHORT values.
    Short(Vec<u16>),
    /// LONG values.
    Long(Vec<u32>),
    /// RATIONAL values.
    Rational(Vec<Rational>),
    /// SBYTE values.
    SByte(Vec<i8>),
    /// UNDEFINED bytes, interpreted by the tag.
    Undefined(Vec<u8>),
    /// SSHORT values.
    SShort(Vec<i16>),
    /// SLONG values.
    SLong(Vec<i32>),
    /// SRATIONAL values.
    SRational(Vec<SRational>),
    /// FLOAT values.
    Float(Vec<f32>),
    /// DOUBLE values.
    Double(Vec<f64>),
    /// IFD offsets (type 13) on a tag that is not a known pointer.
    Ifd(Vec<u32>),
    /// UTF-8 text (Exif 3.0), as stored.
    Utf8(Vec<u8>),
    /// An entry of a type this crate does not know. Its size, and so its
    /// data, cannot be determined; the raw 4-byte value field is kept.
    Unknown {
        /// Field-type id.
        type_id: u16,
        /// Number of values.
        count: u32,
        /// The entry's 4-byte value-or-offset field.
        field: [u8; 4],
    },
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        fn bits32(v: &[f32]) -> impl Iterator<Item = u32> + '_ {
            v.iter().map(|f| f.to_bits())
        }
        fn bits64(v: &[f64]) -> impl Iterator<Item = u64> + '_ {
            v.iter().map(|f| f.to_bits())
        }
        match (self, other) {
            (Self::Byte(a), Self::Byte(b))
            | (Self::Ascii(a), Self::Ascii(b))
            | (Self::Undefined(a), Self::Undefined(b))
            | (Self::Utf8(a), Self::Utf8(b)) => a == b,
            (Self::Short(a), Self::Short(b)) => a == b,
            (Self::Long(a), Self::Long(b)) | (Self::Ifd(a), Self::Ifd(b)) => a == b,
            (Self::Rational(a), Self::Rational(b)) => a == b,
            (Self::SByte(a), Self::SByte(b)) => a == b,
            (Self::SShort(a), Self::SShort(b)) => a == b,
            (Self::SLong(a), Self::SLong(b)) => a == b,
            (Self::SRational(a), Self::SRational(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => bits32(a).eq(bits32(b)),
            (Self::Double(a), Self::Double(b)) => bits64(a).eq(bits64(b)),
            (
                Self::Unknown {
                    type_id: t1,
                    count: c1,
                    field: f1,
                },
                Self::Unknown {
                    type_id: t2,
                    count: c2,
                    field: f2,
                },
            ) => t1 == t2 && c1 == c2 && f1 == f2,
            _ => false,
        }
    }
}

impl Eq for Value {}

impl Value {
    /// Creates an ASCII value from text, adding the NUL terminator.
    #[must_use]
    pub fn ascii(text: &str) -> Self {
        let mut bytes = Vec::with_capacity(text.len().saturating_add(1));
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(0);
        Self::Ascii(bytes)
    }

    /// Creates a UTF-8 value from text, adding the NUL terminator.
    #[must_use]
    pub fn utf8(text: &str) -> Self {
        let mut bytes = Vec::with_capacity(text.len().saturating_add(1));
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(0);
        Self::Utf8(bytes)
    }

    /// Returns the field type, or `None` for [`Value::Unknown`].
    #[must_use]
    pub fn ty(&self) -> Option<Type> {
        Some(match self {
            Self::Byte(_) => Type::Byte,
            Self::Ascii(_) => Type::Ascii,
            Self::Short(_) => Type::Short,
            Self::Long(_) => Type::Long,
            Self::Rational(_) => Type::Rational,
            Self::SByte(_) => Type::SByte,
            Self::Undefined(_) => Type::Undefined,
            Self::SShort(_) => Type::SShort,
            Self::SLong(_) => Type::SLong,
            Self::SRational(_) => Type::SRational,
            Self::Float(_) => Type::Float,
            Self::Double(_) => Type::Double,
            Self::Ifd(_) => Type::Ifd,
            Self::Utf8(_) => Type::Utf8,
            Self::Unknown { .. } => return None,
        })
    }

    /// Returns the number of values, as written in the entry's count field.
    #[must_use]
    pub fn count(&self) -> usize {
        match self {
            Self::Byte(v) | Self::Ascii(v) | Self::Undefined(v) | Self::Utf8(v) => v.len(),
            Self::Short(v) => v.len(),
            Self::Long(v) | Self::Ifd(v) => v.len(),
            Self::Rational(v) => v.len(),
            Self::SByte(v) => v.len(),
            Self::SShort(v) => v.len(),
            Self::SLong(v) => v.len(),
            Self::SRational(v) => v.len(),
            Self::Float(v) => v.len(),
            Self::Double(v) => v.len(),
            Self::Unknown { count, .. } => usize::try_from(*count).unwrap_or(usize::MAX),
        }
    }

    /// For text values, returns the bytes before the first NUL.
    #[must_use]
    pub fn text_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Ascii(bytes) | Self::Utf8(bytes) => {
                let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                bytes.get(..end)
            }
            _ => None,
        }
    }

    /// For text values, returns the text before the first NUL, if it is
    /// valid UTF-8 (which all ASCII is).
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.text_bytes()
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
    }

    /// Decodes `data` as `count` values of type `ty`. `data` must be exactly
    /// `ty.size() * count` bytes; trailing partial values are ignored.
    pub(crate) fn decode(ty: Type, data: &[u8], order: ByteOrder) -> Self {
        fn chunks<const N: usize>(data: &[u8]) -> &[[u8; N]] {
            data.as_chunks::<N>().0
        }
        match ty {
            Type::Byte => Self::Byte(data.to_vec()),
            Type::Ascii => Self::Ascii(data.to_vec()),
            Type::Undefined => Self::Undefined(data.to_vec()),
            Type::Utf8 => Self::Utf8(data.to_vec()),
            Type::SByte => Self::SByte(data.iter().map(|&b| b.cast_signed()).collect()),
            Type::Short => Self::Short(chunks(data).iter().map(|&c| order.u16(c)).collect()),
            Type::SShort => Self::SShort(
                chunks(data)
                    .iter()
                    .map(|&c| order.u16(c).cast_signed())
                    .collect(),
            ),
            Type::Long => Self::Long(chunks(data).iter().map(|&c| order.u32(c)).collect()),
            Type::Ifd => Self::Ifd(chunks(data).iter().map(|&c| order.u32(c)).collect()),
            Type::SLong => Self::SLong(
                chunks(data)
                    .iter()
                    .map(|&c| order.u32(c).cast_signed())
                    .collect(),
            ),
            Type::Float => Self::Float(
                chunks(data)
                    .iter()
                    .map(|&c| f32::from_bits(order.u32(c)))
                    .collect(),
            ),
            Type::Double => Self::Double(
                chunks(data)
                    .iter()
                    .map(|&c| f64::from_bits(order.u64(c)))
                    .collect(),
            ),
            Type::Rational => Self::Rational(
                chunks::<8>(data)
                    .iter()
                    .map(|&[a, b, c, d, e, f, g, h]| {
                        Rational::new(order.u32([a, b, c, d]), order.u32([e, f, g, h]))
                    })
                    .collect(),
            ),
            Type::SRational => Self::SRational(
                chunks::<8>(data)
                    .iter()
                    .map(|&[a, b, c, d, e, f, g, h]| {
                        SRational::new(
                            order.u32([a, b, c, d]).cast_signed(),
                            order.u32([e, f, g, h]).cast_signed(),
                        )
                    })
                    .collect(),
            ),
        }
    }
}

/// How many values [`Value`]'s `Display` prints before eliding the rest.
const DISPLAY_LIMIT: usize = 16;

fn write_list<T: fmt::Display>(f: &mut fmt::Formatter<'_>, items: &[T]) -> fmt::Result {
    for (i, item) in items.iter().take(DISPLAY_LIMIT).enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{item}")?;
    }
    if items.len() > DISPLAY_LIMIT {
        write!(f, ", … ({} values)", items.len())?;
    }
    Ok(())
}

fn write_bytes(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    let printable = !bytes.is_empty() && bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ');
    if printable && bytes.len() <= 64 {
        // Covers version tags like ExifVersion ("0300").
        return f.write_str(&String::from_utf8_lossy(bytes));
    }
    for (i, b) in bytes.iter().take(DISPLAY_LIMIT).enumerate() {
        if i > 0 {
            f.write_str(" ")?;
        }
        write!(f, "{b:02X}")?;
    }
    if bytes.len() > DISPLAY_LIMIT {
        write!(f, " … ({} bytes)", bytes.len())?;
    }
    Ok(())
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ascii(_) | Self::Utf8(_) => {
                let bytes = self.text_bytes().unwrap_or_default();
                f.write_str(&String::from_utf8_lossy(bytes))
            }
            Self::Undefined(bytes) => write_bytes(f, bytes),
            Self::Byte(v) => write_list(f, v),
            Self::Short(v) => write_list(f, v),
            Self::Long(v) | Self::Ifd(v) => write_list(f, v),
            Self::Rational(v) => write_list(f, v),
            Self::SByte(v) => write_list(f, v),
            Self::SShort(v) => write_list(f, v),
            Self::SLong(v) => write_list(f, v),
            Self::SRational(v) => write_list(f, v),
            Self::Float(v) => write_list(f, v),
            Self::Double(v) => write_list(f, v),
            Self::Unknown { type_id, count, .. } => {
                write!(f, "(unknown type {type_id}, {count} values)")
            }
        }
    }
}

/// Why a [`Value`] could not be converted to the requested Rust type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValueError {
    /// The value's type cannot be converted to the requested type.
    WrongType {
        /// The stored type, or `None` if it is unknown.
        found: Option<Type>,
    },
    /// The requested type needs exactly one value.
    WrongCount {
        /// The number of stored values.
        found: usize,
    },
    /// The value does not fit in the requested type, or is not one the
    /// tag defines.
    OutOfRange,
    /// The tag does not allow this number of values.
    CountNotAllowed {
        /// The number of values given.
        found: usize,
    },
    /// The text is not valid UTF-8.
    InvalidText,
    /// The value does not follow the format the tag defines (for example a
    /// malformed date, or a rational with a zero denominator).
    Invalid,
}

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongType { found: Some(ty) } => write!(f, "unexpected type {ty}"),
            Self::WrongType { found: None } => f.write_str("unknown type"),
            Self::WrongCount { found } => write!(f, "expected 1 value, found {found}"),
            Self::CountNotAllowed { found } => write!(f, "the tag does not allow {found} values"),
            Self::OutOfRange => f.write_str("value out of range"),
            Self::InvalidText => f.write_str("text is not valid UTF-8"),
            Self::Invalid => f.write_str("value does not follow the tag's format"),
        }
    }
}

impl core::error::Error for ValueError {}

/// Lossless conversion from a [`Value`] to a Rust type.
///
/// Conversions never lose information: a LONG converts to `u16` only if it
/// fits, and a RATIONAL never converts to a float implicitly.
pub trait FromValue: Sized {
    /// Converts the value.
    ///
    /// # Errors
    ///
    /// Returns a [`ValueError`] if the type, count or range does not fit.
    fn from_value(value: &Value) -> Result<Self, ValueError>;
}

fn wrong_type(value: &Value) -> ValueError {
    ValueError::WrongType { found: value.ty() }
}

fn single<T: Copy>(values: &[T]) -> Result<T, ValueError> {
    match values {
        [v] => Ok(*v),
        _ => Err(ValueError::WrongCount {
            found: values.len(),
        }),
    }
}

/// All unsigned integer values widened to `u32`.
fn unsigned(value: &Value) -> Result<Vec<u32>, ValueError> {
    Ok(match value {
        Value::Byte(v) | Value::Undefined(v) => v.iter().copied().map(u32::from).collect(),
        Value::Short(v) => v.iter().copied().map(u32::from).collect(),
        Value::Long(v) | Value::Ifd(v) => v.clone(),
        _ => return Err(wrong_type(value)),
    })
}

/// All signed integer values widened to `i32`.
fn signed(value: &Value) -> Result<Vec<i32>, ValueError> {
    Ok(match value {
        Value::SByte(v) => v.iter().copied().map(i32::from).collect(),
        Value::SShort(v) => v.iter().copied().map(i32::from).collect(),
        Value::SLong(v) => v.clone(),
        _ => return Err(wrong_type(value)),
    })
}

impl FromValue for u32 {
    fn from_value(value: &Value) -> Result<Self, ValueError> {
        single(&unsigned(value)?)
    }
}

impl FromValue for u16 {
    fn from_value(value: &Value) -> Result<Self, ValueError> {
        Self::try_from(u32::from_value(value)?).map_err(|_| ValueError::OutOfRange)
    }
}

impl FromValue for u8 {
    fn from_value(value: &Value) -> Result<Self, ValueError> {
        Self::try_from(u32::from_value(value)?).map_err(|_| ValueError::OutOfRange)
    }
}

impl FromValue for i32 {
    fn from_value(value: &Value) -> Result<Self, ValueError> {
        single(&signed(value)?)
    }
}

impl FromValue for Vec<u32> {
    fn from_value(value: &Value) -> Result<Self, ValueError> {
        unsigned(value)
    }
}

impl FromValue for Vec<u16> {
    fn from_value(value: &Value) -> Result<Self, ValueError> {
        unsigned(value)?
            .into_iter()
            .map(|v| u16::try_from(v).map_err(|_| ValueError::OutOfRange))
            .collect()
    }
}

impl FromValue for Rational {
    fn from_value(value: &Value) -> Result<Self, ValueError> {
        match value {
            Value::Rational(v) => single(v),
            _ => Err(wrong_type(value)),
        }
    }
}

impl FromValue for Vec<Rational> {
    fn from_value(value: &Value) -> Result<Self, ValueError> {
        match value {
            Value::Rational(v) => Ok(v.clone()),
            _ => Err(wrong_type(value)),
        }
    }
}

impl FromValue for SRational {
    fn from_value(value: &Value) -> Result<Self, ValueError> {
        match value {
            Value::SRational(v) => single(v),
            _ => Err(wrong_type(value)),
        }
    }
}

impl FromValue for String {
    fn from_value(value: &Value) -> Result<Self, ValueError> {
        let bytes = value.text_bytes().ok_or_else(|| wrong_type(value))?;
        core::str::from_utf8(bytes)
            .map(Into::into)
            .map_err(|_| ValueError::InvalidText)
    }
}

impl FromValue for Value {
    fn from_value(value: &Value) -> Result<Self, ValueError> {
        Ok(value.clone())
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec;

    use super::*;

    #[test]
    fn decodes_both_byte_orders() {
        let le = Value::decode(Type::Short, &[1, 0, 0, 1], ByteOrder::LittleEndian);
        let be = Value::decode(Type::Short, &[0, 1, 1, 0], ByteOrder::BigEndian);
        assert_eq!(le, Value::Short(vec![1, 256]));
        assert_eq!(le, be);
        let r = Value::decode(
            Type::SRational,
            &[0xFF, 0xFF, 0xFF, 0xFF, 3, 0, 0, 0],
            ByteOrder::LittleEndian,
        );
        assert_eq!(r, Value::SRational(vec![SRational::new(-1, 3)]));
    }

    #[test]
    fn conversions_are_lossless() {
        assert_eq!(u32::from_value(&Value::Short(vec![7])), Ok(7));
        assert_eq!(
            u16::from_value(&Value::Long(vec![70_000])),
            Err(ValueError::OutOfRange)
        );
        assert_eq!(
            u16::from_value(&Value::Short(vec![1, 2])),
            Err(ValueError::WrongCount { found: 2 })
        );
        assert_eq!(
            u32::from_value(&Value::Rational(vec![Rational::new(1, 2)])),
            Err(ValueError::WrongType {
                found: Some(Type::Rational)
            })
        );
    }

    #[test]
    fn text_stops_at_nul() {
        let v = Value::Ascii(b"Canon\0\0\0".to_vec());
        assert_eq!(v.text(), Some("Canon"));
        assert_eq!(v.to_string(), "Canon");
        assert_eq!(Value::ascii("x"), Value::Ascii(b"x\0".to_vec()));
        assert_eq!(Value::Ascii(b"\xFF\0".to_vec()).text(), None);
    }

    #[test]
    fn equality_is_bitwise_for_floats() {
        // Found by fuzzing: a NaN made a model unequal to itself.
        let nan = Value::Float(vec![f32::NAN]);
        assert_eq!(nan, nan.clone());
        assert_eq!(Value::Double(vec![f64::NAN]), Value::Double(vec![f64::NAN]));
        assert_ne!(Value::Float(vec![0.0]), Value::Float(vec![-0.0]));
        assert_ne!(Value::Long(vec![1]), Value::Ifd(vec![1]));
    }

    #[test]
    fn display() {
        assert_eq!(Value::Undefined(b"0300".to_vec()).to_string(), "0300");
        assert_eq!(Value::Undefined(vec![1, 2, 3]).to_string(), "01 02 03");
        assert_eq!(
            Value::Rational(vec![Rational::new(1, 250)]).to_string(),
            "1/250"
        );
        assert_eq!(Value::Short(vec![1, 2]).to_string(), "1, 2");
    }
}
