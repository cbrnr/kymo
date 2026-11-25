# Changelog

## [UNRELEASED] · 2026-11-25
### 🌀 Changed
- Select channels by clicking anywhere on their rows, act on the selection from a bar below the channel list, select all, bad, or a type's channels, show "Bad" only on bad channels, as a label rather than a toggle, undo bad marks, and show the channel list's scroll bar whenever it scrolls ([#16](https://github.com/cbrnr/kymo/pull/16) by [Clemens Brunner](https://github.com/cbrnr)).

### 🔧 Fixed
- Stop the channel names column from being dragged wider than its longest name needs ([#15](https://github.com/cbrnr/kymo/pull/15) by [Clemens Brunner](https://github.com/cbrnr)).

## [0.2.0] · 2026-11-25
### ✨ Added
- Add a minimap below the traces (View → Show Minimap) that shows the annotations of the whole recording and where the window is ([#11](https://github.com/cbrnr/kymo/pull/11) by [Clemens Brunner](https://github.com/cbrnr)).
- Add a row per channel to the minimap that shows where the signals are large, flat or missing, with channel annotations in their rows and the window spanning the channels on screen ([#12](https://github.com/cbrnr/kymo/pull/12) by [Clemens Brunner](https://github.com/cbrnr)).

### 🌀 Changed
- Raise the interface's contrast in both themes, draw text closer to how macOS does, and color selections in the system's accent color ([#8](https://github.com/cbrnr/kymo/pull/8) by [Clemens Brunner](https://github.com/cbrnr)).
- Use the system font by default on macOS and Windows ([#8](https://github.com/cbrnr/kymo/pull/8) by [Clemens Brunner](https://github.com/cbrnr)).
- Set the tile cache and GPU memory in Settings from a menu of sizes, and the default window in a field with a stepper ([#9](https://github.com/cbrnr/kymo/pull/9) by [Clemens Brunner](https://github.com/cbrnr)).
- Open dialogs in the middle of the main window, with smaller titles and shadows on all sides ([#10](https://github.com/cbrnr/kymo/pull/10) by [Clemens Brunner](https://github.com/cbrnr)).

### 🔧 Fixed
- Fix the System theme staying dark on macOS after switching between Light and Dark ([#8](https://github.com/cbrnr/kymo/pull/8) by [Clemens Brunner](https://github.com/cbrnr)).

## [0.1.0] · 2026-11-23
### ✨ Added
- Add a viewer for large EEG and MEG recordings that opens them almost instantly, scrolls and zooms smoothly on the GPU, keeps every peak visible, and uses little memory regardless of file size.
- Add support for EDF/EDF+, BDF/BDF+, BrainVision, BVRF, EEGLAB, FIFF, GDF and XDF files, including mixed sampling rates, gaps, XDF streams and BVRF participants.
- Add navigation with trackpad, mouse wheel and keyboard, a value readout on hover, a recent files list, opening recordings dropped on the window or, on macOS, from Finder, a Recording Info window, and `kymo --info` to summarize a recording in the terminal.
- Add channel types with a color, DC removal and scale per type, scaling of single channels, bad channels, and saving and loading channel types and bad channels as a BIDS `channels.tsv` file.
- Add measuring time, frequency and peak-to-peak amplitude by dragging across the traces with ⇧ held down.
- Add annotations and stim channel events, an annotation editing mode with undo and redo, and saving and loading annotations as BIDS events files or MNE-Python annotation CSV files.
- Add light and dark themes, custom colors, native menus and keyboard shortcuts, settings saved between sessions, and clearing the disk cache from Settings.
- Add checking for a newer release on GitHub at startup (at most once a day, can be turned off in Settings) and with Check for Updates….
- Add standalone builds for macOS (Apple silicon, a signed and notarized app in a disk image), Windows (x86_64, an installer) and Linux (x86_64 and arm64, an AUR package and an archive), with an app icon.
