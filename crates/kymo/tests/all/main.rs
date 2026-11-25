//! Integration tests of the viewer, one module per area.
//!
//! One binary for all of them, so they link once and run in parallel.

// Test code: panicking on unexpected results is the point.
#![allow(clippy::unwrap_used)]

mod common;
mod kinds;

mod annotations;
mod app_mode;
mod axis;
mod frame;
mod history;
mod icon;
mod keymap;
mod measure;
mod menu;
mod minimap;
mod native_menu;
mod opened;
mod palette;
mod readout;
mod render;
mod scale;
mod scale_bar;
mod settings;
mod shortcuts;
mod stim_annotations;
mod theme;
mod types;
mod ui;
mod view;
mod widgets;
