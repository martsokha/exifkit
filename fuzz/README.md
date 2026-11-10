# Fuzzing

Fuzz targets for exifkit, run with [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz)
on a nightly toolchain. This directory is its own Cargo workspace, so it does not
affect the main build.

```bash
cargo install cargo-fuzz
cargo +nightly fuzz list
cargo +nightly fuzz run tiff_parse -- -rss_limit_mb=256 -max_total_time=600
```

| Target        | Checks                                                     |
|---------------|------------------------------------------------------------|
| `tiff_parse`  | `Exif::from_tiff` and `Display` never panic; strict mode fails exactly when lenient mode reports an error |
| `tiff_header` | `Header::parse` never panics and round-trips what it accepts |
| `tag_lookup`  | Lookups by id and by name never panic and agree            |

The memory cap (`-rss_limit_mb=256`) catches allocation-size bugs. When a run
finds a crash, minimize it (`cargo fuzz tmin`) and commit the input as a
regression test: every bug fix adds a fixture or a corpus entry
(see `docs/roadmap.md`). The planned targets (`read_any`, `roundtrip`,
`write_into`) arrive with the containers and the writer.
