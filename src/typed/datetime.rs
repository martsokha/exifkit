//! Exif date and time values. See `docs/format.md` §4.

use core::fmt;

use crate::model::Exif;
use crate::tag::Tag;
use crate::value::{FromValue, Value, ValueError};

/// A UTC offset, as recorded by the `OffsetTime*` tags (`"+09:00"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Offset {
    minutes: i16,
}

impl Offset {
    /// UTC.
    pub const UTC: Self = Self { minutes: 0 };

    /// Creates an offset east of UTC, in minutes. Returns `None` outside ±23:59.
    #[must_use]
    pub const fn from_minutes(minutes: i16) -> Option<Self> {
        if minutes > -24 * 60 && minutes < 24 * 60 {
            Some(Self { minutes })
        } else {
            None
        }
    }

    /// Returns the offset east of UTC, in minutes.
    #[must_use]
    pub const fn minutes(self) -> i16 {
        self.minutes
    }

    /// Parses `"+HH:MM"` or `"-HH:MM"`. Blank (all spaces) parses as `None`.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError::Invalid`] for anything else.
    pub fn parse(text: &str) -> Result<Option<Self>, ValueError> {
        let text = text.trim_end_matches('\0');
        if text.trim().is_empty() {
            return Ok(None);
        }
        let bytes = text.as_bytes();
        let [sign, h1, h2, b':', m1, m2] = *bytes else {
            return Err(ValueError::Invalid);
        };
        let sign = match sign {
            b'+' => 1,
            b'-' => -1,
            _ => return Err(ValueError::Invalid),
        };
        let hours = digits2(h1, h2).ok_or(ValueError::Invalid)?;
        let minutes = digits2(m1, m2).ok_or(ValueError::Invalid)?;
        if hours > 23 || minutes > 59 {
            return Err(ValueError::Invalid);
        }
        let total = i16::from(hours) * 60 + i16::from(minutes);
        Ok(Self::from_minutes(sign * total))
    }
}

impl fmt::Display for Offset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.minutes < 0 { '-' } else { '+' };
        let abs = self.minutes.unsigned_abs();
        write!(f, "{sign}{:02}:{:02}", abs / 60, abs % 60)
    }
}

/// Fractional seconds, as recorded by the `SubSecTime*` tags.
///
/// The digits are kept exactly: `"05"` (50 ms) and `"5"` (500 ms) differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubSec {
    value: u32,
    digits: u8,
}

impl SubSec {
    /// Parses a run of ASCII digits (at most 9). Trailing spaces and NULs
    /// are ignored; blank parses as `None`.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError::Invalid`] for non-digits or more than 9 digits.
    pub fn parse(text: &str) -> Result<Option<Self>, ValueError> {
        let text = text.trim_end_matches(['\0', ' ']);
        if text.is_empty() {
            return Ok(None);
        }
        if text.len() > 9 || !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ValueError::Invalid);
        }
        let value = text.parse().map_err(|_| ValueError::Invalid)?;
        let digits = u8::try_from(text.len()).map_err(|_| ValueError::Invalid)?;
        Ok(Some(Self { value, digits }))
    }

    /// Returns the fraction in nanoseconds.
    #[must_use]
    pub fn nanos(self) -> u32 {
        let scale = 10u32.pow(9u32.saturating_sub(u32::from(self.digits)));
        self.value.saturating_mul(scale)
    }
}

impl fmt::Display for SubSec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:0width$}",
            self.value,
            width = usize::from(self.digits)
        )
    }
}

/// A date and time from the Exif `DateTime*` tags, with the matching
/// sub-second and UTC-offset tags merged in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DateTime {
    /// Year, e.g. 2026.
    pub year: u16,
    /// Month, 1–12.
    pub month: u8,
    /// Day of the month, 1–31.
    pub day: u8,
    /// Hour, 0–23.
    pub hour: u8,
    /// Minute, 0–59.
    pub minute: u8,
    /// Second, 0–60 (60 for a leap second).
    pub second: u8,
    /// Fractional seconds, from `SubSecTime*`.
    pub subsec: Option<SubSec>,
    /// UTC offset, from `OffsetTime*`. `None` means local time of unknown zone.
    pub offset: Option<Offset>,
}

