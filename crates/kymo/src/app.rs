//! The window: no recording, one being opened, one on screen, or why it couldn't open.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use eframe::egui::{self, Align, Color32, Event, Layout, RichText, Ui};
use kymo_core::annotation_file::{self, Format};
use kymo_core::channel_file;
use kymo_core::pyramid::Sidecar;
use kymo_core::service::{PyramidStore, ServiceOptions, TileService};
use kymo_core::stim::EventOptions;

use crate::action::Action;
use crate::annotations::AnnotationColors;
use crate::app_mode::AppMode;
use crate::disk_cache::DiskCache;
use crate::document::{self, Document, Loading, error_text, file_name, format_duration, plural};
use crate::keymap::{Keymap, Platform};
use crate::native_menu::NativeMenu;
use crate::opened::Inbox;
use crate::recent::Recent;
use crate::settings::{Settings, Theme, load_annotation_colors, save_annotation_colors};
use crate::ui::Overlay;
use crate::ui::fonts::apply_fonts;
use crate::ui::go_to::{GoToField, go_to_window};
use crate::ui::gpu::Gpu;
use crate::ui::header;
use crate::ui::info::info_window;
use crate::ui::menus::{MenuState, about_window, egui_menu_bar, shortcuts_window};
use crate::ui::settings::settings_window;
use crate::ui::theme;
use crate::ui::types::channel_types_window;
use crate::update::{self, Check, Release, Source, UpdateState};
use crate::viewer::Viewer;

const APP_NAME: &str = "Kymo";

/// The title of the notice that a check for updates failed.
const UPDATE_FAILED: &str = "Could Not Check for Updates";

/// The app's icon, for the window: the title bar and taskbar's on Windows and X11. Wayland
/// takes it from the desktop entry instead; macOS gets [`window_icon`]'s.
pub fn icon() -> egui::IconData {
    // Embedded, so it can only fail if the file in the repository is broken, which a test checks.
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/png/kymo-256.png"))
        .expect("the embedded icon is a valid PNG")
}

/// The icon eframe gives the window, and on macOS the Dock while Kymo runs. In an app bundle
/// that is none: the bundle's own icon adapts to light, dark, tinted and clear mode, which
/// any image set at runtime (eframe would set egui's logo without one) would replace. eframe
/// leaves the Dock alone when the image is invalid, as this one is. A bare binary has no
/// bundle icon and gets one drawn on macOS's icon grid, with its margins, as the Dock expects.
#[cfg(target_os = "macos")]
fn window_icon() -> egui::IconData {
    let bundled = objc2_foundation::NSBundle::mainBundle()
        .bundleIdentifier()
        .is_some();
    if bundled {
        return egui::IconData {
            rgba: Vec::new(),
            width: 1,
            height: 1,
        };
    }
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/png/kymo-macos-256.png"))
        .expect("the embedded icon is a valid PNG")
}

#[cfg(not(target_os = "macos"))]
fn window_icon() -> egui::IconData {
    icon()
}

/// How the window keeps recordings and shows its menus, and the settings it starts with.
#[derive(Clone, Debug)]
pub struct Options {
    /// Where pyramid sidecars go; `None` keeps pyramids in memory.
    pub sidecar_dir: Option<PathBuf>,
    pub menus: Menus,
    pub settings: Settings,
    /// Colors picked for annotation descriptions, as remembered from earlier runs.
    pub annotation_colors: AnnotationColors,
    /// The recordings opened last, for File → Open Recent, as remembered from earlier runs.
    pub recent: Recent,
    /// Where Check for Updates asks; `None` checks nowhere (and says so when asked).
    pub updates: Option<Source>,
    /// When the startup check last ran and the version skipped, from earlier runs.
    pub update_state: UpdateState,
}

/// Where the menu bar comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menus {
    /// The system's, handed over with [`KymoApp::set_native_menu`] (macOS, Windows).
    Native,
    /// Drawn by egui in the window, as on `Platform` (Linux).
    Egui(Platform),
}

impl Default for Options {
    /// Sidecars in the user's cache directory; native menus where the platform has them;
    /// default settings, no annotation colors picked, no recent recordings; no update
    /// checks (`run` asks GitHub).
    fn default() -> Self {
        let platform = Platform::current();
        Options {
            sidecar_dir: Sidecar::default_dir(),
            menus: if platform.native_menu() {
                Menus::Native
            } else {
                Menus::Egui(platform)
            },
            settings: Settings::default(),
            annotation_colors: AnnotationColors::new(),
            recent: Recent::default(),
            updates: None,
            update_state: UpdateState::default(),
        }
    }
}

pub struct KymoApp {
    state: State,
    gpu: Gpu,
    keymap: Keymap,
    options: Options,
    native_menu: Option<NativeMenu>,
    show_sidebar: bool,
    /// How stim codes are annotated, for this recording and the ones opened after it.
    stim_options: EventOptions,
    shortcuts_open: bool,
    about_open: bool,
    settings_open: bool,
    /// The disk cache Settings shows.
    disk_cache: DiskCache,
    /// Whether the Recording Info window is open.
    info_open: bool,
    /// Whether the Channel Types window is open, and what its filter holds.
    channel_types_open: bool,
    channel_types_filter: String,
    /// Go to Time…, and what its field holds.
    go_to_open: bool,
    go_to: GoToField,
    /// Show the file dialog once the window is on screen.
    pick_file: bool,
    /// Whether a frame has been shown yet.
    shown: bool,
    /// The window title last set.
    title: String,
    /// Answers to the next file dialogs, in order, instead of the system's (`None` cancels),
    /// for tests.
    dialog_answers: Option<VecDeque<Option<PathBuf>>>,
    /// What waits for the answer to "Save the changes?".
    leaving: Option<Leave>,
    /// A message in a window: its title and text.
    notice: Option<(String, String)>,
    /// The check for updates under way, and whether it was asked for from the menu (which
    /// reports every answer).
    update_check: Option<(Check, bool)>,
    /// A newer release, offered in a window until it is answered.
    update_offer: Option<Release>,
    /// The window may close: its unsaved changes were saved or given up.
    closing: bool,
    /// The system's app mode where winit can't tell it (Windows, Linux), which System
    /// follows where it is known (see [`AppMode`]).
    app_mode: AppMode,
    /// The theme setting, the system's theme and the app mode it was last applied for.
    theme_applied: Option<(Theme, Option<egui::Theme>, Option<bool>)>,
    /// The accent color the looks were last made with (see [`theme::apply`]); `None` as
    /// the app starts, so the first frame makes them.
    accent_applied: Option<Option<egui::Color32>>,
    /// Whether the window had focus in the last frame: gaining it reads the accent color
    /// again (see [`AppMode::refresh_accent`]).
    focused: bool,
    /// The font setting the fonts were last set for, and whether that was during the last
    /// frame.
    fonts_applied: bool,
    fonts_set: bool,
    /// Files the system hands over (Finder on macOS), once [`receive_opened`](Self::receive_opened)
    /// connects it.
    opened: Option<Arc<Inbox>>,
}

/// What the unsaved changes question holds up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Leave {
    Close,
    /// Open a recording picked in the file dialog.
    Open,
    /// Open this recording (dropped on the window).
    OpenPath(PathBuf),
    Quit,
}

