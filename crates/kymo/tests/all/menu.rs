//! The menu description: complete, unique shortcuts, and a clean split between the keys a
//! native menu takes and the ones egui handles.

use std::collections::{HashMap, HashSet};

use eframe::egui::Modifiers;
use kymo::action::Action;
use kymo::keymap::{BINDINGS, Chord, Keymap, Platform};
use kymo::menu::{Item, Role, SystemItem, leaves, menu_bar, taken_chords};

fn modifiers(platform: Platform, chord: Chord) -> Modifiers {
    let mac = platform == Platform::Mac;
    Modifiers {
        alt: chord.alt,
        ctrl: chord.command && !mac,
        shift: chord.shift,
        mac_cmd: chord.command && mac,
        command: chord.command,
    }
}

/// Every item of every menu and submenu, without the submenus themselves.
fn items(platform: Platform) -> Vec<Item> {
    menu_bar(platform)
        .iter()
        .flat_map(|m| leaves(&m.items).into_iter().cloned().collect::<Vec<_>>())
        .collect()
}

fn menu_actions(platform: Platform) -> Vec<Action> {
    menu_bar(platform)
        .iter()
        .flat_map(|m| m.actions().collect::<Vec<_>>())
        .collect()
}

#[test]
fn every_item_has_a_label_and_appears_once() {
    for platform in Platform::ALL {
        let mut seen = HashSet::new();
        for item in items(platform) {
            if let Item::Action { action, label, .. } = item {
                assert!(!label.is_empty());
                assert!(seen.insert(action), "{platform:?}: {action:?} twice");
            }
        }
        for action in [
            Action::Open,
            Action::Close,
            Action::Quit,
            Action::Settings,
            Action::Shortcuts,
            Action::LoadAnnotations,
            Action::SaveAnnotations,
            Action::ExportShownAnnotations,
            Action::LoadChannels,
            Action::SaveChannels,
        ] {
            assert!(seen.contains(&action), "{platform:?}: no {action:?}");
        }
    }
}

#[test]
fn follows_each_platforms_layout() {
    let titles = |p| menu_bar(p).iter().map(|m| m.title).collect::<Vec<_>>();
    assert_eq!(
        titles(Platform::Mac),
        ["Kymo", "File", "Edit", "View", "Go", "Window", "Help"]
    );
    assert_eq!(
        titles(Platform::Windows),
        ["File", "Edit", "View", "Go", "Help"]
    );
    assert_eq!(titles(Platform::Linux), titles(Platform::Windows));

    let mac = menu_bar(Platform::Mac);
    assert_eq!(mac[0].role, Role::App);
    assert_eq!(
        mac[0].actions().collect::<Vec<_>>(),
        [Action::CheckForUpdates, Action::Settings, Action::Quit]
    );
    assert!(mac[0].items.contains(&Item::System(SystemItem::About)));
    assert_eq!(
        mac[2].actions().collect::<Vec<_>>(),
        [
            Action::Undo,
            Action::Redo,
            Action::ToggleEditing,
            Action::DeleteAnnotation,
            Action::MergeAnnotations,
            Action::SelectAllChannels,
            Action::SelectBadChannels,
            Action::DeselectChannels
        ]
    );
    assert_eq!(mac[5].role, Role::Window);
    assert_eq!(mac[6].role, Role::Help);
    assert!(!menu_actions(Platform::Mac).contains(&Action::About));

    let windows = menu_bar(Platform::Windows);
    assert_eq!(
        windows[1].actions().collect::<Vec<_>>(),
        [
            Action::Undo,
            Action::Redo,
            Action::ToggleEditing,
            Action::DeleteAnnotation,
            Action::MergeAnnotations,
            Action::SelectAllChannels,
            Action::SelectBadChannels,
            Action::DeselectChannels,
            Action::Settings
        ]
    );
    assert!(windows[0].actions().any(|a| a == Action::Quit));
    assert!(menu_actions(Platform::Windows).contains(&Action::About));
    assert_eq!(
        windows[4].actions().collect::<Vec<_>>(),
        [Action::Shortcuts, Action::CheckForUpdates, Action::About]
    );
}

#[test]
fn view_has_the_stim_annotations_submenu() {
    for platform in Platform::ALL {
        let view = menu_bar(platform)
            .into_iter()
            .find(|m| m.title == "View")
            .unwrap();
        let Some(Item::Submenu { title, items }) = view.items.last() else {
            panic!("{platform:?}: no submenu at the end of View");
        };
        assert_eq!(*title, "Stim Annotations");
        let actions: Vec<Option<Action>> = items
            .iter()
            .map(|item| match item {
                Item::Action { action, .. } => Some(*action),
                _ => None,
            })
            .collect();
        assert_eq!(
            actions,
            [
                Some(Action::StimEveryChange),
                Some(Action::StimFromZero),
                Some(Action::StimIncreasing),
                None,
                Some(Action::ToggleInitialCode),
            ]
        );
        assert_eq!(items[3], Item::Separator);
        // Menu::actions sees into it.
        assert!(view.actions().any(|a| a == Action::ToggleInitialCode));
    }
}

#[test]
fn every_action_is_reachable() {
    for action in Action::ALL {
        let bound = Keymap::chord(action).is_some();
        let in_menu = Platform::ALL
            .iter()
            .any(|&p| menu_actions(p).contains(&action));
        // Open Recent's items come from the list of recordings.
        let recent = matches!(action, Action::OpenRecent(_) | Action::ClearRecent);
        assert!(bound || in_menu || recent, "{action:?}");
    }
}

