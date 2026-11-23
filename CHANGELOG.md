# Changelog

## [UNRELEASED] · 2026-11-23

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