/// A file dialog.
enum Dialog {
    Open,
    /// Where to save the annotations, suggesting a path.
    Save(PathBuf),
    /// Where to export the shown annotations, suggesting a path.
    Export(PathBuf),
    Load,
    /// Where to save the channels, suggesting a path.
    SaveChannels(PathBuf),
    /// Which `channels.tsv` to load, suggesting a path.
    LoadChannels(PathBuf),
}

/// A recording ready to show, as the loading thread hands it over.
type Opened = (Document, TileService);

enum State {
    Empty,
    Loading(Loading<Opened>),
    Ready(Box<Viewer>),
    Failed { path: PathBuf, error: String },
}

impl KymoApp {
    /// A window opening `path`, or showing a file dialog without one.
    pub fn new(ctx: &egui::Context, gpu: Gpu, options: Options, path: Option<PathBuf>) -> Self {
        // ⌘+ and ⌘− zoom time, not the interface. The title bar's theme is ours: egui sets
        // it only when its theme changes, not again when the system's does (see
        // `apply_theme`).
        ctx.options_mut(|o| {
            o.zoom_with_keyboard = false;
            o.sync_window_theme = false;
        });
        let system_font = options.settings.system_font;
        apply_fonts(ctx, system_font);
        gpu.set_budget(options.settings.gpu_memory_bytes());
        let disk_cache = DiskCache::new(options.sidecar_dir.clone());
        let mut app = KymoApp {
            state: State::Empty,
            gpu,
            keymap: Keymap::new(Platform::current()),
            options,
            native_menu: None,
            show_sidebar: true,
            stim_options: EventOptions::default(),
            shortcuts_open: false,
            about_open: false,
            settings_open: false,
            disk_cache,
            info_open: false,
            channel_types_open: false,
            channel_types_filter: String::new(),
            go_to_open: false,
            go_to: GoToField::default(),
            pick_file: path.is_none(),
            shown: false,
            // As the window opens.
            title: APP_NAME.to_owned(),
            dialog_answers: None,
            leaving: None,
            notice: None,
            update_check: None,
            update_offer: None,
            closing: false,
            app_mode: AppMode::new(),
            theme_applied: None,
            accent_applied: None,
            focused: false,
            fonts_applied: system_font,
            fonts_set: false,
            opened: None,
        };
        if let Some(path) = path {
            app.open(ctx, path);
        }
        app
    }

    /// Takes over the native menu bar, already installed: its items act from now on, and
    /// its shortcuts no longer reach egui. Without one, [`Menus::Native`] draws the menu bar
    /// with egui.
    pub fn set_native_menu(&mut self, menu: NativeMenu) {
        self.native_menu = Some(menu);
    }

    /// Whether the native menu bar takes its shortcuts before egui sees them.
    fn menu_takes_keys(&self) -> bool {
        self.native_menu.is_some() && self.keymap.platform().menu_takes_shortcuts()
    }

    /// The platform whose menu bar egui draws, if egui draws one.
    fn egui_menus(&self) -> Option<Platform> {
        match self.options.menus {
            Menus::Egui(platform) => Some(platform),
            Menus::Native if self.native_menu.is_some() => None,
            Menus::Native => Some(self.keymap.platform()),
        }
    }

    /// Whether the sidebar is shown.
    pub fn sidebar_shown(&self) -> bool {
        self.show_sidebar
    }

    /// Whether the shortcut overview is open.
    pub fn shortcuts_open(&self) -> bool {
        self.shortcuts_open
    }

    /// Whether the Settings window is open.
    pub fn settings_open(&self) -> bool {
        self.settings_open
    }

    /// Whether the Recording Info window is open.
    pub fn info_open(&self) -> bool {
        self.info_open
    }

    /// Whether the Channel Types window is open.
    pub fn channel_types_open(&self) -> bool {
        self.channel_types_open
    }

    pub fn settings(&self) -> &Settings {
        &self.options.settings
    }

    /// The GPU the traces are drawn on.
    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// Changes the settings and applies them at once: the tile cache and GPU budgets (a
    /// smaller one evicts), anti-aliasing, and the default window for ⌘0 and recordings
    /// opened from now on; the theme from the next frame.
    pub fn set_settings(&mut self, settings: Settings) {
        let settings = settings.clamped();
        self.options.settings = settings.clone();
        self.gpu.set_budget(settings.gpu_memory_bytes());
        if let State::Ready(viewer) = &mut self.state {
            viewer.cache().set_budget(settings.tile_cache_bytes());
            viewer.set_default_span(settings.default_window_s);
            viewer.set_antialias(settings.antialias);
            viewer.set_scale_bars(settings.scale_bars);
            viewer.set_remove_dc(settings.remove_dc);
            viewer.set_type_settings(settings.types.clone());
        }
    }

    /// Has System follow `mode` (the system's app mode, read on Windows and Linux) instead
    /// of the system's theme as winit reports it, wherever the mode is known, and the
    /// selection the system's accent color.
    pub fn follow_app_mode(&mut self, mode: AppMode) {
        mode.refresh_accent();
        self.app_mode = mode;
    }

    /// Opens the files the system hands over through `inbox` (see [`crate::opened`]) as
    /// dropped ones: a recording replaces the open one, after asking about unsaved changes.
    pub fn receive_opened(&mut self, ctx: &egui::Context, inbox: Arc<Inbox>) {
        inbox.wake(ctx);
        self.opened = Some(inbox);
    }

    /// Applies the theme before the first frame, which the startup file dialog (opened
    /// while the second is built) leaves on screen: otherwise a forced theme, or the app
    /// mode's, would show as egui's default behind it. Call it once the app mode is known
    /// and the native menu attached. Starts the startup check for updates, if it is on and
    /// due.
    pub fn start(&mut self, ctx: &egui::Context) {
        self.apply_theme(ctx);
        if self.options.settings.check_updates
            && self.options.update_state.due(jiff::Timestamp::now())
        {
            self.check_for_updates(ctx, false);
        }
    }

    /// Starts a check for updates (one at a time). `manual` (from the menu) reports every
    /// answer; a startup check only offers a newer release that wasn't skipped.
    fn check_for_updates(&mut self, ctx: &egui::Context, manual: bool) {
        if let Some((_, reported)) = &mut self.update_check {
            *reported |= manual;
            return;
        }
        let Some(source) = &self.options.updates else {
            if manual {
                self.notice = Some((
                    UPDATE_FAILED.into(),
                    "This copy of Kymo has nowhere to check.".into(),
                ));
            }
            return;
        };
        let repaint = ctx.clone();
        let check = Check::start(source, move || repaint.request_repaint());
        self.update_check = Some((check, manual));
    }

    /// Picks up the answer of the check for updates, once it is there.
    fn poll_update(&mut self) {
        let Some((check, manual)) = &self.update_check else {
            return;
        };
        let Some(result) = check.poll() else {
            return;
        };
        let manual = *manual;
        self.update_check = None;
        if result.is_ok() {
            self.options.update_state.last_check = Some(jiff::Timestamp::now());
        }
        match result {
            Ok(Some(release))
                if release.is_newer() && (manual || self.options.update_state.offers(&release)) =>
            {
                self.update_offer = Some(release);
            }
            Ok(_) if manual => {
                self.notice = Some((
                    "Kymo Is Up to Date".into(),
                    format!("Kymo {} is the latest version.", update::current()),
                ));
            }
            Err(error) if manual => {
                self.notice = Some((UPDATE_FAILED.into(), error_text(&error)));
            }
            _ => {}
        }
    }

    /// Whether a check for updates is under way.
    pub fn checking_for_updates(&self) -> bool {
        self.update_check.is_some()
    }

