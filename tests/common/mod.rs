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

/// Collects every golden (`*.json`) under `dir`.
pub(crate) fn goldens(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            goldens(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
}
