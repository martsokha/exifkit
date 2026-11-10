//! Encoded text tags (`UserComment`, `GPSProcessingMethod`,
//! `GPSAreaInformation`) and `Copyright`. See `docs/format.md` §3.

use alloc::string::String;
use alloc::vec::Vec;

use crate::model::Exif;
use crate::tag::Tag;
use crate::tiff::ByteOrder;
use crate::value::{Value, ValueError};

/// The character code named by the 8-byte prefix of an encoded text value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TextEncoding {
    /// `"ASCII\0\0\0"`.
    Ascii,
    /// `"UNICODE\0"`: UTF-16 (UCS-2).
    Unicode,
    /// `"JIS\0\0\0\0\0"`: JIS X 0208.
    Jis,
    /// Eight NULs: undefined character code.
    Undefined,
    /// A prefix the spec does not define. The whole value is the text.
    Invalid,
}

/// An encoded text value: an 8-byte character-code prefix, then the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedText {
    encoding: TextEncoding,
    data: Vec<u8>,
    order: ByteOrder,
}

impl EncodedText {
    /// Parses an UNDEFINED value. `order` is the TIFF block's byte order,
    /// used for UTF-16 when the text itself gives no hint.
    ///
    /// **quirk** (as ExifTool): spaces instead of NULs in the prefix are
    /// accepted, and only the upper-case `UNICODE` prefix means UTF-16.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError::Invalid`] if the value is shorter than the prefix.
    pub fn parse(bytes: &[u8], order: ByteOrder) -> Result<Self, ValueError> {
        let Some((prefix, data)) = bytes.split_first_chunk::<8>() else {
            return Err(ValueError::Invalid);
        };
        let blank = |rest: &[u8]| rest.iter().all(|&b| b == 0 || b == b' ');
        let encoding = match prefix {
            [0, ..] => TextEncoding::Undefined,
            p if blank(p) => TextEncoding::Undefined,
            [b'A', b'S', b'C', b'I', b'I', 0, ..] => TextEncoding::Ascii,
            [b'A', b'S', b'C', b'I', b'I', rest @ ..] if blank(rest) => TextEncoding::Ascii,
            [b'U', b'N', b'I', b'C', b'O', b'D', b'E', 0 | b' '] => TextEncoding::Unicode,
            [b'J', b'I', b'S', rest @ ..] if blank(rest) => TextEncoding::Jis,
            _ => TextEncoding::Invalid,
        };
        let data = if encoding == TextEncoding::Invalid {
            bytes.to_vec()
        } else {
            data.to_vec()
        };
        Ok(Self {
            encoding,
            data,
            order,
        })
    }

    /// Returns the character code.
    #[must_use]
    pub const fn encoding(&self) -> TextEncoding {
        self.encoding
    }

    /// Returns the text bytes after the prefix (the whole value for
    /// [`TextEncoding::Invalid`]).
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Decodes the text, trimming trailing blanks. ASCII and undefined text
    /// stops at the first NUL and is decoded as UTF-8, replacing invalid
    /// bytes. UTF-16 byte order is taken from a BOM, else guessed from the
    /// text, else the TIFF byte order. Returns `None` for JIS, which needs
    /// conversion tables this crate does not carry.
    #[must_use]
    pub fn text(&self) -> Option<String> {
        let text = match self.encoding {
            TextEncoding::Jis => return None,
            TextEncoding::Unicode => decode_utf16(&self.data, self.order),
            TextEncoding::Ascii | TextEncoding::Undefined | TextEncoding::Invalid => {
                let end = self
                    .data
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(self.data.len());
                String::from_utf8_lossy(self.data.get(..end).unwrap_or_default()).into_owned()
            }
        };
        Some(String::from(text.trim_end_matches(' ')))
    }
}

/// Decodes UTF-16, choosing the byte order from a BOM, else from where the
/// zero bytes are (ASCII-range text has its zeros on the high byte), else
/// from the TIFF byte order.
fn decode_utf16(data: &[u8], order: ByteOrder) -> String {
    let (units, _) = data.as_chunks::<2>();
    let (order, skip) = match units.first() {
        Some([0xFF, 0xFE]) => (ByteOrder::LittleEndian, 1),
        Some([0xFE, 0xFF]) => (ByteOrder::BigEndian, 1),
        _ => {
            let zeros_high_first = units.iter().filter(|[a, b]| *a == 0 && *b != 0).count();
            let zeros_high_last = units.iter().filter(|[a, b]| *a != 0 && *b == 0).count();
            let guessed = match zeros_high_first.cmp(&zeros_high_last) {
                core::cmp::Ordering::Greater => ByteOrder::BigEndian,
                core::cmp::Ordering::Less => ByteOrder::LittleEndian,
                core::cmp::Ordering::Equal => order,
            };
            (guessed, 0)
        }
    };
    let code_units = units
        .iter()
        .skip(skip)
        .map(|&unit| order.u16(unit))
        .take_while(|&unit| unit != 0);
    char::decode_utf16(code_units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// The `Copyright` tag: photographer and editor copyright, NUL separated.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Copyright {
    /// The photographer's copyright notice.
    pub photographer: Option<String>,
    /// The editor's copyright notice.
    pub editor: Option<String>,
}

impl Copyright {
    /// Parses `"photographer\0editor\0"`. A part that is a single space (as
    /// the spec writes an absent photographer) or empty is `None`.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Self {
        let mut parts = bytes.split(|&b| b == 0);
        let mut part = || {
            parts
                .next()
                .map(|p| String::from(String::from_utf8_lossy(p).trim()))
                .filter(|p| !p.is_empty())
        };
        Self {
            photographer: part(),
            editor: part(),
        }
    }
}

