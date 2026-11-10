//! Prints every Exif entry and diagnostic in a file.
//!
//! ```bash
//! cargo run --example dump -- photo.jpg
//! ```
//!
//! Reads TIFF files directly. For JPEG it does a minimal APP1 scan; the real
//! container support (docs/containers.md) arrives in milestone M2.

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
    let Some(tiff) = tiff_block(&data) else {
        eprintln!("{path}: no Exif found");
        return ExitCode::FAILURE;
    };
    let (exif, diagnostics) = match Exif::from_tiff(tiff) {
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

/// Returns the TIFF block: the whole file for TIFF, or the payload of the
/// first `Exif\0\0` APP1 segment for JPEG.
fn tiff_block(data: &[u8]) -> Option<&[u8]> {
    if data.starts_with(b"II*\0") || data.starts_with(b"MM\0*") {
        return Some(data);
    }
    let mut rest = data.strip_prefix(&[0xFF, 0xD8])?;
    while let [0xFF, marker, hi, lo, tail @ ..] = rest {
        if *marker == 0xDA {
            break; // start of scan: no metadata after this
        }
        let len = usize::from(u16::from_be_bytes([*hi, *lo])).checked_sub(2)?;
        let payload = tail.get(..len)?;
        if *marker == 0xE1
            && let Some(tiff) = payload.strip_prefix(b"Exif\0\0")
        {
            return Some(tiff);
        }
        rest = tail.get(len..)?;
    }
    None
}