    /// The newer release offered, until the offer is answered.
    pub fn update_offer(&self) -> Option<&Release> {
        self.update_offer.as_ref()
    }

    /// When the startup check last ran and the version skipped.
    pub fn update_state(&self) -> &UpdateState {
        &self.options.update_state
    }

    /// Offers the newer release, if there is one, once no question or notice is shown:
    /// Download opens its page in the browser, Skip This Version offers it no more at
    /// startup, Later (or Esc) leaves it for the next check.
    fn update_window(&mut self, ctx: &egui::Context) {
        let Some(release) = &self.update_offer else {
            return;
        };
        if self.leaving.is_some() || self.notice.is_some() {
            return;
        }
        #[derive(Clone, Copy)]
        enum Answer {
            Download,
            Skip,
            Later,
        }
        let mut answer = None;
        let modal = egui::Modal::new(egui::Id::unique("kymo update")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading("Update Available");
            ui.label(format!(
                "Kymo {} is available; you have {}. Download opens its page on GitHub.",
                release.version,
                update::current()
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Download").clicked() {
                    answer = Some(Answer::Download);
                }
                if ui.button("Skip This Version").clicked() {
                    answer = Some(Answer::Skip);
                }
                if ui.button("Later").clicked() {
                    answer = Some(Answer::Later);
                }
            });
        });
        if answer.is_none() && modal.should_close() {
            answer = Some(Answer::Later);
        }
        match answer {
            Some(Answer::Download) => ctx.open_url(egui::OpenUrl::new_tab(release.url())),
            Some(Answer::Skip) => {
                self.options.update_state.skipped = Some(release.version.clone());
            }
            Some(Answer::Later) | None => {}
        }
        if answer.is_some() {
            self.update_offer = None;
        }
    }

    /// Whether a question, notice or update offer holds the window until it is answered.
    fn asking(&self) -> bool {
        self.leaving.is_some() || self.notice.is_some() || self.update_offer.is_some()
    }

    /// Applies the theme setting to egui, the title bar and the native menus, when it, the
    /// system's theme or the app mode changed. Again on a change of the system's: Windows
    /// puts the title bar back to the system's mode then, whatever the window was set to.
    /// Following an app mode, System is applied as Light or Dark. The light and dark looks
    /// are made again when the accent color changed.
    fn apply_theme(&mut self, ctx: &egui::Context) {
        let accent = self.app_mode.accent();
        if self.accent_applied != Some(accent) {
            self.accent_applied = Some(accent);
            theme::apply(ctx, accent);
        }
        let theme = self.options.settings.theme;
        let app_dark = self.app_mode.dark();
        let now = (theme, ctx.system_theme(), app_dark);
        if self.theme_applied == Some(now) {
            return;
        }
        self.theme_applied = Some(now);
        let shown = match (theme, app_dark) {
            (Theme::System, Some(true)) => Theme::Dark,
            (Theme::System, Some(false)) => Theme::Light,
            (theme, _) => theme,
        };
        ctx.set_theme(shown.preference());
        ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(shown.window()));
        if let Some(menu) = &self.native_menu {
            menu.set_theme(shown);
        }
    }

    /// The colors picked for annotation descriptions.
    pub fn annotation_colors(&self) -> &AnnotationColors {
        &self.options.annotation_colors
    }

    /// Whether the About window is open.
    pub fn about_open(&self) -> bool {
        self.about_open
    }

    fn menu_state(&self, ctx: &egui::Context) -> AppMenuState {
        let viewer = self.viewer();
        AppMenuState {
            ready: matches!(self.state, State::Ready(_)),
            overlay: !self.open_overlays().is_empty(),
            typing: ctx.egui_wants_keyboard_input(),
            editing: viewer.is_some_and(Viewer::editing),
            selected: viewer.is_some_and(|v| v.selected().is_some()),
            channels_selected: viewer.is_some_and(|v| !v.view().selected().is_empty()),
            dragging: viewer.is_some_and(Viewer::dragging),
            measured: viewer.is_some_and(Viewer::measured),
            can_undo: viewer.is_some_and(|v| v.history().can_undo()),
            can_redo: viewer.is_some_and(|v| v.history().can_redo()),
            sidebar: self.show_sidebar,
            antialias: self.options.settings.antialias,
            scale_bars: self.options.settings.scale_bars,
            dc: self.options.settings.remove_dc,
            stim: viewer.is_some_and(Viewer::has_stim),
            shown: viewer.is_some_and(|v| v.legend().shown_count().0 > 0),
            // Not looked for during a drag, which changes the store every frame.
            mergeable: viewer.is_some_and(|v| !v.dragging() && v.can_merge()),
            stim_options: self.stim_options,
            recent: self.options.recent.labels(),
        }
    }

    /// Settings, Keyboard Shortcuts and About, those that are open.
    fn open_overlays(&self) -> Vec<Overlay> {
        Overlay::ALL
            .into_iter()
            .filter(|&overlay| match overlay {
                Overlay::Settings => self.settings_open,
                Overlay::Shortcuts => self.shortcuts_open,
                Overlay::About => self.about_open,
                Overlay::GoToTime => self.go_to_open,
                Overlay::RecordingInfo => self.info_open,
                Overlay::ChannelTypes => self.channel_types_open,
            })
            .collect()
    }

    fn overlay_open(&mut self, overlay: Overlay) -> &mut bool {
        match overlay {
            Overlay::Settings => &mut self.settings_open,
            Overlay::Shortcuts => &mut self.shortcuts_open,
            Overlay::About => &mut self.about_open,
            Overlay::GoToTime => &mut self.go_to_open,
            Overlay::RecordingInfo => &mut self.info_open,
            Overlay::ChannelTypes => &mut self.channel_types_open,
        }
    }

    /// The recording on screen, if there is one.
    pub fn viewer(&self) -> Option<&Viewer> {
        match &self.state {
            State::Ready(viewer) => Some(viewer),
            _ => None,
        }
    }

    /// Why the last recording couldn't be opened.
    pub fn error(&self) -> Option<&str> {
        match &self.state {
            State::Failed { error, .. } => Some(error),
            _ => None,
        }
    }

    /// The window title: the recording's file name, with "(edited)" while its annotations
    /// differ from the opened ones, and the app's name.
    pub fn title(&self) -> String {
        let Some(path) = self.current_path() else {
            return APP_NAME.to_owned();
        };
        let edited = if self.viewer().is_some_and(Viewer::edited) {
            " (edited)"
        } else {
            ""
        };
        format!("{}{edited} – {APP_NAME}", file_name(path))
    }

    /// The title last sent to the window, which [`title`](Self::title) gives at the end of
    /// each frame.
    pub fn window_title(&self) -> &str {
        &self.title
    }

    /// Opens `path` on a background thread, with its tile service.
    pub fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        let store = match &self.options.sidecar_dir {
            Some(dir) => PyramidStore::Sidecar { dir: dir.clone() },
            None => PyramidStore::Memory,
        };
        let repaint = ctx.clone();
        let tiles = ctx.clone();
        let mut service_options = ServiceOptions::new(store);
        service_options.cache_budget = self.options.settings.tile_cache_bytes();
        let open = move |path: &Path| -> Result<Opened> {
            let document = document::open(path)?;
            let service =
                TileService::new(document.recording.clone(), service_options, move || {
                    tiles.request_repaint()
                })?;
            Ok((document, service))
        };
        self.state = State::Loading(Loading::start(path, open, move || {
            repaint.request_repaint();
        }));
    }

    /// One frame: picks up a finished open, handles keys, and draws the window.
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        // Fonts set during the last frame are in use now: widths measured once in the old
        // ones are measured again. Not in that frame, which had laid out text in them, so
        // egui kept them until this one began.
        if self.fonts_set {
            self.fonts_set = false;
            header::forget_text_widths(&ctx);
            if let State::Ready(viewer) = &mut self.state {
                viewer.forget_text_widths();
            }
        }
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(false);
        if focused && !self.focused {
            self.app_mode.refresh_accent();
        }
        self.focused = focused;
        self.apply_theme(&ctx);
        self.poll_loading();
        self.poll_update();
        // The window's close button, or Quit's own request: unsaved changes are asked about.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.closing
            && self.viewer().is_some_and(Viewer::edited)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.leaving = Some(Leave::Quit);
        }
        // The startup dialog is modal: show it once the window has been drawn and has focus.
        // On macOS a panel opened before the app is active is canceled at once; gaining
        // focus repaints, which brings us back here.
        // Before the file dialog a window started without a recording shows: Finder starts
        // an app bundle without arguments and hands over the file it was started for.
        self.take_opened(&ctx);
        if self.pick_file {
            let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
            if self.shown && focused {
                self.pick_file = false;
                self.pick_at_startup(&ctx);
            } else if !self.shown {
                ctx.request_repaint();
            }
        }
        self.shown = true;

        let mut actions = self
            .native_menu
            .as_ref()
            .map(NativeMenu::poll)
            .unwrap_or_default();
        actions.extend(self.key_actions(&ctx));
        if let Some(platform) = self.egui_menus() {
            let state = self.menu_state(&ctx);
            egui::Panel::top("menu bar").show(ui, |ui| {
                actions.extend(egui_menu_bar(ui, platform, &state));
            });
        }
        for action in actions {
            self.perform(&ctx, action);
        }
        self.take_dropped(&ctx);
        shortcuts_window(&ctx, &mut self.shortcuts_open, self.keymap.platform());
        about_window(&ctx, &mut self.about_open);
        if self.viewer().is_none() {
            self.go_to_open = false;
            self.info_open = false;
        }
        if let State::Ready(viewer) = &self.state {
            let document = viewer.document();
            info_window(&ctx, &mut self.info_open, &document.path, viewer.summary());
        }
        if let Some(seconds) = go_to_window(&ctx, &mut self.go_to_open, &mut self.go_to)
            && let State::Ready(viewer) = &mut self.state
        {
            viewer.go_to(seconds);
        }
        let repaint = ctx.clone();
        self.disk_cache
            .follow(self.settings_open, move || repaint.request_repaint());
        let mut settings = self.options.settings.clone();
        let mut clear_cache = false;
        if settings_window(
            &ctx,
            &mut self.settings_open,
            &mut settings,
            &mut self.channel_types_open,
            &self.disk_cache,
            &mut clear_cache,
        ) {
            self.set_settings(settings);
            self.apply_theme(&ctx);
        }
        if clear_cache {
            let keep = self
                .viewer()
                .and_then(Viewer::sidecar_path)
                .into_iter()
                .collect();
            let repaint = ctx.clone();
            self.disk_cache
                .clear(keep, move || repaint.request_repaint());
        }
        let mut settings = self.options.settings.clone();
        let recording_types = self.viewer().map(|v| v.view().kinds()).unwrap_or_default();
        if channel_types_window(
            &ctx,
            &mut self.channel_types_open,
            &mut settings.types,
            &recording_types,
            &mut self.channel_types_filter,
        ) {
            self.set_settings(settings);
        }
        let system_font = self.options.settings.system_font;
        if system_font != self.fonts_applied {
            self.fonts_applied = system_font;
            self.fonts_set = true;
            apply_fonts(&ctx, system_font);
        }
        if self.native_menu.is_some() {
            let state = self.menu_state(&ctx);
            if let Some(menu) = &mut self.native_menu {
                menu.update(&state);
            }
        }

        let mut open_clicked = false;
        let mut asked = None;
        match &mut self.state {
            State::Ready(viewer) => {
                let shown = viewer.show(
                    ui,
                    &self.gpu,
                    &self.keymap,
                    self.show_sidebar,
                    &mut self.options.annotation_colors,
                );
                asked = shown.action;
                if shown.channel_types {
                    self.channel_types_open = true;
                }
                if let Some(types) = shown.types {
                    self.options.settings.types = types;
                }
            }
            State::Empty => {
                egui::CentralPanel::default().show(ui, |ui| {
                    open_clicked = centered_message(ui, "No Recording Open", None);
                });
            }
            State::Loading(loading) => {
                let text = format!("Opening {}…", loading.path().display());
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.horizontal_centered(|ui| {
                            ui.spinner();
                            ui.label(text);
                        });
                    });
                });
            }
            State::Failed { error, .. } => {
                let color = ui.visuals().error_fg_color;
                egui::CentralPanel::default().show(ui, |ui| {
                    open_clicked =
                        centered_message(ui, "Could Not Open the Recording", Some((error, color)));
                });
            }
        }
        if open_clicked {
            self.leave(&ctx, Leave::Open);
        }
        if let Some(action) = asked {
            self.perform(&ctx, action);
        }
        self.drop_hint(&ctx);
        self.unsaved_question(&ctx);
        self.notice_window(&ctx);
        self.update_window(&ctx);
        let title = self.title();
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    /// Carries out `action`, unless it is disabled now.
    pub fn perform(&mut self, ctx: &egui::Context, action: Action) {
        // Esc and ⌘W close the window in front first; Esc takes back a drag before that.
        let dragging = self.viewer().is_some_and(Viewer::dragging);
        let closes = action == Action::Close || action == Action::Deselect && !dragging;
        if closes && let Some(front) = Overlay::front(ctx, self.open_overlays()) {
            *self.overlay_open(front) = false;
            return;
        }
        let state = self.menu_state(ctx);
        if !state.enabled(action) {
            return;
        }
        match action {
            // A text field with the keyboard undoes its own typing; on macOS the menu takes
            // ⌘Z before the field sees it, so it is handed on.
            Action::Undo | Action::Redo if state.typing => {
                if let Some(event) = text_field_key(action) {
                    ctx.input_mut(|i| i.events.push(event));
                }
            }
            Action::Open => self.leave(ctx, Leave::Open),
            Action::OpenRecent(index) => {
                if let Some(path) = self.options.recent.get(usize::from(index)) {
                    self.leave(ctx, Leave::OpenPath(path.to_owned()));
                }
            }
            Action::ClearRecent => self.options.recent.clear(),
            Action::Close => self.leave(ctx, Leave::Close),
            Action::SaveAnnotations => {
                self.save_annotations();
            }
            Action::ExportShownAnnotations => {
                if let Some(path) = self.viewer().map(|v| export_path(&v.save_path()))
                    && let Some(path) = self.ask(Dialog::Export(path))
                {
                    self.export_shown_to(&path);
                }
            }
            Action::LoadAnnotations => {
                if let Some(path) = self.ask(Dialog::Load) {
                    self.load_annotations_from(&path);
                }
            }
            Action::SaveChannels => {
                if let Some(path) = self.viewer().map(Viewer::channels_path)
                    && let Some(path) = self.ask(Dialog::SaveChannels(path))
                {
                    self.save_channels_to(&path);
                }
            }
            Action::LoadChannels => {
                if let Some(path) = self.viewer().map(Viewer::channels_path)
                    && let Some(path) = self.ask(Dialog::LoadChannels(path))
                {
                    self.load_channels_from(&path);
                }
            }
            Action::Quit => self.leave(ctx, Leave::Quit),
            Action::Shortcuts => self.shortcuts_open = !self.shortcuts_open,
            Action::RecordingInfo => self.info_open = !self.info_open,
            Action::About => self.about_open = true,
            Action::CheckForUpdates => self.check_for_updates(ctx, true),
            Action::ToggleSidebar => self.show_sidebar = !self.show_sidebar,
            Action::ToggleDc => self.set_settings(Settings {
                remove_dc: !self.options.settings.remove_dc,
                ..self.options.settings.clone()
            }),
            Action::ToggleScaleBars => self.set_settings(Settings {
                scale_bars: !self.options.settings.scale_bars,
                ..self.options.settings.clone()
            }),
            Action::ToggleAntialias => self.set_settings(Settings {
                antialias: !self.options.settings.antialias,
                ..self.options.settings.clone()
            }),
            Action::Settings => self.settings_open = true,
            Action::GoToTime => {
                if let Some(viewer) = self.viewer() {
                    self.go_to = GoToField::new(format_duration(viewer.view().start()));
                    self.go_to_open = true;
                }
            }
            _ => {
                if let Some(options) = action.stim_options(self.stim_options) {
                    self.stim_options = options;
                    if let State::Ready(viewer) = &mut self.state {
                        viewer.set_stim_options(options);
                    }
                } else if let State::Ready(viewer) = &mut self.state {
                    viewer.apply(action);
                }
            }
        }
    }

    /// The actions of this frame's key presses, unless a text field has the keyboard. Esc
    /// with a menu or popup open only closes it.
    fn key_actions(&self, ctx: &egui::Context) -> Vec<Action> {
        if ctx.egui_wants_keyboard_input() || self.asking() {
            return Vec::new();
        }
        // Esc in a text field: egui took the keyboard back before this frame's widgets.
        let popup = egui::Popup::is_any_open(ctx) || self.viewer().is_some_and(Viewer::was_typing);
        let actions: Vec<Action> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|event| match *event {
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } if self.menu_takes_keys() => self.keymap.egui_action(key, modifiers),
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => self.keymap.action(key, modifiers),
                    _ => None,
                })
                .collect()
        });
        actions
            .into_iter()
            .filter(|&a| !(popup && a == Action::Deselect))
            .collect()
    }

    fn pick_and_open(&mut self, ctx: &egui::Context) {
        if let Some(path) = self.ask(Dialog::Open) {
            self.open(ctx, path);
        }
    }

    /// The startup file dialog. On macOS it opens on the main queue's next turn and hands
    /// its file over as Finder does: started from Finder or the Dock, the window's first
    /// frames run while AppKit restores and draws it, inside a Core Animation commit, where
    /// AppKit refuses a modal dialog and cancels it at once.
    fn pick_at_startup(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "macos")]
        if self.dialog_answers.is_none()
            && let Some(inbox) = &self.opened
        {
            let inbox = Arc::clone(inbox);
            let files = self.open_dialog();
            dispatch2::DispatchQueue::main().exec_async(move || {
                if let Some(path) = files.pick_file() {
                    inbox.push([path]);
                }
            });
            return;
        }
        self.pick_and_open(ctx);
    }

    /// The dialog that opens a recording, not yet shown.
    fn open_dialog(&self) -> rfd::FileDialog {
        let mut files = rfd::FileDialog::new();
        if let Some(dir) = self.current_path().and_then(Path::parent) {
            files = files.set_directory(dir);
        }
        files
            .set_title("Open a Recording")
            // The first filter is the one selected when the dialog opens.
            .add_filter(
                "All Supported File Types",
                &[
                    "edf", "bdf", "vhdr", "set", "gdf", "fif", "xdf", "bvrh", "EDF", "BDF", "VHDR",
                    "SET", "GDF", "FIF", "XDF", "BVRH",
                ],
            )
            .add_filter("BDF", &["bdf", "BDF"])
            .add_filter("BrainVision", &["vhdr", "VHDR"])
            .add_filter("BVRF", &["bvrh", "BVRH"])
            .add_filter("EDF", &["edf", "EDF"])
            .add_filter("EEGLAB", &["set", "SET"])
            .add_filter("FIFF", &["fif", "FIF"])
            .add_filter("GDF", &["gdf", "GDF"])
            .add_filter("XDF", &["xdf", "XDF"])
            .add_filter("All Files", &["*"])
    }

    /// Answers the next file dialogs with `answers`, in order, instead of showing the
    /// system's; `None` cancels one. For tests.
    pub fn answer_file_dialogs(&mut self, answers: impl IntoIterator<Item = Option<PathBuf>>) {
        self.dialog_answers = Some(answers.into_iter().collect());
    }

    /// The path a file dialog picks, or `None` if it was canceled.
    fn ask(&mut self, dialog: Dialog) -> Option<PathBuf> {
        if let Some(answers) = &mut self.dialog_answers {
            return answers.pop_front().flatten();
        }
        let mut files = rfd::FileDialog::new();
        if let Some(dir) = self.current_path().and_then(Path::parent) {
            files = files.set_directory(dir);
        }
        let save = |files: rfd::FileDialog, title: &str, suggested: &Path| {
            let mut files = files.set_title(title).add_filter("Events Table", &["tsv"]);
            if let Some(dir) = suggested.parent() {
                files = files.set_directory(dir);
            }
            if let Some(name) = suggested.file_name() {
                files = files.set_file_name(name.to_string_lossy());
            }
            files.save_file()
        };
        match dialog {
            Dialog::Open => self.open_dialog().pick_file(),
            Dialog::Save(suggested) => save(files, "Save Annotations", &suggested),
            Dialog::Export(suggested) => save(files, "Export Shown Annotations", &suggested),
            Dialog::Load => files
                .set_title("Load Annotations")
                .add_filter("Events Table or MNE CSV", &["tsv", "csv", "TSV", "CSV"])
                .pick_file(),
            Dialog::SaveChannels(suggested) => {
                let mut files = files
                    .set_title("Save Channels")
                    .add_filter("Channels Table", &["tsv"]);
                if let Some(dir) = suggested.parent() {
                    files = files.set_directory(dir);
                }
                if let Some(name) = suggested.file_name() {
                    files = files.set_file_name(name.to_string_lossy());
                }
                files.save_file()
            }
            Dialog::LoadChannels(suggested) => {
                let mut files = files
                    .set_title("Load Channels")
                    .add_filter("Channels Table", &["tsv", "TSV"]);
                if let Some(dir) = suggested.parent() {
                    files = files.set_directory(dir);
                }
                files.pick_file()
            }
        }
    }

    /// Opens a file dropped on the window (the first, if several): a BIDS `channels.tsv`
    /// (by its name, `…_channels.tsv`) is loaded as Load Channels… does, an annotation file
    /// (by its extension, as Load Annotations… takes them) into the open recording,
    /// anything else opened as a recording, as if picked with Open…. Ignored while a
    /// question, notice or update offer is shown.
    fn take_dropped(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_owned()));
        let Some(path) = dropped else {
            return;
        };
        if self.asking() {
            return;
        }
        self.open_file(ctx, path);
    }

    /// Opens a file the system handed over (the first, if several) as a dropped one (see
    /// [`take_dropped`](Self::take_dropped)). While a question, notice or update offer is shown, it waits.
    fn take_opened(&mut self, ctx: &egui::Context) {
        let Some(inbox) = &self.opened else {
            return;
        };
        if self.asking() || inbox.is_empty() {
            return;
        }
        let Some(path) = inbox.take().into_iter().next() else {
            return;
        };
        self.pick_file = false;
        self.open_file(ctx, path);
    }

    /// Opens `path` as a recording, or loads it into the open one if it holds channels or
    /// annotations, by its name.
    fn open_file(&mut self, ctx: &egui::Context, path: PathBuf) {
        if Format::of(&path).is_none() {
            self.leave(ctx, Leave::OpenPath(path));
        } else if self.viewer().is_some() && is_channels_file(&path) {
            self.load_channels_from(&path);
        } else if self.viewer().is_some() {
            self.load_annotations_from(&path);
        } else if is_channels_file(&path) {
            self.notice = Some((
                "Could Not Load the Channels".to_owned(),
                format!(
                    "{} holds channel types. Open a recording first, then drop it on it.",
                    file_name(&path)
                ),
            ));
        } else {
            self.notice = Some((
                "Could Not Load the Annotations".to_owned(),
                format!(
                    "{} holds annotations. Open a recording first, then drop them on it.",
                    file_name(&path)
                ),
            ));
        }
    }

    /// While files are dragged over the window, dims it and says what dropping does.
    fn drop_hint(&self, ctx: &egui::Context) {
        let hovered = ctx.input(|i| {
            (!i.raw.hovered_files.is_empty()).then(|| i.raw.hovered_files[0].path.clone())
        });
        let Some(path) = hovered else {
            return;
        };
        let annotations = path.as_deref().is_some_and(|p| Format::of(p).is_some());
        let channels = path.as_deref().is_some_and(is_channels_file);
        let text = if channels && self.viewer().is_some() {
            "Drop to Load Channels"
        } else if annotations && self.viewer().is_some() {
            "Drop to Load Annotations"
        } else {
            "Drop to Open"
        };
        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("kymo drop hint"),
        ));
        let visuals = ctx.global_style().visuals.clone();
        painter.rect_filled(screen, 0.0, visuals.extreme_bg_color.gamma_multiply(0.8));
        painter.rect_stroke(
            screen.shrink(12.0),
            8.0,
            egui::Stroke::new(2.0, visuals.selection.stroke.color),
            egui::StrokeKind::Inside,
        );
        painter.text(
            screen.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::TextStyle::Heading.resolve(&ctx.global_style()),
            visuals.strong_text_color(),
        );
    }

    /// Closes the recording, opens another, or quits, after asking what to do with unsaved
    /// changes to the annotations.
    fn leave(&mut self, ctx: &egui::Context, leave: Leave) {
        if self.viewer().is_some_and(Viewer::edited) && !self.closing {
            self.leaving = Some(leave);
        } else {
            self.go(ctx, leave);
        }
    }

    /// Closes the recording, opens another, or quits, without asking.
    fn go(&mut self, ctx: &egui::Context, leave: Leave) {
        match leave {
            Leave::Close => self.state = State::Empty,
            Leave::Open => self.pick_and_open(ctx),
            Leave::OpenPath(path) => self.open(ctx, path),
            Leave::Quit => {
                self.closing = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Whether Go to Time… is open.
    pub fn go_to_open(&self) -> bool {
        self.go_to_open
    }

    /// The recordings in File → Open Recent.
    pub fn recent(&self) -> &Recent {
        &self.options.recent
    }

    /// What waits for the answer to "Save the changes?", while it is asked.
    pub fn leaving(&self) -> Option<Leave> {
        self.leaving.clone()
    }

    /// The message shown in a window, if there is one: its title and text.
    pub fn notice(&self) -> Option<(&str, &str)> {
        self.notice.as_ref().map(|(t, s)| (t.as_str(), s.as_str()))
    }

    /// Whether the window may close now.
    pub fn closing(&self) -> bool {
        self.closing
    }

    /// Asks "Save the changes?" while something waits for the answer: Save… saves (and
    /// goes on unless the save dialog was canceled), Don't Save goes on, Cancel stays.
    fn unsaved_question(&mut self, ctx: &egui::Context) {
        let Some(leave) = self.leaving.clone() else {
            return;
        };
        let name = self
            .viewer()
            .map_or_else(String::new, |v| v.document().name());
        #[derive(PartialEq)]
        enum Answer {
            Save,
            DontSave,
            Cancel,
        }
        let mut answer = None;
        let modal = egui::Modal::new(egui::Id::unique("kymo unsaved changes")).show(ctx, |ui| {
            ui.set_max_width(360.0);
            ui.heading(format!("Save the changes to the annotations of {name}?"));
            ui.label("They are lost if you don't save them.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save…").clicked() {
                    answer = Some(Answer::Save);
                }
                if ui.button("Don't Save").clicked() {
                    answer = Some(Answer::DontSave);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(Answer::Cancel);
                }
            });
        });
        if answer.is_none() && modal.should_close() {
            answer = Some(Answer::Cancel);
        }
        let Some(answer) = answer else {
            return;
        };
        self.leaving = None;
        let go_on = match answer {
            Answer::Save => self.save_annotations(),
            Answer::DontSave => true,
            Answer::Cancel => false,
        };
        if go_on {
            self.go(ctx, leave);
        }
    }

    /// Shows the notice, if there is one, until it is dismissed.
    fn notice_window(&mut self, ctx: &egui::Context) {
        let Some((title, text)) = &self.notice else {
            return;
        };
        let mut ok = false;
        let modal = egui::Modal::new(egui::Id::unique("kymo notice")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading(title.as_str());
            ui.label(text.as_str());
            ui.add_space(8.0);
            ok = ui.button("OK").clicked();
        });
        if ok || modal.should_close() {
            self.notice = None;
        }
    }

    /// Asks where to save the annotations and saves them there; whether it did.
    fn save_annotations(&mut self) -> bool {
        let Some(suggested) = self.viewer().map(Viewer::save_path) else {
            return false;
        };
        let Some(path) = self.ask(Dialog::Save(suggested)) else {
            return false;
        };
        self.save_annotations_to(&path)
    }

    /// Saves the file's and the viewer's annotations to `path`, as an events table with
    /// the extension `.tsv` (so nothing reads it as another format, and the recording is
    /// never overwritten); whether it did. An error shows in a notice.
    pub fn save_annotations_to(&mut self, path: &Path) -> bool {
        let path = with_tsv(path);
        let State::Ready(viewer) = &mut self.state else {
            return false;
        };
        match write_file(&path, |out| viewer.write_annotations(out, false)) {
            Ok(()) => {
                viewer.mark_saved(path);
                true
            }
            Err(error) => {
                self.notice = Some((
                    "Could Not Save the Annotations".into(),
                    format!("{}: {error}", path.display()),
                ));
                false
            }
        }
    }

    /// Exports the annotations of the descriptions shown to `path`, as
    /// [`save_annotations_to`](Self::save_annotations_to) does, without marking them saved.
    pub fn export_shown_to(&mut self, path: &Path) -> bool {
        let path = with_tsv(path);
        let Some(viewer) = self.viewer() else {
            return false;
        };
        match write_file(&path, |out| viewer.write_annotations(out, true)) {
            Ok(()) => true,
            Err(error) => {
                self.notice = Some((
                    "Could Not Export the Annotations".into(),
                    format!("{}: {error}", path.display()),
                ));
                false
            }
        }
    }

    /// Adds the annotations of the file at `path` (an events table or MNE's CSV) as one
    /// step; whether it did. A notice tells of an error, and of annotations skipped.
    pub fn load_annotations_from(&mut self, path: &Path) -> bool {
        let State::Ready(viewer) = &mut self.state else {
            return false;
        };
        let name = file_name(path);
        let fail = |text: String| Some(("Could Not Load the Annotations".to_owned(), text));
        let Some(format) = Format::of(path) else {
            self.notice = fail(format!("{name} is not a .tsv or .csv file."));
            return false;
        };
        let context = annotation_file::Context::of(&viewer.document().recording);
        let read = File::open(path)
            .map_err(annotation_file::FileError::from)
            .and_then(|f| annotation_file::read(BufReader::new(f), format, &context));
        let loaded = match read {
            Ok(loaded) => loaded,
            Err(error) => {
                self.notice = fail(format!("{name}: {error}"));
                return false;
            }
        };
        let count = loaded.annotations.len();
        viewer.add_annotations(loaded.annotations);
        let mut skipped = Vec::new();
        if loaded.outside > 0 {
            skipped.push(format!("{} outside the recording", loaded.outside));
        }
        if loaded.unknown_channels > 0 {
            skipped.push(format!(
                "{} naming channels it doesn't have",
                loaded.unknown_channels
            ));
        }
        if !skipped.is_empty() || count == 0 {
            let mut text = format!("Loaded {count} annotation{} from {name}.", plural(count));
            if !skipped.is_empty() {
                text += &format!(" Skipped {}.", skipped.join(" and "));
            }
            self.notice = Some(("Annotations Loaded".into(), text));
        }
        true
    }

    /// Saves every channel's type and bad mark to `path` as a BIDS `channels.tsv` (with the
    /// extension `.tsv`, so the recording is never overwritten); whether it did. An error
    /// shows in a notice.
    pub fn save_channels_to(&mut self, path: &Path) -> bool {
        let path = with_tsv(path);
        let State::Ready(viewer) = &mut self.state else {
            return false;
        };
        match write_file(&path, |out| viewer.write_channels(out)) {
            Ok(()) => {
                viewer.set_channels_path(path);
                true
            }
            Err(error) => {
                self.notice = Some((
                    "Could Not Save the Channels".into(),
                    format!("{}: {error}", path.display()),
                ));
                false
            }
        }
    }

    /// Takes the channel types and bad marks of the `channels.tsv` at `path`; whether it
    /// did. A type kymo doesn't know, given to a channel, is kept as New Type… keeps one. A
    /// notice tells of an error, and of rows or types left out and units that differ.
    pub fn load_channels_from(&mut self, path: &Path) -> bool {
        let State::Ready(viewer) = &mut self.state else {
            return false;
        };
        let name = file_name(path);
        let context = channel_file::Context::of(&viewer.document().recording);
        let read = File::open(path)
            .map_err(annotation_file::FileError::from)
            .and_then(|f| channel_file::read(BufReader::new(f), &context));
        let loaded = match read {
            Ok(loaded) => loaded,
            Err(error) => {
                self.notice = Some((
                    "Could Not Load the Channels".into(),
                    format!("{name}: {error}"),
                ));
                return false;
            }
        };
        let refused = viewer.load_channels(&loaded);
        viewer.set_channels_path(path.to_owned());
        let mut kept = false;
        for (channel, kind) in &loaded.kinds {
            let custom = kind.known().is_none() && !kind.is_stim();
            let given = viewer.view().kind_of(*channel) == kind;
            let types = &mut self.options.settings.types;
            if custom && given && viewer.view().retyped(*channel) && !types.stored(kind) {
                types.insert(crate::types::TypeSet {
                    name: kind.name().to_owned(),
                    ..Default::default()
                });
                kept = true;
            }
        }
        if kept {
            viewer.set_type_settings(self.options.settings.types.clone());
        }
        let mut notes = Vec::new();
        if loaded.unknown > 0 {
            notes.push(format!(
                "Skipped {} row{} naming channels the recording doesn't have.",
                loaded.unknown,
                plural(loaded.unknown)
            ));
        }
        if refused > 0 {
            notes.push(format!(
                "Left {refused} channel{} as {}: only the file makes a channel Stim.",
                plural(refused),
                if refused == 1 { "it is" } else { "they are" }
            ));
        }
        if loaded.units_differ > 0 {
            let n = loaded.units_differ;
            notes.push(format!(
                "{n} channel{} {} other units in {name}; their values come from the \
                 recording.",
                plural(n),
                if n == 1 { "has" } else { "have" }
            ));
        }
        if loaded.kinds.is_empty() && loaded.bad.is_empty() {
            notes.insert(0, format!("{name} names none of the recording's channels."));
        }
        if !notes.is_empty() {
            self.notice = Some(("Channels Loaded".into(), notes.join(" ")));
        }
        true
    }

    fn current_path(&self) -> Option<&Path> {
        match &self.state {
            State::Empty => None,
            State::Loading(loading) => Some(loading.path()),
            State::Ready(viewer) => Some(&viewer.document().path),
            State::Failed { path, .. } => Some(path),
        }
    }

    fn poll_loading(&mut self) {
        let State::Loading(loading) = &self.state else {
            return;
        };
        let Some(result) = loading.poll() else {
            return;
        };
        // Recordings that open go first in Open Recent; ones that don't (moved, deleted,
        // not a recording) leave it.
        match &result {
            Ok(_) => self.options.recent.add(loading.path()),
            Err(_) => self.options.recent.remove(loading.path()),
        }
        self.state = match result {
            Ok((document, service)) => {
                let mut viewer = Viewer::new(
                    document,
                    service,
                    self.options.settings.default_window_s,
                    self.stim_options,
                );
                viewer.set_antialias(self.options.settings.antialias);
                viewer.set_scale_bars(self.options.settings.scale_bars);
                viewer.set_remove_dc(self.options.settings.remove_dc);
                viewer.set_type_settings(self.options.settings.types.clone());
                if self.options.settings.auto_scale_on_open {
                    viewer.apply(Action::AutoScale);
                }
                State::Ready(Box::new(viewer))
            }
            Err(error) => State::Failed {
                path: loading.path().to_owned(),
                error: error_text(&error),
            },
        };
    }
}

