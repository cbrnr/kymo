# Contributing to Kymo

## Toolchain

Kymo builds with stable Rust, 1.95 or newer, installed through [rustup](https://rustup.rs). The default rustup profile includes `rustfmt` and `clippy`, which the checks below need. Every platform also needs a C compiler, for the BLAKE3 crate's build script.

### macOS

1. Install the Xcode Command Line Tools, which provide the C compiler, the linker and git: `xcode-select --install`
2. Install rustup, either way:
   - with the official script: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
   - with Homebrew: `brew install rustup`, then follow its caveats to put it on your `PATH`
3. Install the stable toolchain and make it the default: `rustup default stable`. The script already does this, so after it the command only confirms it.

The viewer draws with Metal, which every supported Mac has.

### Windows

1. Install the [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the "Desktop development with C++" workload, which provides the MSVC compiler and linker. The rustup installer in step 2 offers to install them if they're missing.
2. Install rustup and the stable toolchain with [rustup-init.exe](https://rustup.rs), using the default `x86_64-pc-windows-msvc` host.

The viewer draws with DX12 or Vulkan, so a current graphics driver is enough.

### Linux

1. Install a C compiler and a Vulkan driver. The viewer draws with Vulkan, and the renderer's tests need a driver too: on a machine without a GPU, Mesa's software driver (lavapipe) works.
   - Arch: `sudo pacman -S base-devel` and your GPU's Vulkan driver (`vulkan-radeon`, `vulkan-intel` or `nvidia-utils`), or `vulkan-swrast` without a GPU
   - Debian and Ubuntu: `sudo apt install build-essential mesa-vulkan-drivers`
   - Fedora: `sudo dnf install gcc mesa-vulkan-drivers`
2. Install rustup, either way:
   - with the official script: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
   - with your package manager: `sudo pacman -S rustup` on Arch, `sudo apt install rustup` on Debian 13 and Ubuntu 24.04 or newer, `sudo dnf install rustup` on Fedora, followed there by `rustup-init`
3. Install the stable toolchain and make it the default: `rustup default stable`. The script and Fedora's `rustup-init` already do this, so after them the command only confirms it.

The build doesn't need any other development packages: X11, Wayland and fontconfig are loaded at run time, and file dialogs, the theme and the accent color go through the desktop portal.

## Build and test

```
cargo build --workspace
cargo test --workspace
cargo run -p kymo --release -- file.bdf
```

`cargo run -p kymo-testgen -- --help` lists the synthetic EDF/BDF, BrainVision, EEGLAB, GDF, FIFF, XDF and BVRF files the generator can write, if you need a recording to open.

Without a GPU, the renderer's tests pass and print that they were skipped. Set `KYMO_REQUIRE_GPU=1` to make them fail instead, as CI does on Linux.

Some tests need real recordings. These are kept outside the repository: point `KYMO_TEST_DATA` to them and run `cargo test --workspace -- --ignored`. `scripts/compare_mne.py` checks Kymo's reader against MNE-Python and needs [uv](https://docs.astral.sh/uv/).

## Before you commit

CI runs these on Linux, macOS and Windows, and all three must be clean:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

If your change is visible to users, add a line for it to `CHANGELOG.md` under `[UNRELEASED]`, in the section that fits (✨ Added, 🌀 Changed, 🗑️ Removed, 🔧 Fixed): one sentence starting with a verb in the imperative, followed by your pull request and name, for example `([#12](https://github.com/cbrnr/kymo/pull/12) by [Clemens Brunner](https://github.com/cbrnr))`.

Read the architecture overview below before changing anything architectural, and `CLAUDE.md` for the project's rules and conventions.

## Architecture

Kymo's frame cost and memory depend on the screen, never on the size of the file. The workspace has three crates: `kymo-core`, the library (readers, recording model, pyramid, tiles; no GUI dependencies), `kymo`, the viewer built on it with egui and wgpu, and `kymo-testgen`, which writes synthetic recordings for tests and benchmarks.

- **Readers** (`kymo-core`, a module per format) memory-map the source files read-only and never change them. Each format sits behind the same interface: a header (channels, units, scaling), an index from sample ranges to byte ranges, with gaps, and block reads (`BlockReader`) that return samples in one of three native types, int16, int32 or float32. The format comes from a file's first bytes, never its extension. Physical values are digital × gain + offset, and EDF/BDF offsets can be huge.
- **Recording model** (`Recording`): channels belong to grids (`Grid`), each a group of channels that share a time base, gaps and sample type. A time base is regular, segmented (regular pieces, as XDF's dejittered segments) or irregular (a time per sample, from a memory-mapped table). EDF allows a rate per channel and XDF irregular streams, so nothing above the readers may assume EDF's record layout or a single sampling rate. Gaps stay gaps: they are drawn as empty space, never filled.
- **Pyramid** (`pyramid`): when a recording opens, a background thread reads it once, sequentially, and builds min/max envelopes in the native type: level 1 holds the minimum and maximum of every 32 samples, each level above combines 4 buckets of the one below. The same pass sums blocks of 4,096 samples for DC removal and records the changes of stim channels. The result is kept as a sidecar file in the user's cache directory, named by a hash of the source files, so opening a recording again is instant. The viewer is usable while the pyramid builds.
- **Tiles** (`tile`, `service`): the screen is put together from tiles of 4,096 values per channel on a grid fixed in sample-index space, so bucket k of level L always covers the same samples, panning never moves bucket boundaries, and a tile stays valid once made. Level 0 tiles hold samples, the levels above min/max pairs: zoomed out, each pixel shows the minimum and maximum of all samples it covers, never a subsample, so peaks never disappear. Worker threads (`TileService`) fill an in-memory cache with a byte budget (`TileCache`), nearest to the view first, with prefetching. The UI thread only reads the cache and never waits for the disk or a computation: a missing tile is drawn from the nearest coarser one that is ready.
- **Rendering** (`kymo`): tiles are uploaded once to GPU storage buffers, and a frame is one instanced draw per tile, which fills, per pixel column, the range between the lowest and highest value that touches it. Each channel's scale and offset are computed on the CPU in f64, relative to a native value near the row's baseline, so huge offsets keep sub-pixel precision. Annotations, labels and the rest of the interface are egui.
- **Annotations** (`AnnotationStore`) live beside the recording, which is immutable and shared with the tile workers, in an interval tree; every edit is an operation, with undo. Events of stim channels come from the pyramid's build.

Tests are behavioral: they check outputs (decoded samples, tile values, annotations), not internal structure, and `proptest` compares tiles with a brute-force min/max pass. Test recordings come from `kymo-testgen`; never commit large binary files. Integration tests live in one binary per crate (`tests/all/`, a module per area). Readers follow each format's specification first, then its reference implementation; `scripts/compare_mne.py` checks them against MNE-Python, pyxdf and pybvrf and lists where Kymo differs on purpose. Benchmarks use `criterion` (`cargo bench -p kymo-core`); `cargo bench -p kymo --bench frames -- FILE` measures the viewer's frame times, its own work in each frame and its memory (`--measure` keeps a measurement on screen, `--edit` adds annotation editing).

## Releasing

Releases are made from `main` by pushing a tag; `.github/workflows/release.yml` does the rest up to a draft release.

1. Set the version in the workspace's `Cargo.toml` (`version = "X.Y.Z"`; a pre-release such as `X.Y.Z-rc.1` becomes a GitHub pre-release) and run `cargo build` to update `Cargo.lock`.
2. In `CHANGELOG.md`, rename the `[UNRELEASED]` heading to `[X.Y.Z]` with today's date.
3. Commit both, and tag and push the commit:

   ```sh
   git commit -am "Release X.Y.Z"
   git tag vX.Y.Z
   git push origin main vX.Y.Z
   ```

   The workflow checks that the tag matches the version and that the changelog has an entry for it, runs the tests, builds the macOS disk image (signed and notarized), the Windows installer and the Linux archives, and creates a draft release with them and the changelog's entry as its notes (GitHub shows each asset's SHA-256).
4. Review the draft on the [Releases](https://github.com/cbrnr/kymo/releases) page and publish it.
5. The AUR package follows: [aur-pkgbuild-updater](https://github.com/cbrnr/aur-pkgbuild-updater) opens a pull request with the new version within a few hours; merge it to push the update to the AUR.
6. Add a new `## [UNRELEASED]` heading at the top of `CHANGELOG.md` for the next changes.
