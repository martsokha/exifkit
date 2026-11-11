//! Encoding values: the inverse of `Value::decode`.

use alloc::vec::Vec;

use crate::tiff::{ByteOrder, Type};
use crate::value::Value;

fn put16(out: &mut Vec<u8>, order: ByteOrder, v: u16) {
    out.extend_from_slice(&match order {
        ByteOrder::LittleEndian => v.to_le_bytes(),
        ByteOrder::BigEndian => v.to_be_bytes(),
    });
}

pub(super) fn put32(out: &mut Vec<u8>, order: ByteOrder, v: u32) {
    out.extend_from_slice(&match order {
        ByteOrder::LittleEndian => v.to_le_bytes(),
        ByteOrder::BigEndian => v.to_be_bytes(),
    });
}

fn put64(out: &mut Vec<u8>, order: ByteOrder, v: u64) {
    out.extend_from_slice(&match order {
        ByteOrder::LittleEndian => v.to_le_bytes(),
        ByteOrder::BigEndian => v.to_be_bytes(),
    });
}

/// The field type, count and encoded bytes of a value, or `None` for
/// [`Value::Unknown`], which cannot be written.
pub(super) fn encode(value: &Value, order: ByteOrder) -> Option<(Type, u32, Vec<u8>)> {
    let ty = value.ty()?;
    let count = u32::try_from(value.count()).ok()?;
    let mut out = Vec::new();
    match value {
        Value::Byte(v) | Value::Ascii(v) | Value::Undefined(v) | Value::Utf8(v) => {
            out.extend_from_slice(v);
        }
        Value::SByte(v) => out.extend(v.iter().map(|b| b.cast_unsigned())),
        Value::Short(v) => v.iter().for_each(|&x| put16(&mut out, order, x)),
        Value::SShort(v) => v
            .iter()
            .for_each(|&x| put16(&mut out, order, x.cast_unsigned())),
        Value::Long(v) | Value::Ifd(v) => v.iter().for_each(|&x| put32(&mut out, order, x)),
        Value::SLong(v) => v
            .iter()
            .for_each(|&x| put32(&mut out, order, x.cast_unsigned())),
        Value::Rational(v) => v.iter().for_each(|r| {
            put32(&mut out, order, r.num);
            put32(&mut out, order, r.den);
        }),
        Value::SRational(v) => v.iter().for_each(|r| {
            put32(&mut out, order, r.num.cast_unsigned());
            put32(&mut out, order, r.den.cast_unsigned());
        }),
        Value::Float(v) => v.iter().for_each(|&x| put32(&mut out, order, x.to_bits())),
        Value::Double(v) => v.iter().for_each(|&x| put64(&mut out, order, x.to_bits())),
        Value::Unknown { .. } => return None,
    }
    Some((ty, count, out))
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::value::{Rational, SRational};

    #[test]
    fn encode_is_the_inverse_of_decode() {
        let values = [
            Value::Byte(vec![1, 2]),
            Value::ascii("Canon"),
            Value::Short(vec![1, 0xFFFF]),
            Value::Long(vec![70_000]),
            Value::Rational(vec![Rational::new(1, 250)]),
            Value::SByte(vec![-1]),
            Value::Undefined(b"0300".to_vec()),
            Value::SShort(vec![-2]),
            Value::SLong(vec![-3]),
            Value::SRational(vec![SRational::new(-1, 3)]),
            Value::Float(vec![1.5, f32::NAN]),
            Value::Double(vec![-0.0]),
            Value::Ifd(vec![8]),
            Value::utf8("Ünï"),
        ];
        for order in [ByteOrder::LittleEndian, ByteOrder::BigEndian] {
            for value in &values {
                let (ty, count, bytes) = encode(value, order).unwrap();
                assert_eq!(usize::try_from(count).unwrap(), value.count());
                assert_eq!(bytes.len(), value.count() * usize::from(ty.size()));
                assert_eq!(&Value::decode(ty, &bytes, order), value);
            }
        }
        let unknown = Value::Unknown {
            type_id: 99,
            count: 1,
            field: [0; 4],
        };
        assert!(encode(&unknown, ByteOrder::LittleEndian).is_none());
    }
}
