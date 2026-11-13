//! Helpers shared by the corpus tests: where the fixtures and goldens are.
//!
//! The sample images are downloaded by `cargo xtask fixtures` (they are not
//! committed). `tests/golden/` holds what ExifTool 13.x reads from each,
//! recorded by `cargo xtask golden`. Without the images, the tests only
//! check that the goldens parse, so a plain `cargo test` works offline.

use std::path::{Path, PathBuf};

/// The repository root.
pub(crate) fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Collects every golden (`*.json`) under `dir`. The goldens are
/// committed, so any I/O error fails the test rather than leaving part of
/// the corpus unchecked.
pub(crate) fn goldens(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|err| panic!("reading {}: {err}", dir.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|err| panic!("reading {}: {err}", dir.display()))
            .path();
        if path.is_dir() {
            goldens(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
}

/// Reads a downloaded sample: `None` if it is absent (not downloaded yet);
/// any other I/O error fails the test.
pub(crate) fn fixture(path: &Path) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(data) => Some(data),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => panic!("reading {}: {err}", path.display()),
    }
}