impl eframe::App for KymoApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    /// Keeps the settings, the annotation colors, the recent recordings and the update
    /// checks' state for the next run.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.options.settings.save(storage);
        self.options.update_state.save(storage);
        save_annotation_colors(&self.options.annotation_colors, storage);
        self.options.recent.save(storage);
    }
}

/// What the menus show: whether a recording is open, what it has, and the toggles' states.
struct AppMenuState {
    ready: bool,
    /// Whether Settings, Keyboard Shortcuts or About is open, for Close to close it.
    overlay: bool,
    /// Whether a text field has the keyboard, for Undo and Redo to act on.
    typing: bool,
    /// The labels of File → Open Recent.
    recent: Vec<String>,
    /// Whether annotation mode is on, and an annotation selected.
    editing: bool,
    selected: bool,
    /// Whether channels are selected in the channel list, for Esc to clear.
    channels_selected: bool,
    /// Whether a drag is changing an annotation: until it ends, nothing else may, and Esc
    /// takes it back.
    dragging: bool,
    /// Whether a measurement is on screen or being made, for Esc to remove.
    measured: bool,
    can_undo: bool,
    can_redo: bool,
    sidebar: bool,
    scale_bars: bool,
    antialias: bool,
    dc: bool,
    /// Whether the recording has stim channels, whose codes the rule annotates.
    stim: bool,
    /// Whether any annotations are shown, for Export Shown Annotations to write.
    shown: bool,
    /// Whether Merge Annotations has anything to merge.
    mergeable: bool,
    stim_options: EventOptions,
}

