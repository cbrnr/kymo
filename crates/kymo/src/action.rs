//! What the user can ask for: discrete actions (keys, menu items) and continuous gestures
//! (scrolling, pinching).

use kymo_core::stim::{EventOptions, EventRule};

/// A command from a key or a menu item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    Open,
    /// Opens the recording at this index in File → Open Recent (see [`crate::recent`]).
    OpenRecent(u8),
    /// Empties File → Open Recent.
    ClearRecent,
    Close,
    Quit,
    /// Adds the annotations of a file (an events table or MNE's CSV).
    LoadAnnotations,
    /// Saves the file's and the viewer's annotations as an events table.
    SaveAnnotations,
    /// Saves the annotations of the descriptions shown, stim ones included.
    ExportShownAnnotations,
    /// Takes channel types and bad channels from a BIDS `channels.tsv`.
    LoadChannels,
    /// Saves channel types and bad channels as a BIDS `channels.tsv`.
    SaveChannels,
    /// Shows or hides the Recording Info window: everything `--info` prints.
    RecordingInfo,
    Settings,
    Shortcuts,
    /// An About window; macOS uses the system's About panel instead.
    About,
    /// Asks GitHub for the latest release and tells whether it is newer (see
    /// [`crate::update`]).
    CheckForUpdates,
    /// A quarter window back or forward in time.
    PanLeft,
    PanRight,
    /// A whole window back or forward.
    PageLeft,
    PageRight,
    ToStart,
    ToEnd,
    /// Asks for a time and moves the window to start there.
    GoToTime,
    ZoomIn,
    ZoomOut,
    /// 20 s from the current start.
    DefaultWindow,
    /// The whole recording.
    Fit,
    ChannelUp,
    ChannelDown,
    ChannelPageUp,
    ChannelPageDown,
    FirstChannels,
    LastChannels,
    MoreChannels,
    FewerChannels,
    /// Taller traces: a smaller amplitude per channel spacing.
    ScaleUp,
    ScaleDown,
    /// Taller traces for the selected channels alone (see [`crate::view::ViewState::scale_channels`]).
    ScaleSelectedUp,
    ScaleSelectedDown,
    /// Fits each channel type's amplitude to what the window shows (see [`crate::scale`]),
    /// once the window is complete.
    AutoScale,
    /// The amplitudes a recording opens with, per type.
    DefaultScale,
    ToggleDc,
    /// Shows or hides the sidebar (file header and channel list).
    ToggleSidebar,
    /// Shows or hides the scale bars beside the traces (a setting, on by default).
    ToggleScaleBars,
    /// Shows or hides the minimap below the traces, in place of the time scrollbar (a
    /// setting, off by default).
    ToggleMinimap,
    /// Turns anti-aliased traces on or off (a setting, off by default).
    ToggleAntialias,
    /// Stim events at every change to a non-zero code (see [`EventRule`]).
    StimEveryChange,
    /// Stim annotations only at changes from 0.
    StimFromZero,
    /// Stim annotations only at changes to a larger code.
    StimIncreasing,
    /// Whether a code at a stim channel's first sample is annotated.
    ToggleInitialCode,
    /// Turns annotation mode on or off: dragging in the traces creates annotations
    /// instead of panning, and clicking selects them.
    ToggleEditing,
    /// Undoes the last annotation edit, or the last typing in a text field with the
    /// keyboard.
    Undo,
    Redo,
    /// Removes the selected annotation.
    DeleteAnnotation,
    /// Merges the shown annotations of a kind that overlap or touch.
    MergeAnnotations,
    /// Clears the annotation selection.
    Deselect,
}

impl Action {
    pub const ALL: [Action; 55] = [
        Action::Open,
        Action::OpenRecent(0),
        Action::ClearRecent,
        Action::Close,
        Action::Quit,
        Action::LoadAnnotations,
        Action::SaveAnnotations,
        Action::ExportShownAnnotations,
        Action::LoadChannels,
        Action::SaveChannels,
        Action::RecordingInfo,
        Action::Settings,
        Action::Shortcuts,
        Action::About,
        Action::CheckForUpdates,
        Action::PanLeft,
        Action::PanRight,
        Action::PageLeft,
        Action::PageRight,
        Action::ToStart,
        Action::ToEnd,
        Action::GoToTime,
        Action::ZoomIn,
        Action::ZoomOut,
        Action::DefaultWindow,
        Action::Fit,
        Action::ChannelUp,
        Action::ChannelDown,
        Action::ChannelPageUp,
        Action::ChannelPageDown,
        Action::FirstChannels,
        Action::LastChannels,
        Action::MoreChannels,
        Action::FewerChannels,
        Action::ScaleUp,
        Action::ScaleDown,
        Action::ScaleSelectedUp,
        Action::ScaleSelectedDown,
        Action::AutoScale,
        Action::DefaultScale,
        Action::ToggleDc,
        Action::ToggleSidebar,
        Action::ToggleScaleBars,
        Action::ToggleMinimap,
        Action::ToggleAntialias,
        Action::StimEveryChange,
        Action::StimFromZero,
        Action::StimIncreasing,
        Action::ToggleInitialCode,
        Action::ToggleEditing,
        Action::Undo,
        Action::Redo,
        Action::DeleteAnnotation,
        Action::MergeAnnotations,
        Action::Deselect,
    ];

