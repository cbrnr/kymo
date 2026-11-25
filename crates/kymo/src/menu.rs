//! The menu bar as plain data, built from the keymap. The viewer turns it into native
//! menus (macOS, Windows) or draws it with egui (Linux).

use eframe::egui::Key;

use crate::action::Action;
use crate::keymap::{Chord, Keymap, Platform};

/// Items the system provides on macOS, with their standard behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemItem {
    About,
    Services,
    Hide,
    HideOthers,
    ShowAll,
    Minimize,
    Zoom,
    BringAllToFront,
}

impl SystemItem {
    pub fn label(self) -> &'static str {
        match self {
            SystemItem::About => "About Kymo",
            SystemItem::Services => "Services",
            SystemItem::Hide => "Hide Kymo",
            SystemItem::HideOthers => "Hide Others",
            SystemItem::ShowAll => "Show All",
            SystemItem::Minimize => "Minimize",
            SystemItem::Zoom => "Zoom",
            SystemItem::BringAllToFront => "Bring All to Front",
        }
    }

    /// The system's shortcut, which the menu takes.
    pub fn chord(self) -> Option<Chord> {
        match self {
            SystemItem::Hide => Some(Chord::cmd(Key::H)),
            SystemItem::HideOthers => Some(Chord::alt_cmd(Key::H)),
            SystemItem::Minimize => Some(Chord::cmd(Key::M)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Action {
        action: Action,
        label: &'static str,
        /// The shortcut shown next to the label.
        shortcut: Option<Chord>,
        /// Whether the native menu takes the shortcut as its key equivalent: on macOS, for
        /// shortcuts with ⌘ (all menu shortcuts; bare navigation keys have no menu item).
        attached: bool,
        /// Shown with a checkmark when on (see [`Action::checkable`]).
        checkable: bool,
    },
    System(SystemItem),
    Separator,
    /// File → Open Recent: a submenu of the recordings opened last, from the menu state,
    /// and Clear Menu.
    Recent,
    Submenu {
        title: &'static str,
        items: Vec<Item>,
    },
}

/// The items of `items` and of their submenus, depth first, without the submenus
/// themselves.
pub fn leaves(items: &[Item]) -> Vec<&Item> {
    items
        .iter()
        .flat_map(|item| match item {
            Item::Submenu { items, .. } => leaves(items),
            item => vec![item],
        })
        .collect()
}

/// The role a menu plays for the system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The macOS application menu, titled with the app's name.
    App,
    Plain,
    /// The macOS Window menu, which lists the open windows.
    Window,
    /// The Help menu, which gets the search field on macOS.
    Help,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    pub title: &'static str,
    pub role: Role,
    pub items: Vec<Item>,
}

impl Menu {
    /// The actions in this menu and its submenus.
    pub fn actions(&self) -> impl Iterator<Item = Action> + '_ {
        leaves(&self.items)
            .into_iter()
            .filter_map(|item| match item {
                Item::Action { action, .. } => Some(*action),
                _ => None,
            })
    }
}

fn action(platform: Platform, action: Action) -> Item {
    let shortcut = Keymap::chord(action);
    let label = match (platform, action) {
        (Platform::Mac, Action::Quit) => "Quit Kymo",
        _ => action.label(),
    };
    Item::Action {
        action,
        label,
        shortcut,
        attached: platform.menu_takes_shortcuts() && shortcut.is_some_and(|c| c.command),
        checkable: action.checkable(),
    }
}