impl MenuState for AppMenuState {
    fn enabled(&self, action: Action) -> bool {
        let now = match action {
            // Closes the window in front, without a recording too.
            Action::Close if self.overlay => return true,
            Action::Undo => self.can_undo && !self.dragging || self.typing,
            Action::Redo => self.can_redo && !self.dragging || self.typing,
            Action::DeleteAnnotation => self.selected && !self.dragging,
            Action::MergeAnnotations => self.mergeable && !self.dragging,
            Action::Deselect => {
                self.selected || self.dragging || self.measured || self.channels_selected
            }
            Action::ExportShownAnnotations => self.shown,
            Action::ScaleSelectedUp | Action::ScaleSelectedDown => self.channels_selected,
            Action::OpenRecent(index) => usize::from(index) < self.recent.len(),
            Action::ClearRecent => !self.recent.is_empty(),
            _ if action.stim_checked(self.stim_options).is_some() => self.stim,
            _ => true,
        };
        action.enabled(self.ready) && now
    }

    fn checked(&self, action: Action) -> Option<bool> {
        match action {
            Action::ToggleSidebar => Some(self.sidebar),
            Action::ToggleScaleBars => Some(self.scale_bars),
            Action::ToggleAntialias => Some(self.antialias),
            Action::ToggleDc => Some(self.dc),
            Action::ToggleEditing => Some(self.editing),
            _ => action.stim_checked(self.stim_options),
        }
    }