    /// The name in menus and the shortcut overview.
    pub fn label(self) -> &'static str {
        match self {
            Action::Open => "Open…",
            // Menus show the recording's name instead.
            Action::OpenRecent(_) => "Open Recent",
            Action::ClearRecent => "Clear Menu",
            Action::Close => "Close",
            Action::Quit => "Quit",
            Action::LoadAnnotations => "Load Annotations…",
            Action::SaveAnnotations => "Save Annotations…",
            Action::ExportShownAnnotations => "Export Shown Annotations…",
            Action::LoadChannels => "Load Channels…",
            Action::SaveChannels => "Save Channels…",
            Action::RecordingInfo => "Recording Info…",
            Action::Settings => "Settings…",
            Action::Shortcuts => "Keyboard Shortcuts",
            Action::About => "About Kymo",
            Action::CheckForUpdates => "Check for Updates…",
            Action::PanLeft => "Back a Quarter Window",
            Action::PanRight => "Forward a Quarter Window",
            Action::PageLeft => "Back a Window",
            Action::PageRight => "Forward a Window",
            Action::ToStart => "Start of Recording",
            Action::ToEnd => "End of Recording",
            Action::GoToTime => "Go to Time…",
            Action::ZoomIn => "Zoom In",
            Action::ZoomOut => "Zoom Out",
            Action::DefaultWindow => "Default Window",
            Action::Fit => "Fit Recording",
            Action::ChannelUp => "Channel Up",
            Action::ChannelDown => "Channel Down",
            Action::ChannelPageUp => "Page of Channels Up",
            Action::ChannelPageDown => "Page of Channels Down",
            Action::FirstChannels => "First Channels",
            Action::LastChannels => "Last Channels",
            Action::MoreChannels => "More Channels",
            Action::FewerChannels => "Fewer Channels",
            Action::ScaleUp => "Scale Up",
            Action::ScaleDown => "Scale Down",
            Action::ScaleSelectedUp => "Scale Selected Channels Up",
            Action::ScaleSelectedDown => "Scale Selected Channels Down",
            Action::AutoScale => "Auto Scale",
            Action::DefaultScale => "Default Scale",
            Action::ToggleDc => "Remove DC",
            Action::ToggleSidebar => "Show Sidebar",
            Action::ToggleScaleBars => "Show Scale Bars",
            Action::ToggleMinimap => "Show Minimap",
            Action::ToggleAntialias => "Anti-Aliasing",
            Action::StimEveryChange => "Every Change",
            Action::StimFromZero => "From Zero",
            Action::StimIncreasing => "Increasing",
            Action::ToggleInitialCode => "Initial Code",
            Action::ToggleEditing => "Edit Annotations",
            Action::Undo => "Undo",
            Action::Redo => "Redo",
            Action::DeleteAnnotation => "Delete",
            Action::MergeAnnotations => "Merge Annotations",
            Action::Deselect => "Deselect",
        }
    }

    /// Whether menus show the action with a checkmark when it is on.
    pub fn checkable(self) -> bool {
        matches!(
            self,
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
        )
    }

    /// Whether the checkable action is one choice of several that exclude each other (the
    /// stim annotation rules), shown with a radio button in the egui menus. Native menus
    /// keep the checkmark: macOS marks such choices that way too, and muda has no radio
    /// items for Windows.
    pub fn radio(self) -> bool {
        matches!(
            self,
            Action::StimEveryChange | Action::StimFromZero | Action::StimIncreasing
        )
    }

    /// The stim annotation options after this action, if it is one of the Stim Annotations
    /// items.
    pub fn stim_options(self, options: EventOptions) -> Option<EventOptions> {
        let rule = |rule| EventOptions { rule, ..options };
        match self {
            Action::StimEveryChange => Some(rule(EventRule::EveryChange)),
            Action::StimFromZero => Some(rule(EventRule::FromZero)),
            Action::StimIncreasing => Some(rule(EventRule::Increasing)),
            Action::ToggleInitialCode => Some(EventOptions {
                initial_event: !options.initial_event,
                ..options
            }),
            _ => None,
        }
    }

    /// Whether the action is on under the stim annotation `options`, if it is one of their
    /// items.
    pub fn stim_checked(self, options: EventOptions) -> Option<bool> {
        match self {
            Action::StimEveryChange => Some(options.rule == EventRule::EveryChange),
            Action::StimFromZero => Some(options.rule == EventRule::FromZero),
            Action::StimIncreasing => Some(options.rule == EventRule::Increasing),
            Action::ToggleInitialCode => Some(options.initial_event),
            _ => None,
        }
    }

    /// Whether the action needs an open recording; menu items for it are grayed out
    /// without one.
    pub fn needs_recording(self) -> bool {
        !matches!(
            self,
            Action::Open
                | Action::OpenRecent(_)
                | Action::ClearRecent
                | Action::Quit
                | Action::Settings
                | Action::Shortcuts
                | Action::About
                | Action::CheckForUpdates
                | Action::ToggleDc
                | Action::ToggleScaleBars
                | Action::ToggleMinimap
                | Action::ToggleAntialias
        )
    }

    pub fn enabled(self, has_recording: bool) -> bool {
        has_recording || !self.needs_recording()
    }
}

/// A continuous input, already in the view's terms. Distances are in points (logical
/// pixels) and follow the content: positive moves it right or down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    /// Moves the traces right by `points`, showing earlier times.
    Pan { points: f64 },
    /// Moves the channel rows down by `points`, showing earlier channels.
    ScrollRows { points: f64 },
    /// Magnifies time by `factor` (above 1 zooms in) around the pointer.
    Zoom { factor: f64 },
    /// Makes traces `factor` times taller.
    Scale { factor: f64 },
    /// Shows `factor` times fewer rows (above 1 magnifies them).
    Rows { factor: f64 },
    /// Makes the trace under the pointer `factor` times taller, or the selected channels'
    /// where it is one of them.
    ScaleChannel { factor: f64 },
}
