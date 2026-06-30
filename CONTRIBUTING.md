# Contributing to hypr-recall

Thanks for your interest in improving hypr-recall! This guide covers the build,
the development workflow, and what's expected before a pull request.

## Prerequisites

- **Rust** — the toolchain is pinned in `rust-toolchain.toml`, so `rustup` will
  install the right version (with `rustfmt` and `clippy`) automatically on first
  build. No manual setup needed.
- **[`just`](https://github.com/casey/just)** — optional, but the dev tasks are
  wrapped in a `justfile`. Run `just` to list recipes.
- **gtk4 + gtk4-layer-shell** — only needed to build the optional restore
  overlay (the `overlay` cargo feature). The core binary builds without them.

## Building

```fish
cargo build                      # core binary
cargo build --all-features       # core + overlay (needs gtk4 / gtk4-layer-shell)
just build                       # release build of both
```

See the README's Install section for installing the binaries.

## Development workflow

New features go on a branch and ship via pull request — **never commit features
directly to `main`**. Branch naming follows `feat/<name>` (e.g. `feat/status`),
or `docs/<name>` / `fix/<name>` as appropriate.

### Before opening a PR

Run the checklist — CI runs the same checks and will fail the PR otherwise:

```fish
just check        # cargo fmt --check + clippy (-D warnings) + cargo test
just check-all    # the above, plus the overlay feature (needs gtk4)
```

Or run them directly:

```fish
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI mirrors this across three jobs: a core lint/test job, an overlay job that
builds the `overlay` feature against gtk4, and a `cargo-deny` advisory audit.

### Don't forget the docs

If your change adds or changes a command, flag, config key, or user-visible
behavior, update the docs in the same PR:

- `README.md` — usage examples, config docs, feature list
- `docs/index.html` — the website
- `docs/guide.html` — the guide

## Code style

- Formatting is enforced by `rustfmt` (`cargo fmt`).
- Lints run under `clippy` with the `pedantic` and `cargo` groups enabled and
  **warnings treated as errors**. Prefer fixing a lint over `#[allow(...)]`; when
  an allow is genuinely warranted, scope it narrowly and add a one-line reason.
- Keep pure logic separate from I/O where practical — it's what keeps the
  planning code (e.g. `plan_workspace`, `plan_column_swaps`) unit-testable
  without a live Hyprland. New logic of that kind should come with tests.

## Project layout

The module map and key design decisions live in the README and in the source —
`src/restore.rs` (the restore orchestration) and `src/hyprland.rs` (the `hyprctl`
/ socket2 IPC layer) are the best places to start.

## Scope

A few things are intentionally out of scope: floating-window positions, special
workspaces, and multi-monitor layout differences (column widths are stored as
monitor-relative ratios). If you're unsure whether a change fits, open an issue
to discuss before investing in a PR.
