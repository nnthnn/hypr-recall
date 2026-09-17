# Contributing to hypr-recall

Thanks for your interest in improving hypr-recall! This guide covers the build,
the development workflow, and what's expected before a pull request.

## Prerequisites

- **Rust** — Arch's `rust` package (`sudo pacman -S --needed rust`) ships
  `cargo`, `rustc`, `rustfmt` and `clippy`. CI builds with a pinned Rust
  version, so a newer local `clippy` can occasionally emit different lints.
- **Build tools** — `sudo pacman -S --needed base-devel git` (brings in
  `pkg-config`, needed to build the GTK bindings).
- **[`just`](https://github.com/casey/just)** — optional, but the dev tasks are
  wrapped in a `justfile`. Run `just` to list recipes. (It's also used by the
  optional git hooks below.)
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

### Git hooks (optional)

A tracked `.githooks/` directory holds two hooks that catch the format/lint
failures CI would otherwise reject. Enable them once per clone:

```fish
just install-hooks
```

`core.hooksPath` is shared git config, so one run also covers every worktree of
this repo.

- **`pre-commit`** — runs `rustfmt` over the Rust files staged for this commit
  and re-stages any changes, so commits are always formatted. Only the staged
  files are touched. A partially-staged file is a known caveat: rustfmt rewrites
  the whole working-tree file, so its unstaged hunks get folded in too.
- **`pre-push`** — runs `just fmt-check clippy` (the checks CI gates on). Tests
  and the overlay build are left to CI so pushes stay fast.

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

## Releasing

`packaging/aur/hypr-recall/` is the source of truth for the AUR package.
Releases are automated by `.github/workflows/release.yml`, which runs whenever
a `v*` tag is pushed. Cut one from a clean, up-to-date `main` with:

```fish
just release X.Y.Z
```

The recipe bumps `version` in `Cargo.toml`, refreshes `Cargo.lock`, runs the
pre-PR checks, commits as `Release X.Y.Z`, and pushes the tag. Cut a release by
hand only if you replicate all of that: the tag must point at a commit whose
`Cargo.toml` `version` equals the tag, and the workflow's first step rejects it
otherwise (which is exactly what it's there to catch).

The workflow then:

1. Verifies the tag matches the `version` in `Cargo.toml`.
2. Builds a source tarball with `git archive` and attaches it to the GitHub
   release. This is deliberate: GitHub's auto-generated `/archive/` tarballs are
   not guaranteed to keep the same bytes, which would silently invalidate the
   AUR `sha256sums`. A release asset does not change.
3. Rewrites `pkgver`/`pkgrel`/`source`/`sha256sums` in the `PKGBUILD`, points
   `source` at that release asset, regenerates `.SRCINFO` with `makepkg`, and
   pushes both to the AUR's `hypr-recall.git` (a separate, per-package repo AUR
   hosts — not this one). This step is skipped entirely until the AUR secret
   below is configured, so tagging a release works with or without AUR access.

One repository secret is needed to publish to the AUR: `AUR_SSH_PRIVATE_KEY`,
the private half of a key registered to the AUR account that owns `hypr-recall`
(add the public half under the account's SSH keys on the AUR website). Until
that secret is set, the AUR job is skipped automatically and tagging a release
still builds and publishes the GitHub release and its source tarball. Optionally
set `AUR_COMMIT_EMAIL` to control the commit author; it defaults to
`github-actions[bot]@users.noreply.github.com`.

If you change other fields in `packaging/aur/hypr-recall/PKGBUILD` (dependencies,
description, …), regenerate the committed `.SRCINFO` yourself before merging:

```fish
cd packaging/aur/hypr-recall
makepkg --printsrcinfo > .SRCINFO
```

## Scope

A few things are intentionally out of scope: floating-window positions, special
workspaces, and multi-monitor layout differences (column widths are stored as
monitor-relative ratios). If you're unsure whether a change fits, open an issue
to discuss before investing in a PR.
