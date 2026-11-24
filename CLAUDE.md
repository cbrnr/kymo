# Kymo

Kymo (after the kymograph) is a Rust viewer for large offline EEG/MEG recordings.
Frame cost and memory depend on the screen, never on the file size.
It will also ship in a Python wheel as an MNE-Python browser backend and launch from MNELAB (see "Roadmap" in `README.md`).

**Read "Architecture" in `CONTRIBUTING.md` before changing anything architectural.**

## Scope

The crates:

1. `crates/kymo-core`: recording model, EDF/BDF, BrainVision, EEGLAB, GDF, FIFF, XDF and BVRF readers, annotation and `channels.tsv` files, index, pyramid with DC sums, tile cache, fixed-grid query.
2. `crates/kymo`: egui + wgpu binary on top of `kymo-core`.

Not built yet: other file formats, display filters, re-referencing, Python packaging. SSP and bipolar montages are out of scope.
Don't build anything that assumes EDF's record layout or a single sampling rate above the reader interface.

## Layout

```
crates/kymo-core     library, no GUI dependencies
crates/kymo          eframe/egui-wgpu viewer (`kymo path/to/file.bdf`); everything lives in its lib, `main.rs` only dispatches the parsed command
crates/kymo-testgen  generates synthetic EDF/BDF, BrainVision, EEGLAB, GDF, FIFF, XDF and BVRF files for tests and benchmarks
scripts/             Python helpers (validation against MNE)
packaging/           macOS bundle and disk image, Windows installer, Linux desktop files (the AUR package lives on the AUR)
CONTRIBUTING.md      toolchain setup per platform, build and checks, architecture, releasing
CHANGELOG.md         release notes in MNELAB's format; user-visible changes add a line under [UNRELEASED]
```

## Non-negotiables

- Never load a whole recording into memory. Source files are memory-mapped read-only and never modified.
- Peaks must never disappear: zoomed out = min/max envelopes, never subsampling. Gaps (EDF+D) stay gaps (NaN).
- Bucket boundaries are fixed in sample-index space (global tile grid), not relative to the view start.
- The UI thread never blocks on disk or computation; missing tiles fall back to coarser ones.
- Physical value = digital × gain + offset; EDF/BDF offsets can be huge.
- DC removal is a setting (View → Remove DC, remembered), on by default.
- Default initial view: ~20 s from the start of the recording.

## Conventions

- Stable Rust, edition 2024. `cargo fmt` and `cargo clippy --all-targets -- -D warnings` must be clean.
- Error handling: `thiserror` in `kymo-core`, `anyhow` only in binaries. No `unwrap()` on file input.
- `unsafe` only where unavoidable (memory mapping; attaching the native menu to the window on Windows, as muda requires, and letting it follow the theme there (uxtheme, and the app mode and accent color from the registry); attaching to the terminal's console on Windows; adding the method that receives files from Finder to winit's app delegate on macOS; reading peak memory in the frames benchmark), with a `// SAFETY:` comment.
- Tests are behavioral: they check outputs (decoded samples, tile values, annotations), not internal structure.
  Use `proptest` to compare tiles against a brute-force min/max pass.
- Integration tests live in one binary per crate, `tests/all/`, a module per area (`main.rs` lists them), so they link once
  and run in parallel. Proptest keeps their failure seeds in `tests/proptest-regressions/<module>.txt`; commit them.
- Test data comes from `crates/kymo-testgen`; never commit large binary files. Real recordings live outside the repo,
  pointed to by the `KYMO_TEST_DATA` env var; tests that need them are `#[ignore]` by default.
- Benchmarks with `criterion` in `crates/kymo-core/benches`.

## Commands

```
cargo build --workspace
cargo test --workspace
cargo test --workspace -- --ignored        # needs KYMO_TEST_DATA
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p kymo-testgen -- --help
cargo run -p kymo --release -- file.bdf
cargo bench -p kymo-core                   # build and decode benches on generated files
KYMO_BENCH_BDF=big.bdf cargo bench -p kymo-core --bench query   # big.bdf from `kymo-testgen bdf-256ch-8h`
cargo run --release -p kymo-core --example pyramid -- file.bdf [--sidecar DIR]   # build speed
cargo bench -p kymo --bench frames -- file.bdf [--fresh] [--antialias] [--edit] [--measure]   # viewer frame times, kymo's work per frame and memory; --edit adds annotation editing (on `kymo-testgen edf+c-annotations`), --measure a measurement on screen
scripts/compare_mne.py file.bdf      # validation against MNE (needs uv); EDF, BDF, BrainVision (.vhdr), EEGLAB (.set), GDF, FIFF, XDF (against pyxdf and mnextend) and BVRF (.bvrh, against pybvrf); also takes directories
scripts/compare_mne.py --testgen     # check kymo-testgen presets against MNE
scripts/compare_mne.py --annotations ...   # also annotation files both ways
scripts/compare_mne.py --channels ...      # also channels.tsv files both ways, against MNE-BIDS
scripts/icons.sh                       # builds every icon from crates/kymo/assets/kymo.svg (written by scripts/icon-svg.py), then macOS's Kymo.icon layers (macOS, needs Icon Composer)
git tag vX.Y.Z && git push origin vX.Y.Z   # on main after the release PR is merged; release.yml: checks version and changelog, runs CI and binaries.yml, creates a draft release (the user tags and publishes)
gh workflow run binaries.yml            # release binaries as artifacts, not zipped again: a signed, notarized .dmg with Kymo.app (macOS arm64), an installer (Windows), a .tar.gz with install.sh and the desktop files (Linux x86_64 and arm64)
packaging/macos/bundle.sh BINARY VERSION DIR   # builds Kymo.app around a binary (unsigned; the workflow signs it); dmg.sh puts it in a disk image
gh api repos/cbrnr/kymo/actions/runs/RUN/artifacts --jq '.artifacts[] | "\(.id) \(.name)"'   # their ids; then
gh api repos/cbrnr/kymo/actions/artifacts/ID/zip > NAME   # fetches one as is (`gh run download` expects a zip and fails)
```

## Working style

- Every change goes through a pull request, including the maintainer's own and release commits: work on a branch off `main`, never commit or push to `main` directly.
- Work in small, reviewable steps; each ends with passing tests and a commit.
- Keep commit messages short: a subject line of about 72 characters at most saying what changed; details, if needed, in a brief body.
- A user-visible change adds one line to `CHANGELOG.md` under `[UNRELEASED]` (section, imperative sentence, its pull request and author) and updates that heading's date.
- Never add a `Co-Authored-By` line or any other AI attribution, anywhere: commits, PRs, issues, comments.
- Before a larger step, propose a short plan and wait for approval.
- When unsure about a requirement, ask rather than guess.
