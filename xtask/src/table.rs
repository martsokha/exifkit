//! Loading and validating `data/tags.toml`.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    #[serde(rename = "enum", default)]
    pub enums: Vec<Enum>,
    #[serde(rename = "tag", default)]
    pub tags: Vec<Tag>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enum {
    pub name: String,
    pub values: Vec<EnumValue>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnumValue {
    pub value: u16,
    pub variant: String,
    pub desc: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tag {
    pub name: String,
    pub alias: Option<String>,
    #[serde(rename = "const")]
    pub const_name: Option<String>,
    pub id: u16,
    #[serde(default)]
    pub ns: Namespace,
    pub ifd: Option<Ifd>,
    pub types: Vec<Type>,
    pub count: Count,
    #[serde(default)]
    pub role: Role,
    pub target: Option<Ifd>,
    #[serde(default)]
    pub origin: Origin,
    pub since: Option<String>,
    #[serde(rename = "enum")]
    pub enum_name: Option<String>,
    #[serde(default)]
    pub personal: bool,
    #[serde(default = "yes")]
    pub verified: bool,
}

fn yes() -> bool {
    true
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    Deserialize
)]
#[serde(rename_all = "kebab-case")]
pub enum Namespace {
    #[default]
    Main,
    Gps,
    Interop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[allow(clippy::enum_variant_names)] // `SubIfd` mirrors `IfdKind::SubIfd`.
pub enum Ifd {
    Primary,
    Thumbnail,
    Exif,
    Gps,
    Interop,
    SubIfd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Type {
    Byte,
    Ascii,
    Short,
    Long,
    Rational,
    Sbyte,
    Undefined,
    Sshort,
    Slong,
    Srational,
    Float,
    Double,
    Ifd,
    Utf8,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Count {
    Fixed(u32),
    Any(AnyMarker),
    OneOf(Vec<u32>),
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub enum AnyMarker {
    #[serde(rename = "any")]
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    #[default]
    Value,
    Pointer,
    DataOffset,
    DataLength,
    MakerNote,
    Computed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    Tiff,
    TiffEp,
    #[default]
    Exif,
    Dng,
    Adobe,
    Microsoft,
    Other,
}

impl Tag {
    /// The IFD the tag belongs in, defaulting from the namespace.
    pub fn ifd(&self) -> Ifd {
        self.ifd.unwrap_or(match self.ns {
            Namespace::Main => Ifd::Primary,
            Namespace::Gps => Ifd::Gps,
            Namespace::Interop => Ifd::Interop,
        })
    }

    /// The generated constant name.
    pub fn const_name(&self) -> String {
        self.const_name
            .clone()
            .unwrap_or_else(|| screaming_snake(&self.name))
    }

    /// `since`, parsed as `(major, minor)`.
    pub fn since(&self) -> Result<Option<(u8, u8)>, String> {
        let Some(since) = &self.since else {
            return Ok(None);
        };
        let parsed = since
            .split_once('.')
            .and_then(|(major, minor)| Some((major.parse().ok()?, minor.parse().ok()?)));
        parsed.map(Some).ok_or_else(|| {
            format!(
                "{}: bad `since` {since:?}, expected e.g. \"2.31\"",
                self.name
            )
        })
    }
}

/// `CamelCase` → `SCREAMING_SNAKE`, splitting before an uppercase letter that
/// follows a lowercase letter or digit, before the last capital of an acronym
/// (`GPSInfo` → `GPS_INFO`), and before a digit run that follows a letter.
pub fn screaming_snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 {
            let prev = chars[i - 1];
            let next = chars.get(i + 1).copied();
            let split = (c.is_ascii_uppercase()
                && (prev.is_ascii_lowercase()
                    || prev.is_ascii_digit()
                    || (prev.is_ascii_uppercase()
                        && next.is_some_and(|n| n.is_ascii_lowercase()))))
                || (c.is_ascii_digit() && prev.is_ascii_alphabetic());
            if split {
                out.push('_');
            }
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

/// Loads and validates the table. Returns it with tags sorted by `(ns, id)`.
pub fn load(text: &str) -> Result<Table, String> {
    let mut table: Table = toml::from_str(text).map_err(|e| format!("data/tags.toml: {e}"))?;
    validate(&table)?;
    table.tags.sort_by_key(|tag| (tag.ns, tag.id));
    Ok(table)
}

fn validate(table: &Table) -> Result<(), String> {
    let mut errors = Vec::new();

    let mut enum_names = BTreeSet::new();
    for e in &table.enums {
        if !enum_names.insert(e.name.as_str()) {
            errors.push(format!("enum {}: defined twice", e.name));
        }
        let mut variants = BTreeSet::new();
        let mut values = BTreeSet::new();
        for v in &e.values {
            if v.variant == "Other" {
                errors.push(format!("enum {}: `Other` is reserved", e.name));
            }
            if !is_ident(&v.variant) {
                errors.push(format!("enum {}: bad variant name {:?}", e.name, v.variant));
            }
            if !variants.insert(v.variant.as_str()) {
                errors.push(format!("enum {}: duplicate variant {}", e.name, v.variant));
            }
            if !values.insert(v.value) {
                errors.push(format!("enum {}: duplicate value {}", e.name, v.value));
            }
        }
    }

    let mut ids = BTreeMap::new();
    let mut names = BTreeMap::new();
    let mut consts = BTreeMap::new();
    let mut used_enums = BTreeSet::new();
    for tag in &table.tags {
        let label = &tag.name;
        if let Some(other) = ids.insert((tag.ns, tag.id), label) {
            errors.push(format!("{label}: same namespace and id as {other}"));
        }
        for name in std::iter::once(&tag.name).chain(&tag.alias) {
            if let Some(other) = names.insert(name.as_str(), label) {
                errors.push(format!("{label}: name {name:?} already used by {other}"));
            }
        }
        let const_name = tag.const_name();
        if !is_ident(&const_name) {
            errors.push(format!("{label}: bad constant name {const_name:?}"));
        }
        if let Some(other) = consts.insert(const_name.clone(), label) {
            errors.push(format!(
                "{label}: constant {const_name} already used by {other}"
            ));
        }
        if tag.types.is_empty() {
            errors.push(format!("{label}: `types` is empty"));
        }
        if let Count::OneOf(counts) = &tag.count
            && counts.is_empty()
        {
            errors.push(format!("{label}: `count` list is empty"));
        }
        match (tag.role, tag.target) {
            (Role::Pointer, None) => errors.push(format!("{label}: pointer without `target`")),
            (Role::Pointer, Some(Ifd::Primary | Ifd::Thumbnail)) => {
                errors.push(format!("{label}: pointer target must be a sub-IFD"));
            }
            (Role::Pointer, Some(_)) | (_, None) => {}
            (_, Some(_)) => errors.push(format!("{label}: `target` on a non-pointer")),
        }
        if let Some(enum_name) = &tag.enum_name {
            used_enums.insert(enum_name.as_str());
            if !enum_names.contains(enum_name.as_str()) {
                errors.push(format!("{label}: unknown enum {enum_name}"));
            }
            if !tag
                .types
                .iter()
                .all(|t| matches!(t, Type::Short | Type::Byte | Type::Undefined))
            {
                errors.push(format!(
                    "{label}: enums need byte, short or undefined types"
                ));
            }
        }
        if let Err(err) = tag.since() {
            errors.push(err);
        }
    }
    for name in enum_names.difference(&used_enums) {
        errors.push(format!("enum {name}: not used by any tag"));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n       "))
    }
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::screaming_snake;

    #[test]
    fn constant_names() {
        assert_eq!(screaming_snake("ImageWidth"), "IMAGE_WIDTH");
        assert_eq!(screaming_snake("GPSInfoIFDPointer"), "GPS_INFO_IFD_POINTER");
        assert_eq!(screaming_snake("FNumber"), "F_NUMBER");
        assert_eq!(screaming_snake("YCbCrPositioning"), "Y_CB_CR_POSITIONING");
        assert_eq!(screaming_snake("GPSVersionID"), "GPS_VERSION_ID");
        assert_eq!(screaming_snake("DateTimeOriginal"), "DATE_TIME_ORIGINAL");
    }
}