    fn recent(&self) -> Vec<String> {
        self.recent.clone()
    }
}

/// The key press a text field takes as `action`, for Undo and Redo: ⌘Z and ⇧⌘Z.
pub fn text_field_key(action: Action) -> Option<Event> {
    let shift = match action {
        Action::Undo => false,
        Action::Redo => true,
        _ => return None,
    };
    let mut modifiers = egui::Modifiers::COMMAND;
    modifiers.shift = shift;
    if cfg!(target_os = "macos") {
        modifiers.mac_cmd = true;
    } else {
        modifiers.ctrl = true;
    }
    Some(Event::Key {
        key: egui::Key::Z,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    })
}

/// `path` with the extension `.tsv`, unless it has it (in any case).
/// Whether `path` is named as a BIDS `channels.tsv` (`…_channels.tsv`, in any case).
fn is_channels_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|n| n == "channels.tsv" || n.ends_with("_channels.tsv"))
}

fn with_tsv(path: &Path) -> PathBuf {
    if Format::of(path) == Some(Format::Table) {
        path.to_owned()
    } else {
        let mut name = path.as_os_str().to_owned();
        name.push(".tsv");
        PathBuf::from(name)
    }
}

/// Where to suggest exporting the shown annotations, next to `saved`.
fn export_path(saved: &Path) -> PathBuf {
    let stem = saved
        .file_stem()
        .map_or("annotations".into(), |s| s.to_string_lossy());
    saved.with_file_name(format!("{stem}-shown.tsv"))
}

