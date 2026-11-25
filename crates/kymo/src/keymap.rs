//! The one table of key bindings, and the translation of scrolling and pinching.
//!
//! Bindings are written platform-neutrally: ⌘ is Cmd on macOS and Ctrl elsewhere, ⌥ is
//! Alt. With the native menu bar on macOS the system hands a menu item's shortcut to the
//! menu before the window sees it; [`Keymap::egui_action`] leaves those keys alone, so no
//! key acts twice.

use eframe::egui::{Key, Modifiers, MouseWheelUnit, Vec2};

use crate::action::{Action, Gesture};
use crate::menu;

/// Which platform's conventions apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    Mac,
    Windows,
    Linux,
}

impl Platform {
    pub const ALL: [Platform; 3] = [Platform::Mac, Platform::Windows, Platform::Linux];

    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::Mac
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }

    /// Whether menus are native (macOS, Windows) or drawn by egui (Linux).
    pub fn native_menu(self) -> bool {
        self != Platform::Linux
    }

    /// Whether the native menu takes its items' shortcuts before the window sees them: on
    /// macOS only. On Windows that needs a Win32 message hook in the event loop, so the menu
    /// shows the shortcuts and egui handles them, as on Linux.
    pub fn menu_takes_shortcuts(self) -> bool {
        self == Platform::Mac
    }
}

/// A key with modifiers. `command` is ⌘ on macOS and Ctrl elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: Key,
    pub command: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Chord {
    pub const fn key(key: Key) -> Chord {
        Chord {
            key,
            command: false,
            alt: false,
            shift: false,
        }
    }

    pub const fn cmd(key: Key) -> Chord {
        Chord {
            command: true,
            ..Chord::key(key)
        }
    }

    pub const fn alt(key: Key) -> Chord {
        Chord {
            alt: true,
            ..Chord::key(key)
        }
    }

    pub const fn alt_cmd(key: Key) -> Chord {
        Chord {
            alt: true,
            ..Chord::cmd(key)
        }
    }

    pub const fn shift_alt_cmd(key: Key) -> Chord {
        Chord {
            shift: true,
            ..Chord::alt_cmd(key)
        }
    }

    pub const fn shift_cmd(key: Key) -> Chord {
        Chord {
            shift: true,
            ..Chord::cmd(key)
        }
    }

    /// The chord pressed, from egui's key and modifiers; `None` with a modifier no binding
    /// uses (Ctrl on macOS).
    pub fn from_input(platform: Platform, key: Key, modifiers: Modifiers) -> Option<Chord> {
        let (command, other) = match platform {
            Platform::Mac => (modifiers.mac_cmd, modifiers.ctrl),
            Platform::Windows | Platform::Linux => (modifiers.ctrl, modifiers.mac_cmd),
        };
        (!other).then_some(Chord {
            key,
            command,
            alt: modifiers.alt,
            shift: modifiers.shift,
        })
    }

    /// As shown in menus: `⌥⌘←` on macOS, `Ctrl+Alt+Left` elsewhere.
    pub fn display(self, platform: Platform) -> String {
        let mut text = String::new();
        if platform == Platform::Mac {
            for (on, symbol) in [(self.alt, "⌥"), (self.shift, "⇧"), (self.command, "⌘")] {
                if on {
                    text.push_str(symbol);
                }
            }
            text.push_str(match self.key {
                Key::ArrowLeft => "←",
                Key::ArrowRight => "→",
                Key::ArrowUp => "↑",
                Key::ArrowDown => "↓",
                Key::PageUp => "⇞",
                Key::PageDown => "⇟",
                Key::Home => "↖",
                Key::End => "↘",
                Key::Backspace => "⌫",
                Key::Delete => "⌦",
                Key::Escape => "⎋",
                key => key.symbol_or_name(),
            });
        } else {
            for (on, name) in [
                (self.command, "Ctrl+"),
                (self.alt, "Alt+"),
                (self.shift, "Shift+"),
            ] {
                if on {
                    text.push_str(name);
                }
            }
            text.push_str(match self.key {
                Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown => self.key.name(),
                key => key.symbol_or_name(),
            });
        }
        text
    }

    /// Keys whose character needs Shift on some layouts (+ on a US keyboard, / on a German
    /// one); a binding without Shift also takes them with it.
    fn shift_is_layout(self) -> bool {
        matches!(
            self.key,
            Key::Plus | Key::Equals | Key::Minus | Key::Slash | Key::Comma
        )
    }
}

