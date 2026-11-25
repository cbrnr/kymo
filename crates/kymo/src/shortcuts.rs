//! The shortcut overview (⌘/) as data: every key binding and gesture, grouped as the
//! menus are, built from the same tables so it can't drift from them.

use crate::action::Action;
use crate::keymap::{BINDINGS, Keymap, Platform};
use crate::menu::{Role, menu_bar};

/// One line: the keys (or gesture) and what they do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub keys: String,
    pub action: &'static str,
}

/// A titled group of rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub title: &'static str,
    pub rows: Vec<Row>,
}

/// The overview for `platform`: trackpad and mouse first, then each menu's actions with
/// keys, then the keys without a menu item.
pub fn overview(platform: Platform) -> Vec<Section> {
    let mut sections = vec![Section {
        title: "Trackpad and Mouse",
        rows: gestures(platform),
    }];
    let mut listed = Vec::new();
    for menu in menu_bar(platform) {
        let title = match menu.role {
            Role::App => "Kymo",
            _ => menu.title,
        };
        // Submenus' items too.
        let rows: Vec<Row> = menu
            .actions()
            .filter_map(|action| row(platform, action))
            .collect();
        listed.extend(menu.actions());
        if !rows.is_empty() {
            sections.push(Section { title, rows });
        }
    }
    let mut others: Vec<Action> = Vec::new();
    for (action, _) in BINDINGS {
        if !listed.contains(action) && !others.contains(action) {
            others.push(*action);
        }
    }
    sections.push(Section {
        title: "Other Keys",
        rows: others
            .into_iter()
            .filter_map(|a| row(platform, a))
            .collect(),
    });
    sections
}

/// `action` with all its keys, the one menus show first; `None` without keys.
fn row(platform: Platform, action: Action) -> Option<Row> {
    let keys: Vec<String> = Keymap::chords(action)
        .map(|c| c.display(platform))
        .collect();
    (!keys.is_empty()).then(|| Row {
        keys: keys.join(" or "),
        action: action.label(),
    })
}

/// The trackpad and mouse gestures, a row per action with every way to do it, with the
/// platform's modifier names.
fn gestures(platform: Platform) -> Vec<Row> {
    let (command, alt, shift) = match platform {
        Platform::Mac => ("⌘", "⌥", "⇧"),
        Platform::Windows | Platform::Linux => ("Ctrl", "Alt", "Shift"),
    };
    // Modifiers together, in the platform's order: ⌥⇧⌘, or Ctrl+Alt+Shift.
    let (alt_command, shift_command, all) = match platform {
        Platform::Mac => ("⌥⌘", "⇧⌘", "⌥⇧⌘"),
        Platform::Windows | Platform::Linux => ("Ctrl+Alt", "Ctrl+Shift", "Ctrl+Alt+Shift"),
    };
    let row = |action, keys: String| Row { keys, action };
    vec![
        row(
            "Pan Time",
            format!("Scroll sideways, {shift} + wheel, or drag the traces"),
        ),
        row("Scroll Channels", "Scroll up or down".into()),
        row(
            "Zoom Time Around the Pointer",
            format!("Pinch or {command} + scroll"),
        ),
        row(
            "More or Fewer Channels",
            format!("{shift_command} + scroll or pinch"),
        ),
        row(
            "Scale Amplitudes",
            format!("{alt_command} or {alt} + scroll or pinch"),
        ),
        row(
            "Scale One Channel (or the Selected Ones)",
            format!("{all} + scroll or pinch over its trace"),
        ),
        row(
            "Measure Time and Values",
            format!("{shift} + drag the traces"),
        ),
        row(
            "Create an Annotation",
            "Drag the traces in annotation mode".into(),
        ),
        row("Select an Annotation", "Click it in annotation mode".into()),
        row("Move the Selected Annotation", "Drag it".into()),
        row(
            "Resize the Selected Annotation",
            "Drag one of its edges".into(),
        ),
        row(
            "Mark a Channel Bad or Good",
            "Click its name beside the traces".into(),
        ),
        row("Go to a Channel", "Click its name in the sidebar".into()),
        row("Move the Window", "Click or drag the minimap".into()),
        row("Resize the Minimap", "Drag its top edge".into()),
        row("Minimap Options", "Right-click the minimap".into()),
        row(
            "Collapse or Expand All Groups",
            format!("{alt} + click a group's arrow in the sidebar"),
        ),
    ]
}