/// Writes a file with `write`, then flushes it.
fn write_file(
    path: &Path,
    write: impl FnOnce(&mut BufWriter<File>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    write(&mut out)?;
    out.flush()
}

/// A message with an optional detail and an Open button, centered. Returns whether the
/// button was clicked.
fn centered_message(ui: &mut Ui, message: &str, detail: Option<(&String, Color32)>) -> bool {
    let mut clicked = false;
    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        ui.add_space(ui.available_height() / 3.0);
        ui.heading(message);
        if let Some((detail, color)) = detail {
            ui.add_space(8.0);
            ui.label(RichText::new(detail).color(color));
        }
        ui.add_space(12.0);
        clicked = ui.button("Open…").clicked();
    });
    clicked
}

/// Starts the window and runs until it closes.
pub fn run(path: Option<PathBuf>) -> Result<()> {
    // macOS titles the app menu with the process name, which outside an app bundle is the
    // executable's ("kymo").
    #[cfg(target_os = "macos")]
    objc2_foundation::NSProcessInfo::processInfo()
        .setProcessName(objc2_foundation::ns_string!(APP_NAME));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_app_id("kymo")
            .with_icon(window_icon())
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([480.0, 320.0]),
        ..Default::default()
    };
    let inbox = Arc::new(Inbox::new());
    let receiving = inbox.clone();
    let creator: eframe::AppCreator<'static> = Box::new(move |cc| {
        let gpu = Gpu::new(cc.wgpu_render_state.as_ref());
        let options = Options {
            settings: Settings::load(cc.storage),
            annotation_colors: load_annotation_colors(cc.storage),
            recent: Recent::load(cc.storage),
            updates: Some(Source::github()),
            update_state: UpdateState::load(cc.storage),
            ..Options::default()
        };
        let native = options.menus == Menus::Native;
        let mut app = KymoApp::new(&cc.egui_ctx, gpu, options, path);
        app.follow_app_mode(AppMode::watch(&cc.egui_ctx));
        // Without it, egui draws the menu bar.
        if native {
            match NativeMenu::attach(cc) {
                Ok(menu) => app.set_native_menu(menu),
                Err(error) => eprintln!("kymo: no native menu bar: {error}"),
            }
        }
        app.receive_opened(&cc.egui_ctx, receiving);
        app.start(&cc.egui_ctx);
        Ok(Box::new(app))
    });
    run_window(options, creator, inbox)
}

