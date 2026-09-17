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

# One-time: point git at the tracked .githooks/ dir. The pre-commit hook
# auto-formats staged Rust files and folds the changes into the commit; the
# pre-push hook runs `fmt-check` + `clippy`. core.hooksPath lives in the shared
# git config, so one run covers every worktree of this repo.
install-hooks:
    git config core.hooksPath .githooks
    chmod +x .githooks/*

# Cut a release: bump the version, commit, tag, and push (triggers release.yml).
# Run from a clean main, e.g. `just release 0.5.0`. The release tag must point at
# a commit whose Cargo.toml `version` matches the tag, so this recipe is the only
# safe way to cut one.
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! printf '%s' "{{version}}" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
      echo "usage: just release X.Y.Z (e.g. just release 0.5.0)" >&2
      exit 1
    fi
    if [ -n "$(git status --porcelain)" ]; then
      echo "working tree is not clean; commit or stash your changes first" >&2
      exit 1
    fi
    branch="$(git rev-parse --abbrev-ref HEAD)"
    if [ "$branch" != "main" ]; then
      echo "releases must be cut from main (currently on '$branch')" >&2
      exit 1
    fi
    if git rev-parse -q --verify "refs/tags/v{{version}}" >/dev/null; then
      echo "tag v{{version}} already exists" >&2
      exit 1
    fi
    git fetch --quiet origin main
    if [ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]; then
      echo "local main is not in sync with origin/main; pull or push first" >&2
      exit 1
    fi
    # Bump the version and let cargo refresh the workspace entry in Cargo.lock.
    sed -i -E 's/^version = ".*"/version = "{{version}}"/' Cargo.toml
    cargo metadata --format-version 1 >/dev/null
    just check
    git add Cargo.toml Cargo.lock
    git commit -m "Release {{version}}"
    git tag -a "v{{version}}" -m "v{{version}}"
    git push origin main
    git push origin "v{{version}}"
