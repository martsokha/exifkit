//! Development tasks for exifkit.
//!
//! ```text
//! cargo xtask codegen          regenerate src/tag/{generated,enums}.rs from data/tags.toml
//! cargo xtask codegen --check  fail if the committed files are out of date
//! cargo xtask fixtures         download the sample images in data/fixtures.toml
//! cargo xtask golden           record ExifTool's view of each sample in tests/golden/
//! ```

mod codegen;
mod fixtures;
mod table;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["codegen"] => codegen::run(&root(), false),
        ["codegen", "--check"] => codegen::run(&root(), true),
        ["fixtures"] => fixtures::fetch(&root()),
        ["golden"] => fixtures::golden(&root()),
        _ => {
            eprintln!("usage: cargo xtask (codegen [--check] | fixtures | golden)");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace root, one level above this crate.
fn root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map_or(manifest_dir.clone(), PathBuf::from)
}
