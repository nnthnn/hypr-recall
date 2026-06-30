# hypr-recall task runner — run `just` to list recipes.

# Show available recipes.
default:
    @just --list

# Pre-PR checklist: format, lint, test (core binary only).
check: fmt-check clippy test

# Same as `check` plus the optional overlay feature (needs gtk4 / gtk4-layer-shell).
check-all: fmt-check clippy-all test-all

# Verify formatting without writing changes.
fmt-check:
    cargo fmt --check

# Apply formatting.
fmt:
    cargo fmt

# Lint all targets, treating warnings as errors.
clippy:
    cargo clippy --all-targets -- -D warnings

# Lint with the overlay feature enabled.
clippy-all:
    cargo clippy --all-targets --all-features -- -D warnings

# Run the test suite.
test:
    cargo test

# Run the test suite with all features.
test-all:
    cargo test --all-features

# Build the release binaries (core + overlay).
build:
    cargo build --release --all-features

# Install the core binary to ~/.cargo/bin.
install:
    cargo install --path . --locked --force

# Install the core + overlay binaries (needs gtk4 / gtk4-layer-shell).
install-all:
    cargo install --path . --all-features --locked --force

# Audit dependencies for known security advisories (needs cargo-deny).
audit:
    cargo deny check advisories
