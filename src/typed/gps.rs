//! GPS helpers. See `docs/format.md` §5.

use crate::model::Exif;
use crate::tag::Tag;
use crate::value::{Rational, Value, ValueError};

/// Which surface an altitude is measured from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AltitudeDatum {
    /// The ellipsoidal surface of the geodetic system (what GNSS reports).
    Ellipsoid,
    /// Mean sea level.
    SeaLevel,
}

/// An altitude with its datum.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Altitude {
    /// Meters above (positive) or below (negative) the datum.
    pub meters: f64,
    /// The surface the altitude is measured from.
    pub datum: AltitudeDatum,
}

/// A read-only view of the GPS IFD. Get one with [`Exif::gps`].
#[derive(Debug, Clone, Copy)]
pub struct Gps<'a> {
    exif: &'a Exif,
}

impl Exif {
    /// Returns a view of the GPS IFD.
    #[must_use]
    pub const fn gps(&self) -> Gps<'_> {
        Gps { exif: self }
    }
}

impl Gps<'_> {
    /// Latitude in decimal degrees, negative for south.
    ///
    /// A missing `GPSLatitudeRef` is treated as north.
    ///
    /// # Errors
    ///
    /// Fails if the value is not three rationals with non-zero denominators,
    /// or the reference is not `N` or `S`.
    pub fn latitude(&self) -> Result<Option<f64>, ValueError> {
        self.coordinate(Tag::GPS_LATITUDE, Tag::GPS_LATITUDE_REF, b'N', b'S')
    }

    /// Longitude in decimal degrees, negative for west.
    ///
    /// A missing `GPSLongitudeRef` is treated as east.
    ///
    /// # Errors
    ///
    /// Fails like [`Gps::latitude`], with `E` and `W` as references.
    pub fn longitude(&self) -> Result<Option<f64>, ValueError> {
        self.coordinate(Tag::GPS_LONGITUDE, Tag::GPS_LONGITUDE_REF, b'E', b'W')
    }

    /// Latitude and longitude, if both are present and valid.
    #[must_use]
    pub fn coordinates(&self) -> Option<(f64, f64)> {
        Some((self.latitude().ok()??, self.longitude().ok()??))
    }

    /// Altitude and the datum it is measured from.
    ///
    /// Exif 3.0 redefined `GPSAltitudeRef` 0 and 1 from sea level to
    /// ellipsoidal height and added 2 and 3 for sea level. Files that declare
    /// an `ExifVersion` before `"0300"`, or none, use the old meaning.
    ///
    /// # Errors
    ///
    /// Fails if `GPSAltitude` is not one rational with a non-zero denominator,
    /// or `GPSAltitudeRef` is not 0–3.
    pub fn altitude(&self) -> Result<Option<Altitude>, ValueError> {
        let Some(meters) = self.exif.get::<Rational>(Tag::GPS_ALTITUDE)? else {
            return Ok(None);
        };
        let meters = meters.to_f64().ok_or(ValueError::Invalid)?;
        let reference = match self.exif.entry(Tag::GPS_ALTITUDE_REF).map(|e| e.value()) {
            None => 0,
            Some(Value::Byte(v) | Value::Undefined(v)) => match v.as_slice() {
                [r] => *r,
                _ => return Err(ValueError::Invalid),
            },
            // **quirk:** some writers store the reference as ASCII "0" / "1".
            Some(value @ Value::Ascii(_)) => match value.text() {
                Some("0") => 0,
                Some("1") => 1,
                _ => return Err(ValueError::Invalid),
            },
            Some(value) => return Err(ValueError::WrongType { found: value.ty() }),
        };
        let modern = self.exif_version().is_some_and(|v| v >= *b"0300");
        let (datum, below) = match (reference, modern) {
            (0 | 1, true) => (AltitudeDatum::Ellipsoid, reference == 1),
            (0 | 1, false) => (AltitudeDatum::SeaLevel, reference == 1),
            (2 | 3, _) => (AltitudeDatum::SeaLevel, reference == 3),
            _ => return Err(ValueError::Invalid),
        };
        let meters = if below { -meters } else { meters };
        Ok(Some(Altitude { meters, datum }))
    }

    fn exif_version(&self) -> Option<[u8; 4]> {
        match self.exif.entry(Tag::EXIF_VERSION)?.value() {
            Value::Undefined(v) | Value::Ascii(v) | Value::Byte(v) => v.get(..4)?.try_into().ok(),
            _ => None,
        }
    }

    fn coordinate(
        &self,
        tag: Tag,
        reference: Tag,
        positive: u8,
        negative: u8,
    ) -> Result<Option<f64>, ValueError> {
        let Some(dms) = self.exif.get::<alloc::vec::Vec<Rational>>(tag)? else {
            return Ok(None);
        };
        let [d, m, s] = dms.as_slice() else {
            return Err(ValueError::WrongCount { found: dms.len() });
        };
        let part = |r: &Rational| r.to_f64().ok_or(ValueError::Invalid);
        let degrees = part(d)? + part(m)? / 60.0 + part(s)? / 3600.0;
        let sign = match self
            .exif
            .entry(reference)
            .and_then(|e| e.value().text_bytes())
        {
            None | Some([]) => 1.0,
            Some([r, ..]) if r.eq_ignore_ascii_case(&positive) => 1.0,
            Some([r, ..]) if r.eq_ignore_ascii_case(&negative) => -1.0,
            Some(_) => return Err(ValueError::Invalid),
        };
        Ok(Some(sign * degrees))
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::model::tests_support::exif_with;

    fn r(num: u32, den: u32) -> Rational {
        Rational::new(num, den)
    }

    #[test]
    fn coordinates() {
        let exif = exif_with(&[
            (Tag::GPS_LATITUDE_REF, Value::ascii("S")),
            (
                Tag::GPS_LATITUDE,
                Value::Rational(vec![r(33, 1), r(51, 1), r(5442, 100)]),
            ),
            (Tag::GPS_LONGITUDE_REF, Value::ascii("E")),
            (
                Tag::GPS_LONGITUDE,
                Value::Rational(vec![r(151, 1), r(12, 1), r(3, 1)]),
            ),
        ]);
        let (lat, lon) = exif.gps().coordinates().unwrap();
        assert!((lat - -33.8651).abs() < 1e-4, "{lat}");
        assert!((lon - 151.200_833).abs() < 1e-4, "{lon}");
    }

    #[test]
    fn bad_coordinates() {
        let zero_den = exif_with(&[(
            Tag::GPS_LATITUDE,
            Value::Rational(vec![r(1, 0), r(0, 1), r(0, 1)]),
        )]);
        assert_eq!(zero_den.gps().latitude(), Err(ValueError::Invalid));
        let two = exif_with(&[(Tag::GPS_LATITUDE, Value::Rational(vec![r(1, 1), r(0, 1)]))]);
        assert_eq!(
            two.gps().latitude(),
            Err(ValueError::WrongCount { found: 2 })
        );
        assert_eq!(Exif::new().gps().coordinates(), None);
    }

    #[test]
    fn altitude_datum_depends_on_version() {
        let tags = |version: &[u8; 4], reference: u8| {
            exif_with(&[
                (Tag::EXIF_VERSION, Value::Undefined(version.to_vec())),
                (Tag::GPS_ALTITUDE_REF, Value::Byte(vec![reference])),
                (Tag::GPS_ALTITUDE, Value::Rational(vec![r(125, 2)])),
            ])
        };
        let alt = |exif: &Exif| exif.gps().altitude().unwrap().unwrap();

        let old = alt(&tags(b"0232", 1));
        assert_eq!((old.meters, old.datum), (-62.5, AltitudeDatum::SeaLevel));
        let new = alt(&tags(b"0300", 1));
        assert_eq!((new.meters, new.datum), (-62.5, AltitudeDatum::Ellipsoid));
        let sea = alt(&tags(b"0300", 2));
        assert_eq!((sea.meters, sea.datum), (62.5, AltitudeDatum::SeaLevel));
        assert_eq!(tags(b"0300", 4).gps().altitude(), Err(ValueError::Invalid));
    }

    #[test]
    fn altitude_ref_as_ascii_quirk() {
        let exif = exif_with(&[
            (Tag::GPS_ALTITUDE_REF, Value::ascii("1")),
            (Tag::GPS_ALTITUDE, Value::Rational(vec![r(10, 1)])),
        ]);
        let alt = exif.gps().altitude().unwrap().unwrap();
        assert_eq!((alt.meters, alt.datum), (-10.0, AltitudeDatum::SeaLevel));
    }
}
