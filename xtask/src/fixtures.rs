//! `cargo xtask fixtures` and `cargo xtask golden`.
//!
//! Third-party sample images are never committed. `fixtures` downloads the
//! pinned list in `data/fixtures.toml` into `tests/fixtures/external/`, and
//! `golden` records what ExifTool reads from each one in `tests/golden/`,
//! which is committed. `tests/corpus.rs` compares exifkit against the goldens.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// ExifTool groups compared by the corpus test (family 1 names).
const GROUPS: &[&str] = &[
    "IFD0",
    "IFD1",
    "IFD2",
    "ExifIFD",
    "GPS",
    "InteropIFD",
    "SubIFD",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    source: Vec<Source>,
    fixture: Vec<Fixture>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    name: String,
    url: String,
    #[allow(dead_code)]
    notes: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    source: String,
    path: String,
    sha256: String,
    size: u64,
}

impl Fixture {
    fn rel(&self) -> PathBuf {
        Path::new(&self.source).join(&self.path)
    }
}

fn load(root: &Path) -> Result<List, String> {
    let text = std::fs::read_to_string(root.join("data/fixtures.toml"))
        .map_err(|e| format!("reading data/fixtures.toml: {e}"))?;
    toml::from_str(&text).map_err(|e| format!("data/fixtures.toml: {e}"))
}