/// Every binding. An action's first chord is the one menus show; the others are
/// alternatives (Home for ⌘←, ⌘= for ⌘+ on keyboards where + needs Shift).
pub const BINDINGS: &[(Action, Chord)] = &[
    (Action::Open, Chord::cmd(Key::O)),
    (Action::Close, Chord::cmd(Key::W)),
    (Action::Quit, Chord::cmd(Key::Q)),
    (Action::SaveAnnotations, Chord::cmd(Key::S)),
    (Action::RecordingInfo, Chord::cmd(Key::I)),
    (Action::Settings, Chord::cmd(Key::Comma)),
    (Action::Shortcuts, Chord::cmd(Key::Slash)),
    (Action::PanLeft, Chord::key(Key::ArrowLeft)),
    (Action::PanRight, Chord::key(Key::ArrowRight)),
    (Action::PageLeft, Chord::alt(Key::ArrowLeft)),
    (Action::PageRight, Chord::alt(Key::ArrowRight)),
    (Action::GoToTime, Chord::cmd(Key::G)),
    (Action::ToStart, Chord::cmd(Key::ArrowLeft)),
    (Action::ToStart, Chord::key(Key::Home)),
    (Action::ToEnd, Chord::cmd(Key::ArrowRight)),
    (Action::ToEnd, Chord::key(Key::End)),
    (Action::ChannelUp, Chord::key(Key::ArrowUp)),
    (Action::ChannelDown, Chord::key(Key::ArrowDown)),
    (Action::ChannelPageUp, Chord::alt(Key::ArrowUp)),
    (Action::ChannelPageUp, Chord::key(Key::PageUp)),
    (Action::ChannelPageDown, Chord::alt(Key::ArrowDown)),
    (Action::ChannelPageDown, Chord::key(Key::PageDown)),
    (Action::FirstChannels, Chord::cmd(Key::ArrowUp)),
    (Action::LastChannels, Chord::cmd(Key::ArrowDown)),
    (Action::ZoomIn, Chord::cmd(Key::Plus)),
    (Action::ZoomIn, Chord::cmd(Key::Equals)),
    (Action::ZoomOut, Chord::cmd(Key::Minus)),
    (Action::DefaultWindow, Chord::cmd(Key::Num0)),
    (Action::Fit, Chord::cmd(Key::Num9)),
    (Action::MoreChannels, Chord::shift_cmd(Key::Plus)),
    (Action::MoreChannels, Chord::shift_cmd(Key::Equals)),
    (Action::FewerChannels, Chord::shift_cmd(Key::Minus)),
    (Action::ScaleUp, Chord::alt_cmd(Key::Plus)),
    (Action::ScaleUp, Chord::alt_cmd(Key::Equals)),
    (Action::ScaleDown, Chord::alt_cmd(Key::Minus)),
    (Action::ScaleSelectedUp, Chord::shift_alt_cmd(Key::Plus)),
    (Action::ScaleSelectedUp, Chord::shift_alt_cmd(Key::Equals)),
    (Action::ScaleSelectedDown, Chord::shift_alt_cmd(Key::Minus)),
    (Action::AutoScale, Chord::cmd(Key::R)),
    (Action::DefaultScale, Chord::alt_cmd(Key::Num0)),
    (Action::ToggleDc, Chord::cmd(Key::D)),
    (Action::ToggleAntialias, Chord::shift_cmd(Key::A)),
    (Action::ToggleEditing, Chord::cmd(Key::E)),
    (Action::Undo, Chord::cmd(Key::Z)),
    (Action::Redo, Chord::shift_cmd(Key::Z)),
    (Action::Redo, Chord::cmd(Key::Y)),
    (Action::DeleteAnnotation, Chord::key(Key::Delete)),
    (Action::DeleteAnnotation, Chord::key(Key::Backspace)),
    (Action::Deselect, Chord::key(Key::Escape)),
    (Action::SelectAllChannels, Chord::cmd(Key::A)),
];

/// Points per line of a mouse wheel.
pub const LINE_POINTS: f32 = 40.0;
/// Scrolling this many points zooms or scales by a factor of 2.
pub const POINTS_PER_DOUBLING: f64 = 200.0;

/// Key bindings and gestures for one platform.
#[derive(Clone, Debug)]
pub struct Keymap {
    platform: Platform,
    /// Chords a native menu takes before egui sees them.
    taken: Vec<Chord>,
}

