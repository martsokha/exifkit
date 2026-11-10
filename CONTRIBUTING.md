# Contributing

Thank you for your interest in contributing to exifkit! We appreciate your
help in making this project better.

## Code of Conduct

This project follows the
[Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct). By
participating, you are expected to uphold this code.

## Getting Started

1. Fork the repository on GitHub
2. Clone your fork locally:
   ```bash
   git clone https://github.com/YOUR_USERNAME/exifkit.git
   cd exifkit
   ```
3. Add the upstream repository:
   ```bash
   git remote add upstream https://github.com/martsokha/exifkit.git
   ```

Read [`docs/README.md`](docs/README.md) first. Design decisions live there,
and a PR that changes documented behavior updates the doc in the same PR.

## Development Setup

### Prerequisites

- Rust 1.95 or later
- A nightly toolchain for `rustfmt` (the config uses unstable options)

### Building the Project

```bash
# Run tests
cargo test --workspace --all-features

# Check formatting
cargo +nightly fmt --all --check

# Run clippy
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Check the no_std build
cargo build --no-default-features --target thumbv7em-none-eabihf
```

### The tag table

Tags and their value enums live in `data/tags.toml`. After editing it,
regenerate the Rust code and commit both:

```bash
cargo xtask codegen          # rewrites src/tag/generated.rs and src/tag/enums.rs
cargo xtask codegen --check  # what CI runs
```

Never edit the generated files by hand. Codegen formats them with nightly
rustfmt; set `RUSTFMT` to use another binary.

### Sample corpus

`tests/corpus.rs` compares exifkit with ExifTool on real files:

```bash
cargo xtask fixtures   # download the samples in data/fixtures.toml (not committed)
cargo test --test corpus
cargo xtask golden     # after adding samples: re-record ExifTool's output (needs exiftool)
```

To add a sample, append it to `data/fixtures.toml` with its SHA-256, and
record where it comes from. Then run `cargo xtask golden` and commit the
new golden file. Never commit the image itself.

### Fuzzing

See [`fuzz/README.md`](fuzz/README.md). Fuzzing needs nightly and
`cargo install cargo-fuzz`.

## Making Changes

1. Create a new branch for your changes:
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. Make your changes following our code style. Parsing code must never
   panic on input: no indexing, `unwrap` or `expect` on data from the
   input, and use checked arithmetic (see
   [`docs/architecture.md`](docs/architecture.md#safety-model)).

3. Add tests for any new functionality. Every bug fix adds a fixture or a
   fuzz-corpus entry that reproduces it.

4. Ensure all tests pass:
   ```bash
   cargo test --all-features
   ```

5. Update documentation if needed

## Submitting Changes

1. Commit your changes with clear, descriptive commit messages

2. Push your changes to your fork:
   ```bash
   git push origin feature/your-feature-name
   ```

3. Create a Pull Request on GitHub

### Pull Request Guidelines

- Keep PRs focused on a single feature or fix
- Update CHANGELOG.md with your changes
- Ensure CI passes (tests, formatting, clippy)

## Reporting Bugs

If you find a bug, please create an issue on GitHub with:

- Steps to reproduce the issue
- A sample image that triggers it, if you can share one
- Expected vs actual behavior
- Rust version and operating system

## License

By contributing, you agree that your contributions will be licensed under the
MIT License.
