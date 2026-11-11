//! Prints every Exif entry and diagnostic in a file.
//!
//! ```bash
//! cargo run --example dump -- photo.jpg
//! ```
//!
//! Supports JPEG, TIFF (including DNG and other TIFF-based raw files), PNG and
//! WebP. Build with `--all-features` to also read legacy PNG text chunks.

use std::process::ExitCode;

use exifkit::{Exif, IfdKind};

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: dump <file>");
        return ExitCode::from(2);
    };
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(err) => {
            eprintln!("{path}: {err}");
            return ExitCode::FAILURE;
        }
    };
    let (exif, diagnostics) = match Exif::read(&data) {
        Ok(read) => read,
        Err(err) => {
            eprintln!("{path}: {err}");
            return ExitCode::FAILURE;
        }
    };

    let mut current: Option<IfdKind> = None;
    for (kind, entry) in exif.entries() {
        if current != Some(kind) {
            println!("[{kind:?}]");
            current = Some(kind);
        }
        println!("  {entry}");
    }
    if let Some(thumbnail) = exif.thumbnail() {
        println!("thumbnail: {} bytes", thumbnail.len());
    }
    if let Ok(Some(taken)) = exif.date_time_original() {
        println!("taken: {taken}");
    }
    for diagnostic in &diagnostics {
        println!("warning: {diagnostic}");
    }
    ExitCode::SUCCESS
}
