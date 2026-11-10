//! Tag lookups by id and by name must never panic, and must agree with each other.

#![no_main]

use exifkit::{Namespace, Tag};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: (u8, u16, &str)| {
    let (ns, id, name) = input;
    let ns = match ns % 3 {
        0 => Namespace::Main,
        1 => Namespace::Gps,
        _ => Namespace::Interop,
    };
    let tag = Tag::new(ns, id);
    if let Some(name) = tag.name() {
        assert_eq!(Tag::from_name(name), Some(tag));
    }
    if let Some(found) = Tag::from_name(name) {
        assert!(found.info().is_some());
    }
});