impl DateTime {
    /// Parses `"YYYY:MM:DD HH:MM:SS"`.
    ///
    /// Blank values (spaces, as the spec uses for "unknown") and the all-zero
    /// value parse as `None`. **quirk:** also accepts `-` or `/` as date
    /// separators, `T` between date and time, and a missing seconds field.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError::Invalid`] if the text is not a date and time.
    pub fn parse(text: &str) -> Result<Option<Self>, ValueError> {
        let text = text.trim_end_matches('\0').trim();
        if text.is_empty() || text.bytes().all(|b| matches!(b, b'0' | b':' | b' ')) {
            return Ok(None);
        }
        let b = text.as_bytes();
        let (date, time) = match b {
            [d @ .., b' ' | b'T', h1, h2, b':', n1, n2, b':', s1, s2] if d.len() == 10 => {
                (d, [*h1, *h2, *n1, *n2, *s1, *s2])
            }
            [d @ .., b' ' | b'T', h1, h2, b':', n1, n2] if d.len() == 10 => {
                (d, [*h1, *h2, *n1, *n2, b'0', b'0'])
            }
            _ => return Err(ValueError::Invalid),
        };
        let &[y1, y2, y3, y4, sep1, mo1, mo2, sep2, d1, d2] = date else {
            return Err(ValueError::Invalid);
        };
        if sep1 != sep2 || !matches!(sep1, b':' | b'-' | b'/') {
            return Err(ValueError::Invalid);
        }
        let year = digits4(y1, y2, y3, y4).ok_or(ValueError::Invalid)?;
        let month = digits2(mo1, mo2).ok_or(ValueError::Invalid)?;
        let day = digits2(d1, d2).ok_or(ValueError::Invalid)?;
        let [h1, h2, n1, n2, s1, s2] = time;
        let hour = digits2(h1, h2).ok_or(ValueError::Invalid)?;
        let minute = digits2(n1, n2).ok_or(ValueError::Invalid)?;
        let second = digits2(s1, s2).ok_or(ValueError::Invalid)?;
        let valid = (1..=12).contains(&month)
            && (1..=31).contains(&day)
            && hour <= 23
            && minute <= 59
            && second <= 60;
        if !valid {
            return Err(ValueError::Invalid);
        }
        Ok(Some(Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
            subsec: None,
            offset: None,
        }))
    }
}

impl fmt::Display for DateTime {
    /// Formats as ISO 8601: `2026-01-30 12:34:56.120+09:00`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )?;
        if let Some(subsec) = self.subsec {
            write!(f, ".{subsec}")?;
        }
        if let Some(offset) = self.offset {
            write!(f, "{offset}")?;
        }
        Ok(())
    }
}

impl FromValue for DateTime {
    /// Converts a `DateTime*` value alone. Use [`Exif::date_time_original`]
    /// and friends to merge the sub-second and offset tags.
    fn from_value(value: &Value) -> Result<Self, ValueError> {
        let text = value.text().ok_or(ValueError::InvalidText)?;
        Self::parse(text)?.ok_or(ValueError::Invalid)
    }
}

const fn digit(b: u8) -> Option<u8> {
    if b.is_ascii_digit() {
        Some(b - b'0')
    } else {
        None
    }
}

fn digits2(a: u8, b: u8) -> Option<u8> {
    Some(digit(a)? * 10 + digit(b)?)
}

fn digits4(a: u8, b: u8, c: u8, d: u8) -> Option<u16> {
    Some(u16::from(digits2(a, b)?) * 100 + u16::from(digits2(c, d)?))
}

impl Exif {
    fn merged_date_time(
        &self,
        date_time: Tag,
        subsec: Tag,
        offset: Tag,
    ) -> Result<Option<DateTime>, ValueError> {
        let Some(entry) = self.entry(date_time) else {
            return Ok(None);
        };
        let text = entry.value().text().ok_or(ValueError::InvalidText)?;
        let Some(mut parsed) = DateTime::parse(text)? else {
            return Ok(None);
        };
        if let Some(text) = self.entry(subsec).and_then(|e| e.value().text()) {
            parsed.subsec = SubSec::parse(text)?;
        }
        if let Some(text) = self.entry(offset).and_then(|e| e.value().text()) {
            parsed.offset = Offset::parse(text)?;
        }
        Ok(Some(parsed))
    }

    /// When the image was captured: `DateTimeOriginal`, with
    /// `SubSecTimeOriginal` and `OffsetTimeOriginal` merged in.
    ///
    /// # Errors
    ///
    /// Fails if any of the three tags is malformed.
    pub fn date_time_original(&self) -> Result<Option<DateTime>, ValueError> {
        self.merged_date_time(
            Tag::DATE_TIME_ORIGINAL,
            Tag::SUB_SEC_TIME_ORIGINAL,
            Tag::OFFSET_TIME_ORIGINAL,
        )
    }

