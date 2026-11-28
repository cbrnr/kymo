//! The viewer: what is shown and how it moves, the annotation overlay and stim annotations,
//! the key bindings, the menu bar as data, the trace renderer, the settings, the command
//! line, and the window built from them. The binary (`main.rs`) only dispatches the
//! command.

pub mod action;
pub mod annotations;
pub mod app;
pub mod app_mode;
pub mod cli;
pub mod disk_cache;
pub mod document;
pub mod history;
pub mod keymap;
pub mod measure;
pub mod menu;
pub mod motion;
#[cfg(any(target_os = "macos", windows))]
pub mod native_menu;
#[cfg(not(any(target_os = "macos", windows)))]
#[path = "no_native_menu.rs"]
pub mod native_menu;
pub mod opened;
pub mod recent;
pub mod render;
pub mod scale;
pub mod scale_bar;
pub mod settings;
pub mod shortcuts;
pub mod stim_annotations;
pub mod types;
pub mod ui;
pub mod update;
pub mod view;
pub mod viewer;