impl Keymap {
    pub fn new(platform: Platform) -> Keymap {
        Keymap {
            platform,
            taken: menu::taken_chords(platform),
        }
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// The chords of `action`, the one menus show first.
    pub fn chords(action: Action) -> impl Iterator<Item = Chord> {
        BINDINGS
            .iter()
            .filter(move |(a, _)| *a == action)
            .map(|(_, chord)| *chord)
    }

    /// The chord menus show for `action`.
    pub fn chord(action: Action) -> Option<Chord> {
        Keymap::chords(action).next()
    }

    /// The action bound to a key press, whatever delivers it.
    pub fn action(&self, key: Key, modifiers: Modifiers) -> Option<Action> {
        let chord = Chord::from_input(self.platform, key, modifiers)?;
        let lookup = |chord: Chord| BINDINGS.iter().find(|(_, c)| *c == chord).map(|(a, _)| *a);
        lookup(chord).or_else(|| {
            (chord.shift && chord.shift_is_layout())
                .then(|| {
                    lookup(Chord {
                        shift: false,
                        ..chord
                    })
                })
                .flatten()
        })
    }

    /// The action for a key press that egui receives: `None` for keys a native menu takes.
    pub fn egui_action(&self, key: Key, modifiers: Modifiers) -> Option<Action> {
        let chord = Chord::from_input(self.platform, key, modifiers)?;
        if self.taken.contains(&chord) {
            return None;
        }
        self.action(key, modifiers)
    }

    /// The gestures of a mouse wheel or trackpad scroll. `page` is the size of a page in
    /// points, for wheels that scroll by pages.
    ///
    /// Plain scrolling pans time (horizontal) and scrolls channels (vertical); Shift turns a
    /// vertical wheel into panning. By vertical amounts, ⌘ zooms time, ⌥ (or ⌥⌘) scales
    /// amplitudes, ⇧⌘ changes the number of rows and ⌥⇧⌘ scales the channel under the
    /// pointer; with ⇧ a horizontal amount counts where there is no vertical one, as macOS
    /// turns a mouse wheel's scrolling sideways while ⇧ is held.
    pub fn wheel(
        &self,
        unit: MouseWheelUnit,
        delta: Vec2,
        modifiers: Modifiers,
        page: Vec2,
    ) -> Vec<Gesture> {
        let delta = match unit {
            MouseWheelUnit::Point => delta,
            MouseWheelUnit::Line => delta * LINE_POINTS,
            MouseWheelUnit::Page => delta * page,
        };
        let (dx, dy) = (f64::from(delta.x), f64::from(delta.y));
        let command = self.command(modifiers);
        let doubling = |points: f64| (points / POINTS_PER_DOUBLING).exp2();
        let mut gestures = Vec::new();
        let either = if dy != 0.0 { dy } else { dx };
        if command && modifiers.shift {
            if either != 0.0 {
                let factor = doubling(either);
                gestures.push(if modifiers.alt {
                    Gesture::ScaleChannel { factor }
                } else {
                    Gesture::Rows { factor }
                });
            }
        } else if command && modifiers.alt {
            if dy != 0.0 {
                gestures.push(Gesture::Scale {
                    factor: doubling(dy),
                });
            }
        } else if command {
            if dy != 0.0 {
                gestures.push(Gesture::Zoom {
                    factor: doubling(dy),
                });
            }
        } else if modifiers.alt {
            if dy != 0.0 {
                gestures.push(Gesture::Scale {
                    factor: doubling(dy),
                });
            }
        } else if modifiers.shift {
            let points = if dx != 0.0 { dx } else { dy };
            if points != 0.0 {
                gestures.push(Gesture::Pan { points });
            }
        } else {
            if dx != 0.0 {
                gestures.push(Gesture::Pan { points: dx });
            }
            if dy != 0.0 {
                gestures.push(Gesture::ScrollRows { points: dy });
            }
        }
        gestures
    }

    /// A pinch by `factor` (above 1 spreads): zooms time, or with the modifiers of
    /// [`wheel`](Self::wheel) scales amplitudes (⌥ or ⌥⌘), changes the number of rows (⇧⌘)
    /// or scales the channel under the pointer (⌥⇧⌘).
    pub fn pinch(&self, factor: f32, modifiers: Modifiers) -> Gesture {
        let factor = f64::from(factor);
        let command = self.command(modifiers);
        match (command, modifiers.alt, modifiers.shift) {
            (true, true, true) => Gesture::ScaleChannel { factor },
            (true, false, true) => Gesture::Rows { factor },
            (_, true, _) => Gesture::Scale { factor },
            _ => Gesture::Zoom { factor },
        }
    }

    /// Whether ⌘ (Ctrl on Windows and Linux) is held.
    pub fn command(&self, modifiers: Modifiers) -> bool {
        match self.platform {
            Platform::Mac => modifiers.mac_cmd,
            Platform::Windows | Platform::Linux => modifiers.ctrl,
        }
    }

    /// The modifiers held for scaling a single channel, as the platform writes them:
    /// "⌥⇧⌘", or "Ctrl+Alt+Shift".
    pub fn channel_scale_keys(&self) -> &'static str {
        match self.platform {
            Platform::Mac => "\u{2325}\u{21e7}\u{2318}",
            Platform::Windows | Platform::Linux => "Ctrl+Alt+Shift",
        }
    }
}