    /// When the image was stored as digital data: `DateTimeDigitized`, with
    /// `SubSecTimeDigitized` and `OffsetTimeDigitized` merged in.
    ///
    /// # Errors
    ///
    /// Fails if any of the three tags is malformed.
    pub fn date_time_digitized(&self) -> Result<Option<DateTime>, ValueError> {
        self.merged_date_time(
            Tag::DATE_TIME_DIGITIZED,
            Tag::SUB_SEC_TIME_DIGITIZED,
            Tag::OFFSET_TIME_DIGITIZED,
        )
    }

    /// When the file was last changed: `DateTime`, with `SubSecTime` and
    /// `OffsetTime` merged in.
    ///
    /// # Errors
    ///
    /// Fails if any of the three tags is malformed.
    pub fn date_time(&self) -> Result<Option<DateTime>, ValueError> {
        self.merged_date_time(Tag::DATE_TIME, Tag::SUB_SEC_TIME, Tag::OFFSET_TIME)
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn parses_spec_format() {
        let dt = DateTime::parse("2026:01:30 12:34:56\0").unwrap().unwrap();
        assert_eq!((dt.year, dt.month, dt.day), (2026, 1, 30));
        assert_eq!((dt.hour, dt.minute, dt.second), (12, 34, 56));
        assert_eq!(dt.to_string(), "2026-01-30 12:34:56");
    }

    #[test]
    fn unknown_values_are_none() {
        assert_eq!(DateTime::parse("                   "), Ok(None));
        assert_eq!(DateTime::parse("    :  :     :  :  "), Ok(None));
        assert_eq!(DateTime::parse("0000:00:00 00:00:00"), Ok(None));
        assert_eq!(Offset::parse("      "), Ok(None));
        assert_eq!(SubSec::parse(""), Ok(None));
    }

    #[test]
    fn quirks() {
        let iso = DateTime::parse("2026-01-30T12:34:56").unwrap().unwrap();
        let no_seconds = DateTime::parse("2026/01/30 12:34").unwrap().unwrap();
        assert_eq!((iso.day, iso.second), (30, 56));
        assert_eq!(no_seconds.second, 0);
        assert_eq!(
            DateTime::parse("2026:01-30 12:34:56"),
            Err(ValueError::Invalid)
        );
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "2026:13:01 00:00:00",
            "2026:01:30 24:00:00",
            "yesterday",
            "2026:01:30",
        ] {
            assert_eq!(DateTime::parse(bad), Err(ValueError::Invalid), "{bad}");
        }
        assert_eq!(Offset::parse("+24:00"), Err(ValueError::Invalid));
        assert_eq!(SubSec::parse("12a"), Err(ValueError::Invalid));
    }

    #[test]
    fn merges_subsec_and_offset() {
        use crate::model::tests_support::exif_with;
        let exif = exif_with(&[
            (Tag::DATE_TIME_ORIGINAL, Value::ascii("2026:01:30 12:34:56")),
            (Tag::SUB_SEC_TIME_ORIGINAL, Value::ascii("12")),
            (Tag::OFFSET_TIME_ORIGINAL, Value::ascii("+09:00")),
        ]);
        let dt = exif.date_time_original().unwrap().unwrap();
        assert_eq!(dt.to_string(), "2026-01-30 12:34:56.12+09:00");
        assert_eq!(dt.subsec.map(SubSec::nanos), Some(120_000_000));
        assert_eq!(exif.date_time(), Ok(None));
        assert_eq!(
            exif.get::<DateTime>(Tag::DATE_TIME_ORIGINAL)
                .unwrap()
                .unwrap()
                .offset,
            None
        );
    }

    #[test]
    fn offsets_and_subsec() {
        assert_eq!(Offset::parse("+09:00").unwrap().unwrap().minutes(), 540);
        assert_eq!(
            Offset::parse("-03:30").unwrap().unwrap().to_string(),
            "-03:30"
        );
        let short = SubSec::parse("5").unwrap().unwrap();
        let padded = SubSec::parse("05").unwrap().unwrap();
        assert_eq!(short.nanos(), 500_000_000);
        assert_eq!(padded.nanos(), 50_000_000);
        assert_eq!(padded.to_string(), "05");
    }
}