#[test]
fn open_recent_follows_open_in_the_file_menu() {
    for platform in Platform::ALL {
        let file = menu_bar(platform)
            .into_iter()
            .find(|m| m.title == "File")
            .unwrap();
        let open = file
            .items
            .iter()
            .position(|i| {
                matches!(
                    i,
                    Item::Action {
                        action: Action::Open,
                        ..
                    }
                )
            })
            .unwrap();
        assert_eq!(file.items[open + 1], Item::Recent, "{platform:?}");
    }
}

#[test]
fn shortcuts_are_unique_per_platform() {
    for platform in Platform::ALL {
        let mut owner: HashMap<Chord, String> = HashMap::new();
        let mut claim = |chord: Chord, who: String| {
            if let Some(previous) = owner.insert(chord, who.clone()) {
                assert_eq!(previous, who, "{platform:?}: {chord:?} bound twice");
            }
        };
        for (action, chord) in BINDINGS {
            claim(*chord, format!("{action:?}"));
        }
        for item in items(platform) {
            match item {
                Item::Action {
                    action,
                    shortcut: Some(chord),
                    ..
                } => claim(chord, format!("{action:?}")),
                Item::System(system) if platform == Platform::Mac => {
                    if let Some(chord) = system.chord() {
                        claim(chord, format!("{system:?}"));
                    }
                }
                _ => {}
            }
        }
    }
}

#[test]
fn menu_and_egui_split_the_keys_exactly() {
    for platform in Platform::ALL {
        let keymap = Keymap::new(platform);
        let taken = taken_chords(platform);
        for (action, chord) in BINDINGS {
            let m = modifiers(platform, *chord);
            assert_eq!(keymap.action(chord.key, m), Some(*action));
            let by_menu = taken.contains(chord);
            let by_egui = keymap.egui_action(chord.key, m).is_some();
            assert!(by_menu != by_egui, "{platform:?}: {action:?} {chord:?}");
            if by_menu {
                // The menu item that takes it has the same action.
                let owner = items(platform).into_iter().find_map(|item| match item {
                    Item::Action {
                        action,
                        shortcut: Some(c),
                        attached: true,
                        ..
                    } if c == *chord => Some(action),
                    _ => None,
                });
                assert_eq!(owner, Some(*action), "{platform:?}: {chord:?}");
            }
        }
        if !platform.menu_takes_shortcuts() {
            assert!(taken.is_empty());
        }
    }
}

#[test]
fn only_command_shortcuts_are_attached() {
    for platform in Platform::ALL {
        for item in items(platform) {
            if let Item::Action {
                action,
                shortcut,
                attached,
                checkable,
                ..
            } = item
            {
                assert_eq!(shortcut, Keymap::chord(action), "{action:?}");
                let command = shortcut.is_some_and(|c| c.command);
                assert_eq!(
                    attached,
                    command && platform == Platform::Mac,
                    "{platform:?} {action:?}"
                );
                assert_eq!(
                    checkable,
                    matches!(
                        action,
                        Action::ToggleDc
                            | Action::ToggleSidebar
                            | Action::ToggleScaleBars
                            | Action::ToggleMinimap
                            | Action::ToggleAntialias
                            | Action::StimEveryChange
                            | Action::StimFromZero
                            | Action::StimIncreasing
                            | Action::ToggleInitialCode
                            | Action::ToggleEditing
                    ),
                    "{action:?}"
                );
            }
        }
    }
}

#[test]
fn grays_out_what_needs_a_recording() {
    for platform in Platform::ALL {
        let enabled: HashSet<Action> = menu_actions(platform)
            .into_iter()
            .filter(|a| a.enabled(false))
            .collect();
        let mut expected: HashSet<Action> = [
            Action::Open,
            Action::Quit,
            Action::Settings,
            Action::Shortcuts,
            Action::CheckForUpdates,
            Action::ToggleDc,
            Action::ToggleScaleBars,
            Action::ToggleMinimap,
            Action::ToggleAntialias,
        ]
        .into();
        if platform != Platform::Mac {
            expected.insert(Action::About);
        }
        assert_eq!(enabled, expected, "{platform:?}");
        assert!(menu_actions(platform).iter().all(|a| a.enabled(true)));
    }
}

#[test]
fn every_command_shortcut_has_a_menu_item() {
    // Native menus show these and flash when they are pressed.
    for platform in Platform::ALL {
        let actions = menu_actions(platform);
        for action in Action::ALL {
            if Keymap::chord(action).is_some_and(|c| c.command) {
                assert!(actions.contains(&action), "{platform:?}: {action:?}");
            }
        }
    }
}

#[test]
fn the_macos_menu_takes_the_system_shortcuts() {
    let taken = taken_chords(Platform::Mac);
    for chord in [
        Chord::cmd(eframe::egui::Key::H),
        Chord::alt_cmd(eframe::egui::Key::H),
        Chord::cmd(eframe::egui::Key::M),
    ] {
        assert!(taken.contains(&chord), "{chord:?}");
    }
    assert!(!taken_chords(Platform::Windows).contains(&Chord::cmd(eframe::egui::Key::H)));
}