impl Exif {
    fn encoded_text(&self, tag: Tag) -> Result<Option<EncodedText>, ValueError> {
        match self.entry(tag).map(|e| e.value()) {
            None => Ok(None),
            Some(Value::Undefined(bytes) | Value::Byte(bytes)) => {
                EncodedText::parse(bytes, self.byte_order()).map(Some)
            }
            Some(value) => Err(ValueError::WrongType { found: value.ty() }),
        }
    }

    /// The `UserComment` tag.
    ///
    /// # Errors
    ///
    /// Fails if the value is not UNDEFINED or is shorter than its prefix.
    pub fn user_comment(&self) -> Result<Option<EncodedText>, ValueError> {
        self.encoded_text(Tag::USER_COMMENT)
    }

    /// The `GPSProcessingMethod` tag.
    ///
    /// # Errors
    ///
    /// Fails like [`Exif::user_comment`].
    pub fn gps_processing_method(&self) -> Result<Option<EncodedText>, ValueError> {
        self.encoded_text(Tag::GPS_PROCESSING_METHOD)
    }

    /// The `GPSAreaInformation` tag.
    ///
    /// # Errors
    ///
    /// Fails like [`Exif::user_comment`].
    pub fn gps_area_information(&self) -> Result<Option<EncodedText>, ValueError> {
        self.encoded_text(Tag::GPS_AREA_INFORMATION)
    }

    /// The `Copyright` tag, split into its photographer and editor parts.
    ///
    /// # Errors
    ///
    /// Fails if the value is not text.
    pub fn copyright(&self) -> Result<Option<Copyright>, ValueError> {
        match self.entry(Tag::COPYRIGHT).map(|e| e.value()) {
            None => Ok(None),
            Some(Value::Ascii(bytes) | Value::Utf8(bytes)) => Ok(Some(Copyright::parse(bytes))),
            Some(value) => Err(ValueError::WrongType { found: value.ty() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(bytes: &[u8]) -> Option<String> {
        EncodedText::parse(bytes, ByteOrder::LittleEndian)
            .unwrap()
            .text()
    }

    #[test]
    fn ascii_and_undefined() {
        assert_eq!(text(b"ASCII\0\0\0Hello\0garbage"), Some("Hello".into()));
        assert_eq!(
            text(b"ASCII   Spaces in prefix   "),
            Some("Spaces in prefix".into())
        );
        assert_eq!(text(b"\0\0\0\0\0\0\0\0plain"), Some("plain".into()));
        assert_eq!(text(b"        "), Some(String::new()));
        let enc = EncodedText::parse(b"ASCII\0\0\0x", ByteOrder::BigEndian).unwrap();
        assert_eq!(enc.encoding(), TextEncoding::Ascii);
    }

    #[test]
    fn unicode_byte_order() {
        let le = b"UNICODE\0H\0i\0";
        let be = b"UNICODE\0\0H\0i";
        let bom = b"UNICODE\0\xFE\xFF\0O\0K";
        // Little-endian text in a big-endian file (Microsoft Photo) is detected.
        assert_eq!(
            EncodedText::parse(le, ByteOrder::BigEndian).unwrap().text(),
            Some("Hi".into())
        );
        assert_eq!(text(be), Some("Hi".into()));
        assert_eq!(text(bom), Some("OK".into()));
        let accented = "UNICODE\0"
            .bytes()
            .chain("é".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(text(&accented.collect::<Vec<_>>()), Some("é".into()));
    }

    #[test]
    fn jis_invalid_and_short() {
        assert_eq!(text(b"JIS\0\0\0\0\0\x30\x21"), None);
        let invalid = EncodedText::parse(b"Unicode\0ASCII text", ByteOrder::LittleEndian).unwrap();
        assert_eq!(invalid.encoding(), TextEncoding::Invalid);
        assert_eq!(invalid.text(), Some("Unicode".into()));
        assert_eq!(
            EncodedText::parse(b"short", ByteOrder::LittleEndian),
            Err(ValueError::Invalid)
        );
    }

    #[test]
    fn copyright_parts() {
        let both = Copyright::parse(b"Jane Doe\0Editor Co\0");
        assert_eq!(both.photographer.as_deref(), Some("Jane Doe"));
        assert_eq!(both.editor.as_deref(), Some("Editor Co"));
        let editor_only = Copyright::parse(b" \0Editor Co\0");
        assert_eq!(
            (editor_only.photographer, editor_only.editor.as_deref()),
            (None, Some("Editor Co"))
        );
        assert_eq!(
            Copyright::parse(b"Solo\0"),
            Copyright {
                photographer: Some("Solo".into()),
                editor: None
            }
        );
    }
}