fn sha256(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(
        Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

/// Percent-encodes everything except unreserved characters and `/`.
fn encode(path: &str) -> String {
    path.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn fetch(root: &Path) -> Result<(), String> {
    let list = load(root)?;
    let dir = root.join("tests/fixtures/external");
    let (mut fetched, mut cached) = (0, 0);
    for fixture in &list.fixture {
        let dest = dir.join(fixture.rel());
        if sha256(&dest).as_deref() == Some(fixture.sha256.as_str()) {
            cached += 1;
            continue;
        }
        let source = list
            .source
            .iter()
            .find(|s| s.name == fixture.source)
            .ok_or_else(|| format!("{}: unknown source {}", fixture.path, fixture.source))?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let url = format!("{}{}", source.url, encode(&fixture.path));
        let status = Command::new("curl")
            .args(["-sSfL", "--retry", "3", "-o"])
            .arg(&dest)
            .arg(&url)
            .status()
            .map_err(|e| format!("running curl: {e}"))?;
        if !status.success() {
            return Err(format!("downloading {url} failed"));
        }
        match sha256(&dest) {
            Some(digest) if digest == fixture.sha256 => fetched += 1,
            got => {
                let _ = std::fs::remove_file(&dest);
                return Err(format!(
                    "{}: SHA-256 mismatch (expected {}, got {got:?}, {} bytes expected)",
                    fixture.rel().display(),
                    fixture.sha256,
                    fixture.size
                ));
            }
        }
    }
    println!("fixtures: {fetched} downloaded, {cached} already present");
    Ok(())
}

pub fn golden(root: &Path) -> Result<(), String> {
    let list = load(root)?;
    let version = exiftool(&["-ver"])?;
    let version = version.trim();
    let external = root.join("tests/fixtures/external");
    let out_dir = root.join("tests/golden");
    let mut written = 0;
    for fixture in &list.fixture {
        let file = external.join(fixture.rel());
        if !file.exists() {
            return Err(format!(
                "{} is missing; run `cargo xtask fixtures` first",
                fixture.rel().display()
            ));
        }
        let mut args: Vec<String> = ["-j", "-D", "-n", "-a", "-u", "-G1:3:4"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        for group in GROUPS {
            args.push(format!("-{group}:all"));
            if *group == "SubIFD" {
                args.extend((1..=4).map(|n| format!("-SubIFD{n}:all")));
            }
        }
        args.push("-Warning".into());
        args.push(file.to_string_lossy().into_owned());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = exiftool(&args)?;
        let parsed: Value =
            serde_json::from_str(&output).map_err(|e| format!("{}: {e}", fixture.path))?;
        let object = parsed
            .get(0)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{}: unexpected exiftool output", fixture.path))?;
        let golden = normalize(object, version);
        let dest = out_dir.join(fixture.rel()).with_extension(
            fixture.rel().extension().map_or("json".to_owned(), |e| {
                format!("{}.json", e.to_string_lossy())
            }),
        );
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut text = serde_json::to_string_pretty(&golden).map_err(|e| e.to_string())?;
        text.push('\n');
        std::fs::write(&dest, text).map_err(|e| format!("{}: {e}", dest.display()))?;
        written += 1;
    }
    println!("golden: wrote {written} files with ExifTool {version}");
    Ok(())
}

/// Keeps the compared groups, numeric tag ids only, ordered by group, copy
/// number and id. Keys look like `IFD0:Main:Copy1:Make` because of
/// `-G1:3:4`. Tags from embedded documents (family 3 `Doc1`, `Doc2`, …, such
/// as the JPEG preview inside an RW2 file) are dropped: they are not part of
/// the file's own Exif.
fn normalize(object: &Map<String, Value>, version: &str) -> Value {
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    for (key, item) in object {
        let parts: Vec<&str> = key.split(':').collect();
        let (Some(&group), Some(&name)) = (parts.first(), parts.last()) else {
            continue;
        };
        if parts.len() < 2 {
            continue;
        }
        let middle = parts.get(1..parts.len() - 1).unwrap_or_default();
        if middle.iter().any(|p| p.starts_with("Doc")) {
            continue;
        }
        let copy: u32 = middle
            .iter()
            .find_map(|p| p.strip_prefix("Copy")?.parse().ok())
            .unwrap_or(0);
        if group == "ExifTool" && name == "Warning" {
            if let Some(text) = item.get("val").and_then(Value::as_str) {
                warnings.push(text.to_owned());
            }
            continue;
        }
        // SubIFD, SubIFD1, SubIFD2, … all sort and compare as SubIFDs.
        let base_group = if group.starts_with("SubIFD") {
            "SubIFD"
        } else {
            group
        };
        let known = GROUPS.contains(&base_group);
        let Some(id) = item.get("id").and_then(Value::as_u64) else {
            continue;
        };
        if !known {
            continue;
        }
        entries.push((
            GROUPS
                .iter()
                .position(|g| *g == base_group)
                .unwrap_or(GROUPS.len()),
            group.to_owned(),
            copy,
            id,
            json!({
                "group": group,
                "copy": copy,
                "id": id,
                "name": name,
                "value": item.get("val").cloned().unwrap_or(Value::Null),
            }),
        ));
    }
    entries.sort_by(|a, b| (a.0, &a.1, a.3, a.2).cmp(&(b.0, &b.1, b.3, b.2)));
    // ExifTool numbers copies across all groups; renumber them per group and
    // tag, so `copy` is the occurrence index within one IFD.
    let mut previous: Option<(String, u64)> = None;
    let mut index = 0u64;
    let entries: Vec<Value> = entries
        .into_iter()
        .map(|(_, group, _, id, mut entry)| {
            let key = (group, id);
            index = if previous.as_ref() == Some(&key) {
                index + 1
            } else {
                0
            };
            previous = Some(key);
            entry["copy"] = json!(index);
            entry
        })
        .collect();
    warnings.sort();
    json!({
        "exiftool": version,
        "entries": entries,
        "warnings": warnings,
    })
}

fn exiftool(args: &[&str]) -> Result<String, String> {
    let output = Command::new("exiftool")
        .args(args)
        .output()
        .map_err(|e| format!("running exiftool (is it installed?): {e}"))?;
    // ExifTool exits non-zero when a file has no metadata it can read; the
    // JSON on stdout is still valid.
    String::from_utf8(output.stdout).map_err(|e| format!("exiftool output: {e}"))
}