/// The menu bar for `platform`, left to right.
pub fn menu_bar(platform: Platform) -> Vec<Menu> {
    let mac = platform == Platform::Mac;
    let a = |act| action(platform, act);
    let mut menus = Vec::new();
    if mac {
        menus.push(Menu {
            title: "Kymo",
            role: Role::App,
            items: vec![
                Item::System(SystemItem::About),
                a(Action::CheckForUpdates),
                Item::Separator,
                a(Action::Settings),
                Item::Separator,
                Item::System(SystemItem::Services),
                Item::Separator,
                Item::System(SystemItem::Hide),
                Item::System(SystemItem::HideOthers),
                Item::System(SystemItem::ShowAll),
                Item::Separator,
                a(Action::Quit),
            ],
        });
    }
    let mut file = vec![
        a(Action::Open),
        Item::Recent,
        a(Action::Close),
        Item::Separator,
        a(Action::RecordingInfo),
        Item::Separator,
        a(Action::LoadAnnotations),
        a(Action::SaveAnnotations),
        a(Action::ExportShownAnnotations),
        Item::Separator,
        a(Action::LoadChannels),
        a(Action::SaveChannels),
    ];
    if !mac {
        file.extend([Item::Separator, a(Action::Quit)]);
    }
    menus.push(Menu {
        title: "File",
        role: Role::Plain,
        items: file,
    });
    let mut edit = vec![
        a(Action::Undo),
        a(Action::Redo),
        Item::Separator,
        a(Action::ToggleEditing),
        a(Action::DeleteAnnotation),
        a(Action::MergeAnnotations),
    ];
    if !mac {
        edit.extend([Item::Separator, a(Action::Settings)]);
    }
    menus.push(Menu {
        title: "Edit",
        role: Role::Plain,
        items: edit,
    });
    menus.push(Menu {
        title: "View",
        role: Role::Plain,
        items: vec![
            a(Action::ToggleSidebar),
            a(Action::ToggleScaleBars),
            a(Action::ToggleMinimap),
            a(Action::ToggleAntialias),
            Item::Separator,
            a(Action::ZoomIn),
            a(Action::ZoomOut),
            a(Action::DefaultWindow),
            a(Action::Fit),
            Item::Separator,
            a(Action::MoreChannels),
            a(Action::FewerChannels),
            Item::Separator,
            a(Action::ScaleUp),
            a(Action::ScaleDown),
            a(Action::ScaleSelectedUp),
            a(Action::ScaleSelectedDown),
            a(Action::AutoScale),
            a(Action::DefaultScale),
            Item::Separator,
            a(Action::ToggleDc),
            Item::Separator,
            Item::Submenu {
                title: "Stim Annotations",
                items: vec![
                    a(Action::StimEveryChange),
                    a(Action::StimFromZero),
                    a(Action::StimIncreasing),
                    Item::Separator,
                    a(Action::ToggleInitialCode),
                ],
            },
        ],
    });
    menus.push(Menu {
        title: "Go",
        role: Role::Plain,
        items: vec![
            a(Action::GoToTime),
            Item::Separator,
            a(Action::ToStart),
            a(Action::ToEnd),
            Item::Separator,
            a(Action::FirstChannels),
            a(Action::LastChannels),
        ],
    });
    if mac {
        menus.push(Menu {
            title: "Window",
            role: Role::Window,
            items: vec![
                Item::System(SystemItem::Minimize),
                Item::System(SystemItem::Zoom),
                Item::Separator,
                Item::System(SystemItem::BringAllToFront),
            ],
        });
    }
    let mut help = vec![a(Action::Shortcuts)];
    if !mac {
        help.extend([
            Item::Separator,
            a(Action::CheckForUpdates),
            a(Action::About),
        ]);
    }
    menus.push(Menu {
        title: "Help",
        role: Role::Help,
        items: help,
    });
    menus
}

/// The chords a native menu bar takes on `platform`: attached item shortcuts and the
/// system items' own. None on Windows and Linux, where egui handles every shortcut.
pub fn taken_chords(platform: Platform) -> Vec<Chord> {
    if !platform.menu_takes_shortcuts() {
        return Vec::new();
    }
    menu_bar(platform)
        .iter()
        .flat_map(|menu| leaves(&menu.items))
        .filter_map(|item| match item {
            Item::Action {
                shortcut: Some(chord),
                attached: true,
                ..
            } => Some(*chord),
            Item::System(system) => system.chord(),
            _ => None,
        })
        .collect()
}