/// Runs the window on eframe's own event loop: Windows and Linux hand over files only as
/// arguments.
#[cfg(not(target_os = "macos"))]
fn run_window(
    options: eframe::NativeOptions,
    creator: eframe::AppCreator<'static>,
    _inbox: Arc<Inbox>,
) -> Result<()> {
    eframe::run_native(APP_NAME, options, creator)
        .map_err(|e| anyhow::anyhow!("cannot open a window: {e}"))
}

/// Runs the window on an event loop built here, so that winit's app delegate can receive
/// the files Finder hands over (into `inbox`) before the app has launched: those it was
/// started for arrive before then.
#[cfg(target_os = "macos")]
fn run_window(
    options: eframe::NativeOptions,
    creator: eframe::AppCreator<'static>,
    inbox: Arc<Inbox>,
) -> Result<()> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let event_loop = winit::event_loop::EventLoop::<eframe::UserEvent>::with_user_event()
        .build()
        .map_err(|e| anyhow::anyhow!("cannot open a window: {e}"))?;
    crate::opened::receive_on(inbox);
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("the window must be opened on the main thread"))?;
    // Winit's delegate, which it set while building the event loop.
    match NSApplication::sharedApplication(mtm).delegate() {
        Some(delegate) => {
            let delegate: &objc2::runtime::AnyObject = delegate.as_ref();
            crate::opened::add_open_urls(delegate.class());
        }
        None => eprintln!("kymo: no app delegate, files from Finder can't be opened"),
    }
    let mut app = eframe::create_native(APP_NAME, options, creator, &event_loop);
    event_loop
        .run_app(&mut app)
        .map_err(|e| anyhow::anyhow!("cannot open a window: {e}"))
}
