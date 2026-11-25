//! The window, driven headless through egui_kittest: opening, keys, clicks and the status
//! line, with the trace renderer on a real GPU where there is one.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::kinds::{KINDS, kind_index};
use eframe::egui::{self, Key, Modifiers};
use eframe::egui_wgpu;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use kymo::action::Action;
use kymo::annotations::{self, AnnotationColor, AnnotationColors};
use kymo::app::{KymoApp, Menus, Options};
use kymo::app_mode::AppMode;
use kymo::keymap::Platform;
use kymo::settings::{Settings, Theme, load_annotation_colors};
use kymo::types::TypeSettings;
use kymo::ui::axis::Clock;
use kymo::ui::gpu::Gpu;
use kymo::ui::palette::{annotation_color, trace_color};
use kymo::ui::types::{ALL_TYPES, CHANGE_TYPE, FROM_FILE, NEW_TYPE, OTHER_TYPES};
use kymo::ui::{minimap, scrollbar, status};
use kymo::update::{Release, Source, UpdateState};
use kymo::viewer::Viewer;
use kymo_core::ChannelKind;
use kymo_testgen::{TempDir, TempFile};

/// A preset with its noise and spikes from `seed` (0 is the preset's own).
fn seeded(name: &str, seed: u64) -> TempFile {
    let mut spec = kymo_testgen::Spec::preset(name).unwrap();
    spec.seed ^= seed;
    TempFile::written(&spec)
}

/// A preset without its trigger channel, so only the file's annotations are shown.
fn without_triggers(name: &str) -> TempFile {
    let mut spec = kymo_testgen::Spec::preset(name).unwrap();
    spec.channels.retain(|c| !c.trigger);
    TempFile::written(&spec)
}

/// Pyramids in memory, the menu bar drawn by egui as on Linux, on every platform, and
/// default settings but for egui's fonts, the same on every platform.
fn options() -> Options {
    Options {
        sidecar_dir: None,
        menus: Menus::Egui(Platform::Linux),
        settings: Settings {
            system_font: false,
            ..Settings::default()
        },
        annotation_colors: Default::default(),
        recent: Default::default(),
        updates: None,
        update_state: Default::default(),
    }
}

/// The system font fallback eframe adds (for ⌘ and friends), as the app has it.
fn system_fonts(ctx: &egui::Context) {
    ctx.add_font_provider(std::sync::Arc::new(
        egui_system_font_provider::SystemFontProvider::new(),
    ));
}

const SIZE: egui::Vec2 = egui::vec2(1200.0, 700.0);

/// The window opening `path`, without a GPU: egui skips the trace renderer's callback.
fn window(path: &Path) -> Harness<'static, KymoApp> {
    window_from(Harness::builder().with_size(SIZE), path)
}

/// [`window`], its screenshots rendered on the shared GPU (see [`shared_gpu`]) rather than
/// on a device kittest creates for it; on kittest's where there is no shared one.
fn screenshot_window(path: &Path) -> Harness<'static, KymoApp> {
    screenshot_window_sized(path, SIZE)
}

/// [`screenshot_window`] of `size`.
fn screenshot_window_sized(path: &Path, size: egui::Vec2) -> Harness<'static, KymoApp> {
    let mut builder = Harness::builder();
    if let Some(gpu) = shared_gpu() {
        builder = builder.wgpu_setup(egui_wgpu::WgpuSetup::Existing(gpu.clone()));
    }
    window_from(builder.with_size(size), path)
}

fn window_from(
    builder: egui_kittest::HarnessBuilder<KymoApp>,
    path: &Path,
) -> Harness<'static, KymoApp> {
    builder.build_eframe(|cc| {
        // eframe installs it before the app starts; the measurement's → needs it.
        system_fonts(&cc.egui_ctx);
        KymoApp::new(
            &cc.egui_ctx,
            Gpu::none("no GPU in this test"),
            options(),
            Some(path.to_owned()),
        )
    })
}

/// ⌘ on macOS, Ctrl elsewhere, as winit reports them.
fn command() -> Modifiers {
    let base = if cfg!(target_os = "macos") {
        Modifiers::MAC_CMD
    } else {
        Modifiers::CTRL
    };
    base | Modifiers::COMMAND
}

/// Steps until `done` holds, sleeping between steps for the background threads.
fn step_until(harness: &mut Harness<'_, KymoApp>, what: &str, done: impl Fn(&KymoApp) -> bool) {
    let start = Instant::now();
    while !done(harness.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for {what}"
        );
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.step();
}

/// Whether the status line reads a value in µV (EEG's unit, whatever the file says); the
/// annotations under the pointer may follow it.
fn has_reading(status: &str) -> bool {
    status.split(" · ").any(|part| part.ends_with(" \u{b5}V"))
}

/// Opens the recording and waits for the pyramid build and every tile of the view.
fn ready(harness: &mut Harness<'_, KymoApp>) {
    step_until(harness, "the recording", |app| app.viewer().is_some());
    step_until(harness, "the build and the tiles", |app| {
        let viewer = app.viewer().unwrap();
        !viewer.status().right.contains("Building") && viewer.complete()
    });
}

#[test]
fn opens_a_recording_and_builds_its_overview() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let viewer = harness.state().viewer().unwrap();
    assert_eq!(viewer.view().time(), 0.0..20.0);
    // Once the overview is built, the status line's right side is empty.
    assert_eq!(viewer.status().right, "", "{:?}", viewer.status());
    harness.get_by_label("A1");
}

#[test]
fn a_missing_file_shows_the_error() {
    let file = TempFile::new("bdf");
    let mut harness = window(file.path());
    step_until(&mut harness, "the error", |app| app.error().is_some());
    let error = harness.state().error().unwrap().to_owned();
    assert!(
        error.contains(&file.path().display().to_string()),
        "{error}"
    );
    harness.get_by_label("Could Not Open the Recording");
    harness.get_by_label(&error);
}

#[test]
fn keys_move_the_view() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let time = |h: &Harness<'_, KymoApp>| h.state().viewer().unwrap().view().time();
    harness.key_press(Key::ArrowRight);
    harness.step();
    assert_eq!(time(&harness), 5.0..25.0);
    let cmd = command();
    harness.key_press_modifiers(cmd, Key::Num9);
    harness.step();
    assert_eq!(time(&harness), 0.0..60.0);
    harness.key_press_modifiers(cmd, Key::Num0);
    harness.step();
    assert_eq!(time(&harness), 0.0..20.0);
    harness.key_press_modifiers(cmd, Key::W);
    harness.step();
    assert!(harness.state().viewer().is_none());
    harness.get_by_label("No Recording Open");
}

fn viewer<'h>(harness: &'h Harness<'_, KymoApp>) -> &'h Viewer {
    harness.state().viewer().unwrap()
}

/// Moves the pointer to `pos` and lets the window see it.
fn hover(harness: &mut Harness<'_, KymoApp>, pos: egui::Pos2) {
    harness.hover_at(pos);
    harness.step();
}

/// A primary click at `pos`.
fn click(harness: &mut Harness<'_, KymoApp>, pos: egui::Pos2) {
    hover(harness, pos);
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.step();
}

/// Two primary clicks at `pos`, a double click where the harness's frames are short enough
/// (each event gets its own; see [`window_with_short_frames`]).
fn double_click(harness: &mut Harness<'_, KymoApp>, pos: egui::Pos2) {
    hover(harness, pos);
    for pressed in [true, false, true, false] {
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.step();
}

/// [`window`] with frames of 1/60 s rather than kittest's 1/4 s, for a double click.
fn window_with_short_frames(path: &Path) -> Harness<'static, KymoApp> {
    window_from(
        Harness::builder().with_size(SIZE).with_step_dt(1.0 / 60.0),
        path,
    )
}

/// A trackpad scroll of `delta` points at the pointer.
fn scroll(harness: &mut Harness<'_, KymoApp>, delta: egui::Vec2, modifiers: Modifiers) {
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Trackpad,
        modifiers,
    });
    harness.step();
}

#[test]
fn scrolling_pans_time_and_rows() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    // Five rows of five channels, so there is something to scroll.
    harness.key_press_modifiers(command() | Modifiers::SHIFT, Key::Minus);
    harness.step();
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    assert!(rows < 5.0, "{rows} rows");
    hover(&mut harness, layout.traces.center());

    // Content follows the fingers: moving it left shows later times.
    let width = layout.traces.width();
    scroll(&mut harness, egui::vec2(-width / 4.0, 0.0), Modifiers::NONE);
    let time = viewer(&harness).view().time();
    assert!((time.start - 5.0).abs() < 1e-3, "{time:?}");
    // Moving it up by a row shows the next channel.
    let row = layout.traces.height() / rows;
    scroll(&mut harness, egui::vec2(0.0, -row), Modifiers::NONE);
    assert!((viewer(&harness).view().top() - 1.0).abs() < 1e-3);

    // Outside the trace region, scrolling leaves the view alone.
    hover(&mut harness, egui::pos2(5.0, 5.0));
    scroll(&mut harness, egui::vec2(-100.0, -100.0), Modifiers::NONE);
    assert!((viewer(&harness).view().time().start - 5.0).abs() < 1e-3);
    assert!((viewer(&harness).view().top() - 1.0).abs() < 1e-3);
}

#[test]
fn pinching_and_command_scrolling_zoom_around_the_pointer() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    harness.key_press(Key::ArrowRight);
    harness.step();
    let layout = viewer(&harness).layout().unwrap();
    let traces = layout.traces;
    // A quarter of the way in: 5 + 20/4 = 10 s.
    let at = egui::pos2(traces.left() + traces.width() / 4.0, traces.center().y);
    hover(&mut harness, at);
    harness.event(egui::Event::Zoom(2.0));
    harness.step();
    let time = viewer(&harness).view().time();
    assert!((time.end - time.start - 10.0).abs() < 1e-6, "{time:?}");
    assert!(
        (time.start + (time.end - time.start) / 4.0 - 10.0).abs() < 1e-3,
        "{time:?}"
    );

    // ⌘-scrolling zooms out again, around the same time.
    scroll(&mut harness, egui::vec2(0.0, -200.0), command());
    let time = viewer(&harness).view().time();
    assert!((time.end - time.start - 20.0).abs() < 1e-6, "{time:?}");
    assert!((time.start - 5.0).abs() < 1e-3, "{time:?}");
}

#[test]
fn dragging_the_traces_pans() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    harness.key_press(Key::ArrowRight);
    harness.step();
    let traces = viewer(&harness).layout().unwrap().traces;
    let from = traces.center();
    harness.drag_at(from);
    harness.step();
    // Dragging right by a tenth of the width shows 2 s earlier.
    harness.hover_at(from + egui::vec2(traces.width() / 10.0, 0.0));
    harness.step();
    harness.drop_at(from + egui::vec2(traces.width() / 10.0, 0.0));
    harness.step();
    let time = viewer(&harness).view().time();
    assert!((time.start - 3.0).abs() < 1e-3, "{time:?}");
}

#[test]
fn clicking_a_name_toggles_bad() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    // Just above the lower edge of the second row, so a row lookup off by the panel's
    // margin lands in the third.
    let row_y = |i: f32| layout.names.top() + (i + 1.0) * layout.names.height() / rows - 2.0;
    let a2 = kymo_core::ChannelId(1);

    let name = egui::pos2(layout.names.center().x, row_y(1.0));
    click(&mut harness, name);
    assert!(viewer(&harness).view().is_bad(a2));
    click(&mut harness, name);
    assert!(!viewer(&harness).view().is_bad(a2));
}

/// Bad marks can be undone and redone, however they were made: a click on a trace's name,
/// the selection bar. They don't mark the annotations edited.
#[test]
fn bad_marks_undo_and_redo() {
    use egui::accesskit::Role;
    let file = typed_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    let row_y = |i: f32| layout.names.top() + (i + 0.5) * layout.names.height() / rows;
    let bad = |h: &Harness<'_, KymoApp>| -> Vec<u32> {
        viewer(h).view().bad_channels().map(|c| c.0).collect()
    };
    let undo = |h: &mut Harness<'_, KymoApp>| press(h, command(), Key::Z);
    let redo = |h: &mut Harness<'_, KymoApp>| press(h, command() | Modifiers::SHIFT, Key::Z);

    click(
        &mut harness,
        egui::pos2(layout.names.center().x, row_y(1.0)),
    );
    assert_eq!(bad(&harness), [1]);
    assert!(!viewer(&harness).edited());
    undo(&mut harness);
    assert!(bad(&harness).is_empty());
    redo(&mut harness);
    assert_eq!(bad(&harness), [1]);

    // The bar's marks, one step.
    click_in_list(&mut harness, Role::Label, "A1");
    harness
        .get_by_role_and_label(Role::Label, "A2")
        .click_modifiers(command());
    harness.run_steps(2);
    menu_path(&mut harness, &["Mark", "Mark 1 Channel as Bad"]);
    assert_eq!(bad(&harness), [0, 1]);
    menu_path(&mut harness, &["Mark", "Mark 2 Channels as Good"]);
    assert!(bad(&harness).is_empty());
    undo(&mut harness);
    assert_eq!(bad(&harness), [0, 1]);
    assert!(!viewer(&harness).edited());
}

/// A click anywhere on a row of the channel list selects it, and takes the only selected row
/// out of the selection again; a selected row shows it under the pointer too, and the names
/// beside the traces don't show the selection. A row's checkbox says on hover what a click
/// does.
#[test]
fn a_click_on_the_selected_row_deselects_it() {
    use egui::accesskit::Role;
    let file = typed_preset();
    let mut harness = fonts_window(file.path(), options());
    ready(&mut harness);
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    // A2's name beside the traces, right of the name; in its row of the list, between the
    // name and the Bad toggle.
    let name_spot = egui::pos2(
        layout.names.right() - 6.0,
        layout.names.top() + 1.5 * layout.names.height() / rows,
    );
    // Where the list has scrolled to, and the pointer on the row: below and right of the
    // spot, so the pointer drawn there doesn't cover it.
    let row_spot = |h: &Harness<'_, KymoApp>| {
        h.get_by_label("Show A2").rect().center() + egui::vec2(40.0, 0.0)
    };
    let on_row = egui::vec2(4.0, 6.0);
    let away = egui::pos2(900.0, 650.0);
    // The pixels at both spots, the pointer at `pointer` (on A2's row without one).
    let pixels = |h: &mut Harness<'_, KymoApp>, pointer: Option<egui::Pos2>| {
        let row_spot = row_spot(h);
        hover(h, pointer.unwrap_or(row_spot + on_row));
        h.run_steps(2);
        let ppp = h.ctx.pixels_per_point();
        let image = h.render().unwrap();
        let at = |p: egui::Pos2| image.get_pixel((p.x * ppp) as u32, (p.y * ppp) as u32).0;
        (at(name_spot), at(row_spot))
    };
    let (name, row) = pixels(&mut harness, Some(away));
    let (_, hovered) = pixels(&mut harness, None);
    click_in_list(&mut harness, Role::Label, "A2");
    assert_eq!(
        viewer(&harness).view().selected(),
        [kymo_core::ChannelId(1)]
    );
    let (selected_name, selected_row) = pixels(&mut harness, Some(away));
    assert_eq!(selected_name, name);
    assert_ne!(selected_row, row);
    let (_, selected_hovered) = pixels(&mut harness, None);
    assert_ne!(selected_hovered, hovered);
    click_in_list(&mut harness, Role::Label, "A2");
    assert!(viewer(&harness).view().selected().is_empty());
    assert_eq!(pixels(&mut harness, Some(away)), (name, row));
    // In a larger selection a click selects the row alone, as ever.
    click_in_list(&mut harness, Role::Label, "A1");
    harness
        .get_by_role_and_label(Role::Label, "A2")
        .click_modifiers(command());
    harness.run_steps(2);
    click_in_list(&mut harness, Role::Label, "A2");
    assert_eq!(
        viewer(&harness).view().selected(),
        [kymo_core::ChannelId(1)]
    );
    // Beside the name, too.
    let name = harness.get_by_role_and_label(Role::Label, "A1").rect();
    click(
        &mut harness,
        egui::pos2(name.right() + 8.0, name.center().y),
    );
    harness.run_steps(2);
    assert_eq!(
        viewer(&harness).view().selected(),
        [kymo_core::ChannelId(0)]
    );
    // A checkbox says what a click does.
    let checkbox = harness.get_by_label("Show A1").rect().center();
    hover(&mut harness, checkbox);
    assert!(tooltip(&mut harness, "Hide A1"));
}

/// The channel list shows units as the readout does: µV for EEG, mV for sEEG, whatever the
/// file stores (uV for both here), in the type's row, as each type's channels share theirs;
/// the channels' rows show none.
#[test]
fn the_channel_list_shows_display_units() {
    use egui::accesskit::Role;
    let file = TempFile::preset("edf+c-kinds");
    let mut harness = window(file.path());
    ready(&mut harness);
    // The list draws the rows in view: EEG Fz comes first; SEEG LA1 once the types between
    // them are folded.
    for kind in ["MEG Mag", "MEG Grad", "EOG", "ECG", "EMG"] {
        harness
            .get_by_label(&format!("Collapse {kind} channels"))
            .click();
        harness.run_steps(2);
    }
    let unit_in_row = |label: &str| {
        let y = harness.get_by_label(label).rect().center().y;
        ["\u{b5}V", "mV", "uV"].into_iter().find(|unit| {
            harness
                .query_all_by_role_and_label(Role::Label, unit)
                .any(|n| (n.rect().center().y - y).abs() < 1.0)
        })
    };
    assert_eq!(unit_in_row("Show EEG channels"), Some("\u{b5}V"));
    assert_eq!(unit_in_row("Show sEEG channels"), Some("mV"));
    assert_eq!(unit_in_row("Show EEG Fz"), None);
    assert_eq!(unit_in_row("Show SEEG LA1"), None);
    assert!(harness.query_by_label("uV").is_none());
}

/// Without a type row whose channels differ in unit there is no unit column: a long name has
/// all the room between its Bad toggle and its menu, so it stays whole in a sidebar just wide
/// enough for it, and is shortened only in a narrower one.
#[test]
fn without_units_in_channel_rows_names_take_their_room() {
    use kymo_testgen::{ChannelSpec, Spec, XdfFormat};
    let long = "Interpupillary distance, both eyes";
    let channels = vec![
        ChannelSpec::xdf("Fz", "EEG", "microvolts", 100, XdfFormat::F32),
        ChannelSpec::xdf(long, "EEG", "microvolts", 100, XdfFormat::F32),
    ];
    let file = TempFile::written(&Spec::new(kymo_testgen::Format::Xdf, channels, 1.0, 30));
    let mut harness = window(file.path());
    ready(&mut harness);
    let name = |h: &Harness<'_, KymoApp>| {
        h.get_by_role_and_label(egui::accesskit::Role::Label, long)
            .rect()
    };
    sidebar_to(&mut harness, 600.0);
    let whole = name(&harness).width();
    // The room past its "Bad", up to the gap before its menu, which lines up with the type
    // rows' menus.
    mark_in_list(&mut harness, long, true);
    let bad = harness.get_by_label(&format!("{long} bad")).rect().right();
    let menu = harness.get_by_label(&format!("Options for {long}")).rect();
    let type_menu = harness.get_by_label("Options for EEG channels").rect();
    assert!((menu.right() - type_menu.right()).abs() < 0.5);
    let free = menu.left() - 8.0 - bad;
    assert!(free > 20.0, "{free}");
    let edge = viewer(&harness).layout().unwrap().names.left() - 8.0;
    sidebar_to(&mut harness, edge - (free - 3.0));
    assert_eq!(name(&harness).width(), whole);
    sidebar_to(&mut harness, edge - (free + 12.0));
    assert!(name(&harness).width() < whole);
}

/// A type row whose channels differ in unit (Misc in V, % and none, as OpenSignals streams
/// have) shows none, and each of its channels shows its own, in one column; the other type
/// rows show theirs, their channels none.
#[test]
fn channels_of_a_type_differing_in_unit_show_theirs() {
    use egui::accesskit::Role;
    use kymo_testgen::{ChannelSpec, Spec, XdfFormat, XdfStream};
    let channel = |label: &str, kind: &str, unit: &str| {
        ChannelSpec::xdf(label, kind, unit, 100, XdfFormat::F32)
    };
    let channels = vec![
        channel("Fz", "EEG", "microvolts"),
        channel("Cz", "EEG", "microvolts"),
        channel("EDA", "Misc", "V"),
        channel("SpO2", "Misc", "percent"),
        channel("Count", "Misc", ""),
        channel("Fire", "Button", ""),
    ];
    let mut spec = Spec::new(kymo_testgen::Format::Xdf, channels, 1.0, 30);
    spec.xdf.streams = vec![
        XdfStream::new(1, "Amp", "EEG", vec![0, 1]),
        XdfStream::new(2, "OpenSignals", "Misc", vec![2, 3, 4, 5]),
    ];
    let file = TempFile::written(&spec);
    let mut harness = window(file.path());
    ready(&mut harness);
    let in_row = |h: &Harness<'_, KymoApp>, label: &str| -> Vec<String> {
        let y = h.get_by_label(label).rect().center().y;
        h.query_all_by_role(Role::Label)
            .filter(|n| (n.rect().center().y - y).abs() < 1.0)
            .filter_map(|n| n.accesskit_node().value())
            .collect()
    };
    assert_eq!(
        in_row(&harness, "Show EEG channels in Amp"),
        ["EEG", "\u{b5}V", "2"]
    );
    assert_eq!(in_row(&harness, "Show Fz"), ["Fz"]);
    assert_eq!(
        in_row(&harness, "Show Misc channels in OpenSignals"),
        ["Misc", "3"]
    );
    assert_eq!(in_row(&harness, "Show EDA"), ["EDA", "V"]);
    assert_eq!(in_row(&harness, "Show SpO2"), ["SpO2", "%"]);
    assert_eq!(in_row(&harness, "Show Count"), ["Count"]);
    // A type whose channels have no unit shows none.
    assert_eq!(
        in_row(&harness, "Show Button channels in OpenSignals"),
        ["Button", "1"]
    );
    let left = |text: &str| {
        harness
            .get_by_role_and_label(Role::Label, text)
            .rect()
            .left()
    };
    assert_eq!(left("V"), left("%"));
    // The type row's whole text on hover gives its unit.
    let eeg = kind_name_rect_in(&harness, "EEG", "Amp").center();
    hover(&mut harness, eeg);
    assert!(tooltip(&mut harness, "EEG\n2 channels \u{b7} \u{b5}V"));
}

#[test]
fn hovering_reads_the_value_under_the_pointer() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    let y = layout.traces.top() + 0.5 * layout.traces.height() / rows;
    hover(&mut harness, egui::pos2(layout.traces.center().x, y));
    // The tiles may still be arriving; the text settles once they are cached.
    step_until(&mut harness, "a reading", |app| {
        has_reading(&app.viewer().unwrap().status().left)
    });
    let left = viewer(&harness).status().left.clone();
    assert!(left.starts_with("0:10.000 · A1 "), "{left}");
    // The trigger that starts at 10 s (Status, channel 4, at 512 Hz).
    let spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    let code = spec.trigger_code(4, 5120).unwrap();
    assert_ne!(spec.trigger_code(4, 5119), Some(code));
    assert!(left.ends_with(&format!(" \u{b5}V · {code}")), "{left}");
    harness.get_by_label(&left);

    // Off the traces, the time goes away.
    hover(&mut harness, egui::pos2(5.0, 5.0));
    harness.step();
    assert_eq!(viewer(&harness).status().left, "");
}

/// Readings are in their channel's scale bar's unit: scaled down until EEG's bar is in mV,
/// the same reading in mV.
#[test]
fn readings_take_their_scale_bars_unit() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    let y = layout.traces.top() + 0.5 * layout.traces.height() / rows;
    hover(&mut harness, egui::pos2(layout.traces.center().x, y));
    step_until(&mut harness, "a reading", |app| {
        has_reading(&app.viewer().unwrap().status().left)
    });
    // A1's numbers and unit ("0:10.000 · A1 −12.34 µV · 77").
    let reading = |harness: &Harness<'_, KymoApp>| {
        let left = viewer(harness).status().left.clone();
        let part = left.split(" \u{b7} ").nth(1).unwrap();
        let (numbers, unit) = part.strip_prefix("A1 ").unwrap().rsplit_once(' ').unwrap();
        let values: Vec<f64> = numbers
            .split(" \u{2026} ")
            .map(|x| x.replace('\u{2212}', "-").parse().unwrap())
            .collect();
        (values, unit.to_owned())
    };
    let bar = |harness: &Harness<'_, KymoApp>| {
        let bars = viewer(harness).scale_bars();
        let eeg = bars.iter().find(|(b, _)| b.kind == ChannelKind::EEG);
        eeg.unwrap().0.label()
    };
    let (micro, unit) = reading(&harness);
    assert_eq!(
        (unit.as_str(), bar(&harness).as_str()),
        ("\u{b5}V", "20 \u{b5}V")
    );
    // Scaled down until the bar is in millivolts.
    for _ in 0..40 {
        if bar(&harness).ends_with(" mV") {
            break;
        }
        harness.key_press_modifiers(command() | Modifiers::ALT, Key::Minus);
        harness.run_steps(2);
    }
    assert!(bar(&harness).ends_with(" mV"), "{}", bar(&harness));
    let (milli, unit) = reading(&harness);
    assert_eq!(unit, "mV");
    assert_eq!(milli.len(), micro.len());
    for (m, u) in milli.iter().zip(&micro) {
        assert!(
            (m * 1e3 - u).abs() <= 0.01 * u.abs().max(1.0),
            "{m} mV, {u} µV"
        );
    }
}

#[test]
fn the_scrollbars_move_the_view() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    harness.key_press_modifiers(command() | Modifiers::SHIFT, Key::Minus);
    harness.step();
    let layout = viewer(&harness).layout().unwrap();

    // Dragging the time thumb (a third of the track, at its start) to the far end.
    let bar = layout.time_bar.unwrap();
    let grab = egui::pos2(bar.left() + 5.0, bar.center().y);
    harness.hover_at(grab);
    harness.step();
    harness.drag_at(grab);
    harness.step();
    for x in [bar.center().x, bar.right() + 50.0] {
        harness.hover_at(egui::pos2(x, bar.center().y));
        harness.step();
    }
    harness.drop_at(egui::pos2(bar.right() + 50.0, bar.center().y));
    harness.step();
    assert_eq!(viewer(&harness).view().time(), 40.0..60.0);

    // Clicking the row track below the thumb pages down to the last rows.
    let rows = viewer(&harness).view().rows();
    click(
        &mut harness,
        egui::pos2(layout.row_bar.center().x, layout.row_bar.bottom() - 2.0),
    );
    assert_eq!(viewer(&harness).view().top(), (5 - rows) as f64);
}

#[test]
fn an_idle_window_stops_repainting() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    // Once the build is done and the tiles are in, nothing asks for frames.
    step_until(&mut harness, "the tiles", |app| {
        app.viewer().unwrap().status().right != "Tiles 0 MB"
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(harness.try_run().is_ok(), "the window keeps repainting");
}

/// Clicks `item` in the egui menu `menu`.
fn choose(harness: &mut Harness<'_, KymoApp>, menu: &str, item: &str) {
    harness.get_by_label(menu).click();
    harness.run_steps(2);
    // Items with a shortcut carry it in their label.
    harness.get_by_label_contains(item).click();
    harness.run_steps(2);
}

/// Whether `item` in the egui menu `menu` is enabled; leaves the menu closed.
fn enabled(harness: &mut Harness<'_, KymoApp>, menu: &str, item: &str) -> bool {
    harness.get_by_label(menu).click();
    harness.run_steps(2);
    let enabled = !harness
        .get_by_label_contains(item)
        .accesskit_node()
        .is_disabled();
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(
        harness.query_by_label_contains(item).is_none(),
        "{menu} stayed open"
    );
    enabled
}

#[test]
fn menu_items_are_enabled_as_the_state_allows() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    assert!(enabled(&mut harness, "View", "Zoom In"));
    assert!(enabled(&mut harness, "File", "Close"));
    assert!(enabled(&mut harness, "View", "Show Sidebar"));
    assert!(enabled(&mut harness, "View", "Remove DC"));
    assert!(enabled(&mut harness, "Edit", "Settings"));

    harness.key_press_modifiers(command(), Key::W);
    harness.step();
    assert!(!enabled(&mut harness, "File", "Close"));
    assert!(!enabled(&mut harness, "View", "Zoom In"));
    assert!(!enabled(&mut harness, "View", "Show Sidebar"));
    assert!(enabled(&mut harness, "File", "Quit"));
    assert!(enabled(&mut harness, "Help", "Keyboard Shortcuts"));
    assert!(enabled(&mut harness, "Edit", "Settings"));
}

#[test]
fn menu_items_act_and_disabled_ones_dont() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    choose(&mut harness, "View", "Zoom In");
    assert_eq!(viewer(&harness).view().time(), 0.0..10.0);
    choose(&mut harness, "Go", "End of Recording");
    assert_eq!(viewer(&harness).view().time(), 50.0..60.0);
    // On by default: the item turns it off.
    choose(&mut harness, "View", "Remove DC");
    assert!(!viewer(&harness).view().dc_removed());
    choose(&mut harness, "Edit", "Settings");
    assert!(harness.state().settings_open());
    harness.get_by_label("Tile Cache");
    // Centered, it settles in a few frames as it grows to its content.
    harness.run_steps(4);
    // Its close button.
    harness.get_by_label("Close window").click();
    harness.run_steps(2);
    assert!(!harness.state().settings_open());

    choose(&mut harness, "File", "Close");
    assert!(harness.state().viewer().is_none());
    // Nothing to close now.
    choose(&mut harness, "File", "Close");
    harness.get_by_label("No Recording Open");
    assert_eq!(harness.state().error(), None);
}

/// Whether the checkable `item` in the egui menu `menu` is checked; leaves the menu closed.
fn checked(harness: &mut Harness<'_, KymoApp>, menu: &str, item: &str) -> bool {
    harness.get_by_label(menu).click();
    harness.run_steps(2);
    let checked = harness
        .get_by_label_contains(item)
        .accesskit_node()
        .toggled()
        .unwrap()
        == egui::accesskit::Toggled::True;
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    checked
}

#[test]
fn checkable_menu_items_show_a_checkbox_or_radio_button() {
    // Show Sidebar on, Anti-Aliasing off (the settings' default).
    let file = TempFile::preset("bdf+c-small");
    let mut harness = screenshot_window(file.path());
    ready(&mut harness);
    harness.get_by_label("View").click();
    harness.run_steps(2);
    let item = |harness: &Harness<'_, KymoApp>, label: &str| {
        let node = harness.get_by_label_contains(label);
        let node = node.accesskit_node();
        (
            node.role(),
            node.toggled(),
            harness.get_by_label_contains(label).rect(),
        )
    };
    let (sidebar_role, sidebar, sidebar_rect) = item(&harness, "Show Sidebar");
    let (antialias_role, antialias, antialias_rect) = item(&harness, "Anti-Aliasing");
    use egui::accesskit::{Role, Toggled};
    assert_eq!(
        (sidebar_role, sidebar),
        (Role::CheckBox, Some(Toggled::True))
    );
    assert_eq!(
        (antialias_role, antialias),
        (Role::CheckBox, Some(Toggled::False))
    );

    // Drawn as a box at the start, in the checkboxes' fill, and without the selection's
    // background, which marked them before.
    let image = harness.render().unwrap();
    let pixel = |x: f32, y: f32| image.get_pixel(x as u32, y as u32).0;
    let style = harness.ctx.global_style();
    let size = style.spacing.icon_width;
    let box_center = |r: egui::Rect| pixel(r.left() + 2.0 + size / 2.0, r.center().y);
    assert_eq!(
        box_center(antialias_rect),
        style.visuals.widgets.inactive.bg_fill.to_array()
    );
    let selection = style.visuals.selection.bg_fill.to_array();
    for r in [sidebar_rect, antialias_rect] {
        assert_ne!(pixel(r.right() - 1.0, r.center().y), selection);
    }
    assert_eq!(
        pixel(sidebar_rect.right() - 1.0, sidebar_rect.center().y),
        pixel(antialias_rect.right() - 1.0, antialias_rect.center().y)
    );

    // The stim annotation rules exclude each other, so they are radio buttons, with the
    // default rule on; the initial code is a checkbox of its own.
    harness.get_by_label("Stim Annotations").hover();
    harness.run_steps(2);
    let rules = ["Every Change", "From Zero", "Increasing"].map(|label| {
        let (role, on, _) = item(&harness, label);
        (role, on)
    });
    assert_eq!(
        rules,
        [
            (Role::RadioButton, Some(Toggled::True)),
            (Role::RadioButton, Some(Toggled::False)),
            (Role::RadioButton, Some(Toggled::False)),
        ]
    );
    assert_eq!(item(&harness, "Initial Code").0, Role::CheckBox);
}

/// A preset with every channel's physical range moved by `offset`, as amplifiers with a
/// DC offset record it (BioSemi files sit tens of millivolts off zero).
fn offset_preset(name: &str, offset: f64) -> TempFile {
    let mut spec = kymo_testgen::Spec::preset(name).unwrap();
    for channel in spec.channels.iter_mut().filter(|c| !c.trigger) {
        channel.physical_min += offset;
        channel.physical_max += offset;
    }
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, file.path()).unwrap();
    file
}

/// The mean of `channel` over the samples of `time` that aren't NaN, brute force.
fn window_mean(
    rec: &kymo_core::Recording,
    channel: kymo_core::ChannelId,
    time: std::ops::Range<f64>,
) -> Option<f64> {
    let grid_id = rec.channel(channel).grid;
    let grid = rec.grid(grid_id);
    let range = kymo_core::tile::samples_in(grid, &time);
    let (mut sum, mut n) = (0.0, 0usize);
    for part in grid.present_in(range) {
        let mut out = vec![0.0f32; (part.end - part.start) as usize];
        rec.read_physical(grid_id, &[channel], part, &mut out)
            .unwrap();
        let values = out.iter().filter(|x| !x.is_nan());
        sum += values.clone().map(|&x| f64::from(x)).sum::<f64>();
        n += values.count();
    }
    (n > 0).then(|| sum / n as f64)
}

/// Whether every visible channel's baseline is its brute-force mean over the view (0 for
/// stim channels).
fn baselines_are_window_means(app: &KymoApp) -> bool {
    let viewer = app.viewer().unwrap();
    let view = viewer.view();
    let rec = &viewer.document().recording;
    view.visible_channels().iter().all(|&c| {
        // DC removal leaves stim channels alone.
        let want = if rec.channel(c).kind == kymo_core::ChannelKind::STIM {
            0.0
        } else {
            window_mean(rec, c, view.time()).unwrap()
        };
        // The brute force reads f32 values.
        (viewer.baseline(c) - want).abs() <= 1e-5 * want.abs().max(1.0)
    })
}

/// The numbers of the status line's reading ("12.5 µV" or "−3 … 4 µV").
fn reading_numbers(status: &str) -> Vec<f64> {
    let reading = status.rsplit_once(" A1 ").unwrap().1;
    // Up to the annotations under the pointer, if any.
    let reading = reading.split(" · ").next().unwrap();
    reading
        .trim_end_matches(" \u{b5}V")
        .split(" … ")
        .map(|x| x.replace('−', "-").parse().unwrap())
        .collect()
}

/// A channel whose window touches missing samples (GDF's smallest int16, read as NaN) is
/// centered on the mean of the samples that are there, not left out of the view: a NaN
/// mean would offset the whole trace by NaN.
#[test]
fn dc_removal_leaves_missing_samples_out_of_the_mean() {
    use kymo_testgen::{ChannelSpec, Format, GdfVersion, Spec, Stored};
    let format = Format::Gdf(GdfVersion::V2);
    let channels = vec![ChannelSpec::gdf("Cz", 256, Stored::I16)];
    let spec = Spec::new(format, channels, 1.0, 30);
    let file = TempFile::written(&spec);
    // The first 1.5 s missing: one channel, so the records hold its samples back to back
    // after the 512-byte header.
    let mut bytes = std::fs::read(file.path()).unwrap();
    for i in 0..384 {
        bytes[512 + 2 * i..][..2].copy_from_slice(&0x8000u16.to_le_bytes());
    }
    std::fs::write(file.path(), bytes).unwrap();

    let mut harness = window(file.path());
    ready(&mut harness);
    assert!(viewer(&harness).view().dc_removed());
    let cz = kymo_core::ChannelId(0);
    let time = viewer(&harness).view().time();
    let rec = &viewer(&harness).document().recording;
    let want = window_mean(rec, cz, time.clone()).unwrap();
    // The view starts in the missing samples.
    let mut first = [0.0f32];
    rec.read_physical(kymo_core::GridId(0), &[cz], 0..1, &mut first)
        .unwrap();
    assert!(first[0].is_nan() && time.start < 1.5, "{first:?} {time:?}");
    step_until(&mut harness, "the mean", baselines_are_window_means);
    let mean = viewer(&harness).baseline(cz);
    assert!(
        mean.is_finite() && (mean - want).abs() <= 1e-5 * want.abs().max(1.0),
        "{mean}"
    );
}

#[test]
fn dc_removal_centers_each_trace_on_its_window_mean() {
    let file = offset_preset("bdf+c-small", -21_000.0);
    let mut harness = window(file.path());
    ready(&mut harness);
    let a1 = kymo_core::ChannelId(0);
    assert!(checked(&mut harness, "View", "Remove DC"));
    dc_off(&mut harness);
    assert_eq!(viewer(&harness).baseline(a1), 0.0);
    assert!(!checked(&mut harness, "View", "Remove DC"));

    // The value under the pointer, without and with DC removal.
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    let a1_row = egui::pos2(
        layout.traces.center().x,
        layout.traces.top() + 0.5 * layout.traces.height() / rows,
    );
    hover(&mut harness, a1_row);
    step_until(&mut harness, "a reading", |app| {
        has_reading(&app.viewer().unwrap().status().left)
    });
    let raw = reading_numbers(&viewer(&harness).status().left);
    assert!(raw.iter().all(|&x| x < -20_000.0), "{raw:?}");

    harness.key_press_modifiers(command(), Key::D);
    harness.step();
    assert!(viewer(&harness).view().dc_removed());
    step_until(&mut harness, "the means", baselines_are_window_means);
    let mean = viewer(&harness).baseline(a1);
    assert!((-21_100.0..-20_900.0).contains(&mean), "{mean}");
    let centered = reading_numbers(&viewer(&harness).status().left);
    assert_eq!(centered.len(), raw.len());
    for (r, c) in raw.iter().zip(&centered) {
        // Four significant digits of the raw value: within 10 uV.
        assert!((r - mean - c).abs() <= 10.0, "{r} − {mean} ≠ {c}");
    }
    assert!(checked(&mut harness, "View", "Remove DC"));

    // The means follow the window.
    harness.key_press(Key::ArrowRight);
    harness.step();
    step_until(&mut harness, "the panned means", baselines_are_window_means);
    assert_eq!(viewer(&harness).view().time(), 5.0..25.0);

    harness.key_press_modifiers(command(), Key::D);
    harness.step();
    assert_eq!(viewer(&harness).baseline(a1), 0.0);
    assert!(!checked(&mut harness, "View", "Remove DC"));
}

/// The status line's DC chip shows whether DC removal is on, toggles it with a click, as
/// ⌘D and the menu item do, and says what it does on hover.
#[test]
fn the_dc_chip_shows_and_toggles_dc_removal() {
    use egui::accesskit::{Role, Toggled};
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let chip = |h: &Harness<'_, KymoApp>| {
        let node = h.get_by_role_and_label(Role::Button, kymo::viewer::DC_CHIP);
        node.accesskit_node().toggled() == Some(Toggled::True)
    };
    assert!(chip(&harness));
    harness
        .get_by_role_and_label(Role::Button, kymo::viewer::DC_CHIP)
        .click();
    harness.run_steps(2);
    assert!(!viewer(&harness).view().dc_removed());
    assert!(!chip(&harness));
    assert!(!checked(&mut harness, "View", "Remove DC"));
    // ⌘D turns it back on, and the chip follows.
    harness.key_press_modifiers(command(), Key::D);
    harness.run_steps(2);
    assert!(viewer(&harness).view().dc_removed());
    assert!(chip(&harness));
    let rect = harness
        .get_by_role_and_label(Role::Button, kymo::viewer::DC_CHIP)
        .rect();
    hover(&mut harness, rect.center());
    let tip = "Remove DC";
    let shown = Instant::now() + Duration::from_secs(2);
    while harness.query_by_label_contains(tip).is_none() && Instant::now() < shown {
        harness.step();
    }
    harness.get_by_label_contains(tip);
}

/// The status line shows the window's start and end, which follow scrolling, and a click
/// on them opens Go to Time.
#[test]
fn the_status_line_range_shows_the_window_and_opens_go_to_time() {
    use egui::accesskit::Role;
    // A minute, 20 s in view.
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    // As tall as the DC chip beside it.
    let range = harness
        .get_by_role_and_label(Role::Button, "0:00.0 to 0:20.0")
        .rect();
    let dc = harness
        .get_by_role_and_label(Role::Button, kymo::viewer::DC_CHIP)
        .rect();
    assert_eq!(range.height(), dc.height());
    harness.key_press_modifiers(command(), Key::G);
    harness.run_steps(2);
    harness.get_by_role(Role::TextInput).type_text("12.5");
    harness.run_steps(2);
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    assert!(!harness.state().go_to_open());
    harness
        .get_by_role_and_label(Role::Button, "0:12.5 to 0:32.5")
        .click();
    harness.run_steps(2);
    assert!(harness.state().go_to_open());
    let field = harness
        .get_by_role(Role::TextInput)
        .value()
        .unwrap_or_default();
    assert_eq!(field, "12.5 s");
}

/// Remove DC is a setting, as the rule for settings has it: the chip, the View menu and ⌘D
/// change it, and the recording opened next and the stored settings keep it.
#[test]
fn remove_dc_is_a_setting_kept_for_the_next_recording() {
    use egui::accesskit::Role;
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    assert!(harness.state().settings().remove_dc);
    harness
        .get_by_role_and_label(Role::Button, kymo::viewer::DC_CHIP)
        .click();
    harness.run_steps(2);
    assert!(!harness.state().settings().remove_dc);
    assert!(!viewer(&harness).view().dc_removed());
    // The next recording opens with it off.
    let ctx = harness.ctx.clone();
    harness.state_mut().open(&ctx, file.path().to_owned());
    ready(&mut harness);
    assert!(!viewer(&harness).view().dc_removed());
    // View → Remove DC shows it, and turns it back on.
    assert!(!checked(&mut harness, "View", "Remove DC"));
    choose(&mut harness, "View", "Remove DC");
    assert!(harness.state().settings().remove_dc);
    assert!(viewer(&harness).view().dc_removed());
    // ⌘D changes the setting too.
    harness.key_press_modifiers(command(), Key::D);
    harness.run_steps(2);
    assert!(!harness.state().settings().remove_dc);
    // Settings doesn't repeat what the View menu sets, and Restore Defaults leaves it.
    harness.key_press_modifiers(command(), Key::Comma);
    harness.run_steps(2);
    for label in ["Remove DC", "Scale Bars", "Anti-Aliased"] {
        assert!(
            harness
                .query_by_role_and_label(Role::CheckBox, label)
                .is_none(),
            "{label}"
        );
    }
    harness.get_by_label("Restore Defaults").click();
    harness.run_steps(2);
    assert!(!harness.state().settings().remove_dc);
    // The stored settings keep it.
    let mut storage = Memory::default();
    eframe::App::save(harness.state_mut(), &mut storage);
    assert!(!Settings::load(Some(&storage)).remove_dc);
}

/// Turns DC removal off, after checking that it is on, as recordings open.
fn dc_off(harness: &mut Harness<'_, KymoApp>) {
    assert!(viewer(harness).view().dc_removed(), "on as recordings open");
    harness.key_press_modifiers(command(), Key::D);
    harness.step();
    assert!(!viewer(harness).view().dc_removed());
}

/// The numbers of the status line's reading of the Status channel, if it shows one. Codes
/// have no unit (the file's is "Boolean"); the annotations under the pointer may follow.
fn status_reading(status: &str) -> Option<Vec<f64>> {
    let reading = status.rsplit_once(" Status ")?.1;
    // Up to the annotations under the pointer, if any.
    let reading = reading.split(" · ").next()?;
    Some(
        reading
            .split(" … ")
            .map(|x| x.replace('−', "-").parse().unwrap())
            .collect(),
    )
}

#[test]
fn stim_channels_read_their_codes_with_and_without_dc_removal() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    dc_off(&mut harness);
    let status = kymo_core::ChannelId(4);
    assert_eq!(
        viewer(&harness).document().recording.channel(status).name,
        "Status"
    );
    let layout = viewer(&harness).layout().unwrap();
    let rows = viewer(&harness).view().rows() as f32;
    let row = viewer(&harness).view().row_of(status).unwrap() as f32;
    let y = layout.traces.top() + (row + 0.5) * layout.traces.height() / rows;
    // Readings across the window: the codes 0 to 255, without the status bits above them.
    let readings = |harness: &mut Harness<'_, KymoApp>| {
        (1..20)
            .map(|k| {
                let x = layout.traces.left() + layout.traces.width() * k as f32 / 20.0;
                hover(harness, egui::pos2(x, y));
                step_until(harness, "a reading", |app| {
                    status_reading(&app.viewer().unwrap().status().left).is_some()
                });
                status_reading(&viewer(harness).status().left).unwrap()
            })
            .collect::<Vec<_>>()
    };
    let raw = readings(&mut harness);
    let numbers: Vec<f64> = raw.iter().flatten().copied().collect();
    assert!(
        numbers
            .iter()
            .all(|&x| (0.0..=255.0).contains(&x) && x.fract() == 0.0),
        "{raw:?}"
    );
    assert!(numbers.iter().any(|&x| x > 0.0), "{raw:?}");

    // DC removal leaves the codes as they are.
    harness.key_press_modifiers(command(), Key::D);
    harness.step();
    step_until(&mut harness, "the means", baselines_are_window_means);
    assert_eq!(viewer(&harness).baseline(status), 0.0);
    assert_ne!(viewer(&harness).baseline(kymo_core::ChannelId(0)), 0.0);
    assert_eq!(readings(&mut harness), raw);
}

/// bdf+c-small with its channels relabeled to five types: A1 and A2 (EEG), "EOG L",
/// "ECG" and Status (stim).
fn typed_preset() -> TempFile {
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.channels[2].label = "EOG L".into();
    spec.channels[3].label = "ECG".into();
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, file.path()).unwrap();
    file
}

/// Clicks the sidebar widget labeled `label` (exactly, with `role`).
fn click_in_list(harness: &mut Harness<'_, KymoApp>, role: egui::accesskit::Role, label: &str) {
    // A row scrolled out of its list takes no clicks. Scrolled to at once, without
    // the animation, which would leave it moving under the pointer.
    harness
        .ctx
        .global_style_mut(|s| s.scroll_animation = egui::style::ScrollAnimation::none());
    harness.get_by_role_and_label(role, label).scroll_to_me();
    // One frame scrolls, the next lays the rows out there, the third hit-tests them.
    harness.run_steps(3);
    harness.get_by_role_and_label(role, label).click();
    harness.run_steps(2);
}

fn toggled(
    harness: &Harness<'_, KymoApp>,
    role: egui::accesskit::Role,
    label: &str,
) -> egui::accesskit::Toggled {
    harness
        .get_by_role_and_label(role, label)
        .accesskit_node()
        .toggled()
        .unwrap()
}

fn shown(harness: &Harness<'_, KymoApp>) -> Vec<u32> {
    viewer(harness)
        .view()
        .visible_channels()
        .iter()
        .map(|c| c.0)
        .collect()
}

#[test]
fn the_channel_list_shows_hides_and_marks_bad() {
    use egui::accesskit::{Role, Toggled};
    let file = typed_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    assert_eq!(shown(&harness), [0, 1, 2, 3, 4]);
    for kind in ["EEG", "EOG", "ECG", "Stim"] {
        assert_eq!(
            toggled(&harness, Role::CheckBox, &format!("Show {kind} channels")),
            Toggled::True,
            "{kind}"
        );
    }
    assert!(
        harness
            .query_by_role_and_label(Role::CheckBox, "Show EMG channels")
            .is_none()
    );
    harness.get_by_label("2 \u{b7} 512 Hz");
    // A type's channels are a level (10 points) in.
    let left = |h: &Harness<'_, KymoApp>, label: &str| h.get_by_label(label).rect().left();
    let level = left(&harness, "Show A1") - left(&harness, "Show EEG channels");
    assert!((level - LEVEL).abs() <= 0.5, "{level}");
    assert!(left(&harness, "Collapse EEG channels") < left(&harness, "Show EEG channels"));
    // Its arrow hides its channels' rows, not their traces.
    harness.get_by_label("Collapse EEG channels").click();
    harness.run_steps(2);
    assert!(harness.query_by_label("Show A1").is_none());
    harness.get_by_label("Show EOG L");
    assert_eq!(shown(&harness), [0, 1, 2, 3, 4]);
    harness.get_by_label("Expand EEG channels").click();
    harness.run_steps(2);
    harness.get_by_label("Show A1");

    // One channel's box hides it; its type's box is mixed then.
    click_in_list(&mut harness, Role::CheckBox, "Show A2");
    assert_eq!(shown(&harness), [0, 2, 3, 4]);
    assert_eq!(toggled(&harness, Role::CheckBox, "Show A2"), Toggled::False);
    assert_eq!(
        toggled(&harness, Role::CheckBox, "Show EEG channels"),
        Toggled::Mixed
    );
    // A mixed type's box shows them all, and then hides them all.
    click_in_list(&mut harness, Role::CheckBox, "Show EEG channels");
    assert_eq!(shown(&harness), [0, 1, 2, 3, 4]);
    click_in_list(&mut harness, Role::CheckBox, "Show EEG channels");
    assert_eq!(shown(&harness), [2, 3, 4]);
    assert_eq!(toggled(&harness, Role::CheckBox, "Show A1"), Toggled::False);
    assert_eq!(
        toggled(&harness, Role::CheckBox, "Show EEG channels"),
        Toggled::False
    );
    click_in_list(&mut harness, Role::CheckBox, "Show EEG channels");
    assert_eq!(shown(&harness), [0, 1, 2, 3, 4]);

    // A bad channel says so; a good one doesn't.
    let ecg = kymo_core::ChannelId(3);
    assert!(harness.query_by_label("ECG bad").is_none());
    mark_in_list(&mut harness, "ECG", true);
    assert!(viewer(&harness).view().is_bad(ecg));
    harness.get_by_role_and_label(Role::Label, "ECG bad");
    mark_in_list(&mut harness, "ECG", false);
    assert!(!viewer(&harness).view().is_bad(ecg));
    assert!(harness.query_by_label("ECG bad").is_none());

    // A click on a row scrolls the traces to it if it is off screen, as little as it
    // takes: three rows of five.
    for _ in 0..2 {
        harness.key_press_modifiers(command() | Modifiers::SHIFT, Key::Minus);
        harness.step();
    }
    assert_eq!(viewer(&harness).view().rows(), 3);
    click_in_list(&mut harness, Role::Label, "Status");
    assert_eq!(viewer(&harness).view().top(), 2.0);
    click_in_list(&mut harness, Role::Label, "EOG L");
    assert_eq!(viewer(&harness).view().top(), 2.0);
    click_in_list(&mut harness, Role::Label, "A1");
    assert_eq!(viewer(&harness).view().top(), 0.0);
    // A type's name scrolls to its first shown channel.
    click_in_list(&mut harness, Role::Label, "EOG");
    assert_eq!(viewer(&harness).view().top(), 2.0);
    click_in_list(&mut harness, Role::Label, "A2");
    assert_eq!(viewer(&harness).view().top(), 1.0);
    // A hidden channel's name doesn't scroll.
    click_in_list(&mut harness, Role::CheckBox, "Show A1");
    click_in_list(&mut harness, Role::Label, "A1");
    assert_eq!(shown(&harness), [1, 2, 3]);
    // A type's name skips its hidden channels: EEG scrolls to A2.
    click_in_list(&mut harness, Role::Label, "EOG");
    assert_eq!(viewer(&harness).view().top(), 1.0);
    click_in_list(&mut harness, Role::Label, "EEG");
    assert_eq!(viewer(&harness).view().top(), 0.0);
}

/// How far each level of the sidebar's lists moves rows in, in points.
const LEVEL: f32 = 10.0;

/// Where the channel list's type row of `name` ("EOG") shows its name: the label of that
/// text in the row of its checkbox.
fn kind_name_rect(harness: &Harness<'_, KymoApp>, name: &str) -> egui::Rect {
    use egui::accesskit::Role;
    let checkbox = harness
        .get_by_role_and_label(Role::CheckBox, &format!("Show {name} channels"))
        .rect();
    harness
        .query_all_by_role_and_label(Role::Label, name)
        .map(|n| n.rect())
        .find(|r| (r.center().y - checkbox.center().y).abs() < 1.0)
        .unwrap_or_else(|| panic!("no name in {name}'s row"))
}

/// As [`kind_name_rect`], for the type row of `name` in `stream`.
fn kind_name_rect_in(harness: &Harness<'_, KymoApp>, name: &str, stream: &str) -> egui::Rect {
    use egui::accesskit::Role;
    let checkbox = harness
        .get_by_role_and_label(Role::CheckBox, &format!("Show {name} channels in {stream}"))
        .rect();
    harness
        .query_all_by_role_and_label(Role::Label, name)
        .map(|n| n.rect())
        .find(|r| (r.center().y - checkbox.center().y).abs() < 1.0)
        .unwrap_or_else(|| panic!("no name in {name}'s row in {stream}"))
}

#[test]
fn the_channel_list_keeps_its_columns_while_scrolling() {
    use egui::accesskit::Role;
    // 60 channels: only the last EEG channel has a long name, and Status comes after it.
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    let status = spec.channels.pop().unwrap();
    for k in 0..55 {
        let label = if k == 54 {
            "LONGCHANNELNAME1".to_owned()
        } else {
            format!("E{k}")
        };
        spec.channels
            .push(kymo_testgen::ChannelSpec::eeg(&label, 512, spec.format));
    }
    spec.channels.push(status);
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, file.path()).unwrap();
    let mut harness = window(file.path());
    ready(&mut harness);
    // All marked bad, to show "Bad".
    let n = viewer(&harness).document().recording.channels().len();
    press(&mut harness, command(), Key::A);
    menu_path(
        &mut harness,
        &["Mark", &format!("Mark {n} Channels as Bad")],
    );
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(harness.query_by_label("LONGCHANNELNAME1").is_none());
    let bad = harness.get_by_label("A1 bad").rect().left();

    // Scrolled to the end, where the long name and Status are.
    let a1 = harness
        .get_by_role_and_label(Role::Label, "A1")
        .rect()
        .center();
    harness.hover_at(a1);
    for _ in 0..10 {
        scroll(&mut harness, egui::vec2(0.0, -2000.0), Modifiers::NONE);
    }
    harness.run_steps(2);
    let long = harness.get_by_label("LONGCHANNELNAME1").rect();
    assert_eq!(
        harness.get_by_label("LONGCHANNELNAME1 bad").rect().left(),
        bad
    );
    assert_eq!(harness.get_by_label("Status bad").rect().left(), bad);
    assert!(
        long.right() < bad,
        "{long:?} reaches the bad marks at {bad}"
    );
    // Codes have no unit, whatever the file says.
    assert!(harness.query_by_label("Boolean").is_none());
}

/// The rates are in the types' rows, with their numbers of channels, all ending at the same
/// edge: a type whose channels differ in rate gives the range, and its channels their rates
/// on hover; the channels' rows have none. An EDF stim channel shows no unit either.
#[test]
fn types_rows_give_their_rates() {
    use egui::accesskit::Role;
    // EEG Fz to Oz at 256 Hz and EEG DC at 512 Hz, Resp at 64 Hz, a trigger at 512 Hz.
    let file = TempFile::preset("edf+d-mixed");
    // Tall enough for every row above the selection bar.
    let mut harness = window_from(
        Harness::builder().with_size(egui::vec2(1200.0, 900.0)),
        file.path(),
    );
    ready(&mut harness);
    let rights: Vec<f32> = [
        "5 \u{b7} 256\u{2013}512 Hz",
        "1 \u{b7} 64 Hz",
        "1 \u{b7} 512 Hz",
    ]
    .iter()
    .map(|text| {
        harness
            .get_by_role_and_label(Role::Label, text)
            .rect()
            .right()
    })
    .collect();
    assert!(rights.iter().all(|&r| r == rights[0]), "{rights:?}");
    for rate in ["64 Hz", "256 Hz", "512 Hz"] {
        assert!(
            harness.query_by_role_and_label(Role::Label, rate).is_none(),
            "{rate}"
        );
    }
    let fz = harness.get_by_role_and_label(Role::Label, "EEG Fz").rect();
    hover(&mut harness, fz.center());
    assert!(tooltip(
        &mut harness,
        "EEG Fz\nEEG \u{b7} \u{b5}V \u{b7} 256 Hz"
    ));
    let eeg = kind_name_rect(&harness, "EEG");
    hover(&mut harness, eeg.center());
    assert!(tooltip(
        &mut harness,
        "EEG\n5 channels \u{b7} \u{b5}V \u{b7} 256 to 512 Hz"
    ));
    // A row too narrow (the narrowest sidebar, in a large font) leaves the count and rate
    // out, and still shows them on hover.
    hover(&mut harness, egui::pos2(900.0, 650.0));
    sidebar_to(&mut harness, 100.0);
    harness.ctx.all_styles_mut(|s| {
        s.text_styles.get_mut(&egui::TextStyle::Body).unwrap().size *= 1.6;
    });
    harness.run_steps(3);
    assert!(
        harness
            .query_by_label_contains("256\u{2013}512 Hz")
            .is_none()
    );
    // The row's right end, before its menu.
    let row = harness.get_by_label("Show EEG channels").rect().center().y;
    let menu = harness
        .get_by_label("Options for EEG channels")
        .rect()
        .left();
    hover(&mut harness, egui::pos2(menu - 10.0, row));
    assert!(tooltip(
        &mut harness,
        "EEG\n5 channels \u{b7} \u{b5}V \u{b7} 256 to 512 Hz"
    ));
    hover(&mut harness, egui::pos2(900.0, 650.0));
    harness.ctx.all_styles_mut(|s| {
        s.text_styles.get_mut(&egui::TextStyle::Body).unwrap().size /= 1.6;
    });
    sidebar_to(&mut harness, 600.0);

    // Scrolled to the end, where the trigger channel is.
    let first = harness
        .get_by_role_and_label(Role::Label, "EEG Fz")
        .rect()
        .center();
    harness.hover_at(first);
    scroll(&mut harness, egui::vec2(0.0, -2000.0), Modifiers::NONE);
    harness.run_steps(2);
    assert!(harness.query_by_label("TRIGGER").is_some());
    assert!(harness.query_by_label("Boolean").is_none());
}

/// An XDF file as eye tracking recordings come: EEG in "microvolts", AUX channels (a type
/// MNE doesn't have), one in microvolts and one with a long name in a long unit, and an
/// irregular 50 Hz stream with a long name.
fn long_names_xdf() -> TempFile {
    use kymo_testgen::{ChannelSpec, Spec, XdfFormat, XdfStamps, XdfStream};
    let f = kymo_testgen::Format::Xdf;
    let channels = vec![
        ChannelSpec::xdf("Fz", "EEG", "microvolts", 100, XdfFormat::F32),
        ChannelSpec::xdf("AUX1", "AUX", "microvolts", 100, XdfFormat::F32),
        ChannelSpec::xdf(
            "POR X - (Point of regard), left eye",
            "AUX",
            "normalized_signed",
            100,
            XdfFormat::F32,
        ),
        ChannelSpec::xdf(
            "IPD - Interpupillary distance, both eyes",
            "Gaze",
            "mm",
            50,
            XdfFormat::F32,
        ),
    ];
    let mut spec = Spec::new(f, channels, 1.0, 30);
    spec.xdf.streams = vec![
        XdfStream::new(1, "Amp", "EEG", vec![0, 1, 2]),
        XdfStream {
            stamps: XdfStamps::All,
            irregular: true,
            ..XdfStream::new(2, "Tracker", "Gaze", vec![3])
        },
    ];
    TempFile::written(&spec)
}

/// Drags the sidebar's edge from its default 300 points to `x`.
fn sidebar_to(harness: &mut Harness<'_, KymoApp>, x: f32) {
    // The trace region starts a panel margin past the sidebar's edge.
    let edge = viewer(harness).layout().unwrap().names.left() - 8.0;
    let from = egui::pos2(edge, 300.0);
    drag(harness, from, egui::pos2(x, 300.0));
    harness.run_steps(3);
}

/// Steps until a tooltip containing `text` shows (tooltips wait for the pointer to rest).
fn tooltip(harness: &mut Harness<'_, KymoApp>, text: &str) -> bool {
    let end = Instant::now() + Duration::from_secs(2);
    while harness.query_by_label_contains(text).is_none() && Instant::now() < end {
        harness.step();
    }
    harness.query_by_label_contains(text).is_some()
}

/// The channel list shows units as symbols ("microvolts" as µV, for Misc channels too) and
/// an irregular grid's rate as "≈50 Hz" (in its stream's row, as the file has two streams),
/// everything in full where the sidebar is wide enough; in a narrower one it shortens units
/// and then names in the middle (screen readers get them whole), then leaves out the unit
/// and type, which hovering the row shows.
#[test]
fn the_channel_list_fits_its_width() {
    use egui::accesskit::Role;
    let file = long_names_xdf();
    let mut harness = window(file.path());
    ready(&mut harness);
    let row_of = |h: &Harness<'_, KymoApp>, name: &str| {
        h.get_by_role_and_label(Role::Label, name).rect().center().y
    };
    let in_row = |h: &Harness<'_, KymoApp>, y: f32, label: &str| {
        h.query_all_by_role_and_label(Role::Label, label)
            .any(|n| (n.rect().center().y - y).abs() < 1.0)
    };
    let em = body_size(&harness);

    // Wide: every column, but the type, which is the row the channel is under.
    sidebar_to(&mut harness, 600.0);
    let aux = row_of(&harness, "AUX1");
    assert!(!in_row(&harness, aux, "Misc"));
    assert!(in_row(&harness, aux, "\u{b5}V"));
    assert!(harness.query_by_label("microvolts").is_none());
    // The rates are in the streams' rows.
    let por = row_of(&harness, "POR X - (Point of regard), left eye");
    assert!(!in_row(&harness, por, "100 Hz"));
    harness.get_by_role_and_label(Role::Label, "3 \u{b7} 100 Hz");
    // The long unit and names in full.
    let full = |h: &Harness<'_, KymoApp>, label: &str| {
        let rect = h.get_by_role_and_label(Role::Label, label).rect();
        let font = egui::TextStyle::Body.resolve(&h.ctx.global_style());
        let text = h.ctx.fonts_mut(|f| {
            f.layout_no_wrap(label.to_owned(), font, egui::Color32::WHITE)
                .size()
                .x
        });
        rect.width() >= text - 0.5
    };
    assert!(full(&harness, "normalized_signed"));
    assert!(full(&harness, "IPD - Interpupillary distance, both eyes"));
    assert!(full(&harness, "POR X - (Point of regard), left eye"));
    let rate = harness
        .get_by_role_and_label(Role::Label, "1 \u{b7} \u{2248}50 Hz")
        .rect();
    // An irregular rate in full on hover; nothing else is shortened.
    hover(&mut harness, rate.center());
    assert!(tooltip(&mut harness, "channel \u{b7} \u{2248}50 Hz"));
    assert!(harness.query_by_label_contains("both eyes\n").is_none());
    hover(&mut harness, egui::pos2(900.0, 650.0));

    // The default width, plus the room the rows under a stream and type are indented by
    // beyond the stream's checkbox (two half levels): the unit and then the names narrow.
    let file = long_names_xdf();
    let mut harness = window(file.path());
    ready(&mut harness);
    let default = viewer(&harness).layout().unwrap().names.left() - 8.0;
    let level = harness
        .get_by_label("Show POR X - (Point of regard), left eye")
        .rect()
        .left()
        - harness.get_by_label("Show Amp").rect().left();
    sidebar_to(&mut harness, default + level);
    let por = row_of(&harness, "POR X - (Point of regard), left eye");
    assert!(!in_row(&harness, por, "100 Hz"));
    let name = harness
        .get_by_role_and_label(Role::Label, "POR X - (Point of regard), left eye")
        .rect();
    assert!(name.width() >= 6.0 * em - 1.0, "{name:?}");
    assert!(!full(&harness, "POR X - (Point of regard), left eye"));
    let unit = harness
        .get_by_role_and_label(Role::Label, "normalized_signed")
        .rect();
    assert!(unit.width() <= 4.0 * em + 0.5, "{unit:?}");
    assert!(!full(&harness, "normalized_signed"));
    // Hovering the row shows what it leaves out.
    hover(&mut harness, unit.center());
    assert!(tooltip(
        &mut harness,
        "POR X - (Point of regard), left eye\nAUX \u{b7} normalized_signed \u{b7} 100 Hz"
    ));
    hover(&mut harness, egui::pos2(900.0, 650.0));

    // Narrowest (200 points): no unit or rate, and nothing in the row past the sidebar,
    // "Bad" included (all marked so).
    let n = viewer(&harness).document().recording.channels().len();
    press(&mut harness, command(), Key::A);
    menu_path(
        &mut harness,
        &["Mark", &format!("Mark {n} Channels as Bad")],
    );
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    sidebar_to(&mut harness, 100.0);
    let fz = row_of(&harness, "Fz");
    for label in ["\u{b5}V", "100 Hz"] {
        assert!(!in_row(&harness, fz, label), "{label}");
    }
    let sidebar = viewer(&harness).layout().unwrap().names.left() - 8.0;
    let rights: Vec<f32> = harness
        .query_all_by_role(Role::Label)
        .chain(harness.query_all_by_role(Role::Button))
        .filter(|n| (n.rect().center().y - fz).abs() < 1.0)
        .map(|n| n.rect().right())
        .collect();
    assert!(rights.len() >= 2, "{rights:?}");
    assert!(
        rights.iter().all(|&r| r <= sidebar),
        "{rights:?} past {sidebar}"
    );
}

/// The texts painted in the last frame that contain `needle`.
fn painted(harness: &Harness<'_, KymoApp>, needle: &str) -> Vec<String> {
    fn texts(shape: &egui::Shape, needle: &str, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) if t.galley.text().contains(needle) => {
                out.push(t.galley.text().to_owned())
            }
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| texts(s, needle, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in &harness.output().shapes {
        texts(&clipped.shape, needle, &mut out);
    }
    out
}

/// Names that fit their column stay whole while the sidebar's edge moves the trace area by
/// fractions of a pixel (its left edge is rounded to one), and the names column drags as
/// narrow as a double click fits it, where the names fit in less than 3 em.
#[test]
fn short_names_stay_whole_and_their_column_drags_to_its_fit() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window_with_short_frames(file.path());
    ready(&mut harness);
    let sidebar = viewer(&harness).layout().unwrap().names.left() - 8.0;
    let from = egui::pos2(sidebar, 300.0);
    hover(&mut harness, from);
    harness.drag_at(from);
    harness.step();
    for k in 1..=40 {
        let at = from + egui::vec2(k as f32 * 0.13, 0.0);
        hover(&mut harness, at);
        let names = viewer(&harness).layout().unwrap().names;
        assert!(names.left() > sidebar, "the sidebar didn't move");
        assert_eq!(
            painted(&harness, "\u{2026}"),
            Vec::<String>::new(),
            "at {at:?}"
        );
    }
    harness.drop_at(from + egui::vec2(40.0 * 0.13, 0.0));
    harness.step();

    // The names fit in less than 3 em; dragging the column narrower stops at that width,
    // which a double click gives too.
    let em = body_size(&harness);
    let fitted = viewer(&harness).layout().unwrap().names.width();
    assert!(fitted < 3.0 * em + 12.0, "{fitted}");
    let edge = |h: &Harness<'_, KymoApp>| {
        let names = viewer(h).layout().unwrap().names;
        egui::pos2(names.right(), names.center().y)
    };
    let from = edge(&harness);
    drag(&mut harness, from, from + egui::vec2(60.0, 0.0));
    harness.run_steps(2);
    let from = edge(&harness);
    drag(&mut harness, from, egui::pos2(0.0, from.y));
    harness.run_steps(2);
    let width = viewer(&harness).layout().unwrap().names.width();
    assert!((width - fitted).abs() < 1.0, "{width} vs {fitted}");
    assert_eq!(painted(&harness, "\u{2026}"), Vec::<String>::new());
}

/// The body font's size: an em.
fn body_size(harness: &Harness<'_, KymoApp>) -> f32 {
    harness.ctx.global_style().text_styles[&egui::TextStyle::Body].size
}

/// Names longer than the names column (10 em) are shortened in the middle, whole on hover;
/// the boundary between names and traces drags (not marking channels bad or panning) up to
/// where all names fit, and a double click fits the names again.
#[test]
fn long_names_shorten_and_the_names_column_resizes() {
    let file = long_names_xdf();
    let mut harness = window_with_short_frames(file.path());
    ready(&mut harness);
    let em = body_size(&harness);
    let pad = 6.0;
    let layout = viewer(&harness).layout().unwrap();
    let fitted = layout.names.width();
    assert!((fitted - (10.0 * em + 2.0 * pad)).abs() < 1.0, "{fitted}");
    let rows = viewer(&harness).view().rows() as f32;
    let row_y = |names: egui::Rect, i: f32| names.top() + (i + 0.5) * names.height() / rows;

    // The third row's name, shortened: whole on hover.
    hover(
        &mut harness,
        egui::pos2(layout.names.center().x, row_y(layout.names, 2.0)),
    );
    assert!(tooltip(
        &mut harness,
        "POR X - (Point of regard), left eye\n\nMark as Bad"
    ));
    // Fz's isn't shortened.
    hover(
        &mut harness,
        egui::pos2(layout.names.center().x, row_y(layout.names, 0.0)),
    );
    assert!(tooltip(&mut harness, "Mark as Bad"));
    assert!(harness.query_by_label_contains("Fz\n").is_none());

    let time = viewer(&harness).view().time();
    let edge = |h: &Harness<'_, KymoApp>| {
        let names = viewer(h).layout().unwrap().names;
        egui::pos2(names.right(), row_y(names, 1.0))
    };
    let width = |h: &Harness<'_, KymoApp>| viewer(h).layout().unwrap().names.width();
    let from = edge(&harness);
    drag(&mut harness, from, from + egui::vec2(40.0, 0.0));
    harness.run_steps(2);
    assert!(
        (width(&harness) - (fitted + 40.0)).abs() < 1.0,
        "{}",
        width(&harness)
    );
    assert_eq!(viewer(&harness).view().time(), time);
    let view = viewer(&harness).view();
    assert!((0..4).all(|c| !view.is_bad(kymo_core::ChannelId(c))));
    // Within its limits: as wide as all names fit (here well within half the trace region,
    // where it would stop otherwise), 3 em.
    let from = edge(&harness);
    drag(&mut harness, from, egui::pos2(1190.0, from.y));
    harness.run_steps(2);
    let widest = width(&harness);
    let region = SIZE.x - 300.0;
    assert!(widest < region * 0.4, "{widest}");
    // The longest name is whole: its tooltip doesn't repeat it.
    let names = viewer(&harness).layout().unwrap().names;
    hover(
        &mut harness,
        egui::pos2(names.center().x, row_y(names, 3.0)),
    );
    assert!(tooltip(&mut harness, "Mark as Bad"));
    let ipd = "IPD - Interpupillary distance, both eyes\n\nMark as Bad";
    assert!(harness.query_by_label_contains(ipd).is_none());
    let from = edge(&harness);
    drag(&mut harness, from, from + egui::vec2(40.0, 0.0));
    harness.run_steps(2);
    assert!(
        (width(&harness) - widest).abs() < 1.0,
        "{}",
        width(&harness)
    );
    let from = edge(&harness);
    drag(&mut harness, from, egui::pos2(0.0, from.y));
    harness.run_steps(2);
    assert!(
        (width(&harness) - (3.0 * em + 2.0 * pad)).abs() < 1.0,
        "{}",
        width(&harness)
    );

    // A double click fits the names again.
    let at = edge(&harness);
    double_click(&mut harness, at);
    harness.run_steps(2);
    assert!(
        (width(&harness) - fitted).abs() < 1.0,
        "{}",
        width(&harness)
    );
}

/// A rate that isn't round shows two decimals in the channel list and the summary; hovering
/// the summary's shows it exactly, with the channels and samples.
#[test]
fn rates_are_rounded_with_the_exact_one_on_hover() {
    use egui::accesskit::Role;
    // As MNELAB exported a recording: 500.0065 Hz.
    let mut spec = kymo_testgen::Spec::preset("bv-int16-multiplexed").unwrap();
    let spr = spec.channels[0].samples_per_record;
    spec.record_duration = f64::from(spr) / 500.006_492_535_299_46;
    // Its markers sit on samples at 500 Hz, not at this rate.
    spec.annotations.clear();
    let file = TempFile::written(&spec);
    let mut harness = window(file.path());
    ready(&mut harness);
    let rate = viewer(&harness).document().recording.grids()[0].rate();
    assert_eq!(format!("{rate:.4}"), "500.0065");
    // Every type's row and the summary's Sampling row.
    let rec = &viewer(&harness).document().recording;
    let mut kinds: Vec<_> = rec.channels().iter().map(|c| c.kind.clone()).collect();
    kinds.sort_unstable_by_key(|k| format!("{k:?}"));
    kinds.dedup();
    let rounded = harness.query_all_by_label_contains("500.01 Hz").count();
    assert_eq!(rounded, kinds.len() + 1);
    let exact = format!(
        "{}: {} channels, ",
        kymo::document::exact_rate(rate),
        spec.channels.len()
    );
    assert!(harness.query_by_label_contains(&exact).is_none());
    let sampling = harness
        .query_all_by_role_and_label(Role::Label, "500.01 Hz")
        .map(|n| n.rect())
        .min_by(|a, b| a.top().total_cmp(&b.top()))
        .unwrap();
    // Tooltips wait for the pointer to rest.
    hover(&mut harness, sampling.center());
    let shown = Instant::now() + Duration::from_secs(2);
    while harness.query_by_label_contains(&exact).is_none() && Instant::now() < shown {
        harness.step();
    }
    harness.get_by_label_contains(&exact);
}

/// The summary's format and the ⓘ after it are one button, "Recording Info" for screen
/// readers with the format as its value; hovering it names it and its shortcut, without the
/// format's header fields, which are in Recording Info.
#[test]
fn the_format_is_one_button_with_its_info_sign() {
    use egui::accesskit::Role;
    let file = TempFile::preset("bv-int16-multiplexed");
    let mut harness = window(file.path());
    ready(&mut harness);
    assert!(harness.query_by_label("BrainVision").is_none());
    let button = harness.get_by_role_and_label(Role::Button, kymo::ui::header::INFO_LABEL);
    assert_eq!(
        button.accesskit_node().value().as_deref(),
        Some("BrainVision")
    );
    let format = button.rect();
    hover(&mut harness, format.center());
    assert!(tooltip(&mut harness, "Recording Info ("));
    assert!(harness.query_by_label_contains("orientation").is_none());
}

/// The channel named `name`.
fn channel_named(harness: &Harness<'_, KymoApp>, name: &str) -> kymo_core::ChannelId {
    let rec = &viewer(harness).document().recording;
    let i = rec.channels().iter().position(|c| c.name == name).unwrap();
    kymo_core::ChannelId(i as u32)
}

/// Opens the menu ("⋯") of the type row whose controls are named `label` ("EEG channels",
/// "EEG channels in Amp").
fn type_options(harness: &mut Harness<'_, KymoApp>, label: &str) {
    let label = format!("Options for {label}");
    harness
        .ctx
        .global_style_mut(|s| s.scroll_animation = egui::style::ScrollAnimation::none());
    harness.get_by_label(&label).scroll_to_me();
    harness.run_steps(3);
    harness.get_by_label(&label).click();
    harness.run_steps(2);
}

/// Closes an open popup, as Esc does.
fn close_popup(harness: &mut Harness<'_, KymoApp>) {
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    assert!(
        harness.query_by_label(kymo::ui::types::ALL_TYPES).is_none()
            && harness
                .query_by_role_and_label(egui::accesskit::Role::SpinButton, "R")
                .is_none(),
        "the popup stayed open"
    );
}

/// DC removal centers the types ticked in their popups in the sidebar (EEG by default, not
/// temperature), the next frame after a box changes; the master switch over them all.
#[test]
fn dc_removal_centers_only_the_types_ticked() {
    let file = TempFile::preset("bvrf-int16");
    let mut harness = fonts_window(file.path(), options());
    ready(&mut harness);
    let (fz, temp) = (
        channel_named(&harness, "Fz"),
        channel_named(&harness, "Temp"),
    );
    step_until(&mut harness, "Fz's mean", |app| {
        app.viewer().unwrap().baseline(fz) != 0.0
    });
    assert_eq!(viewer(&harness).baseline(temp), 0.0);

    type_options(&mut harness, "EEG channels");
    harness.get_by_label("Remove DC of EEG").click();
    harness.run_steps(2);
    assert!(
        !harness
            .state()
            .settings()
            .types
            .remove_dc(&ChannelKind::EEG)
    );
    assert_eq!(viewer(&harness).baseline(fz), 0.0);
    type_options(&mut harness, "Temperature channels");
    harness.get_by_label("Remove DC of Temperature").click();
    harness.run_steps(2);
    step_until(&mut harness, "Temp's mean", |app| {
        app.viewer().unwrap().baseline(temp) != 0.0
    });
    // Temp's offset, 25 °C or so.
    assert!(viewer(&harness).baseline(temp) > 10.0);

    // Off, none.
    harness.key_press_modifiers(command(), Key::D);
    harness.run_steps(2);
    assert_eq!(viewer(&harness).baseline(temp), 0.0);
}

/// Each channel of a type scaled each channel on its own has a scale bar (the photo sensor in
/// mV and the temperature in °C by default, the EEG channels once ticked in their popup),
/// and Settings' Restore Defaults puts the channel types back.
#[test]
fn channels_scaled_on_their_own_each_have_a_scale_bar() {
    let file = TempFile::preset("bvrf-int16");
    let mut harness = fonts_window(file.path(), options());
    ready(&mut harness);
    let bars = |harness: &Harness<'_, KymoApp>, kind| {
        let bars = viewer(harness).scale_bars();
        bars.iter().filter(|(b, _)| b.kind == kind).count()
    };
    assert_eq!(bars(&harness, ChannelKind::named("PHOTO")), 1);
    assert_eq!(bars(&harness, ChannelKind::named("Temperature")), 1);
    assert_eq!(bars(&harness, ChannelKind::EEG), 1);

    type_options(&mut harness, "EEG channels");
    harness.get_by_label("Scale Each EEG Channel").click();
    harness.run_steps(2);
    assert_eq!(bars(&harness, ChannelKind::EEG), 5);
    harness.key_press_modifiers(command(), Key::Comma);
    // Until the window has faded in, which takes no clicks before.
    harness.run_steps(10);
    harness.get_by_label("Restore Defaults").click();
    harness.run_steps(2);
    assert_eq!(harness.state().settings().types, TypeSettings::default());
    assert_eq!(bars(&harness, ChannelKind::EEG), 1);
}

/// Marks channel `name` bad (or good) from its menu in the channel list.
fn mark_in_list(harness: &mut Harness<'_, KymoApp>, name: &str, bad: bool) {
    channel_options(harness, name);
    menu_path(harness, &[if bad { "Mark as Bad" } else { "Mark as Good" }]);
}

/// Opens the menu ("⋯") at the end of channel `name`'s row in the channel list.
fn channel_options(harness: &mut Harness<'_, KymoApp>, name: &str) {
    type_options(harness, name);
}

/// Clicks the menu items `path` one after another (submenus, then an item), in a menu that
/// is open.
fn menu_path(harness: &mut Harness<'_, KymoApp>, path: &[&str]) {
    for item in path {
        menu_item(harness, item).click();
        harness.run_steps(2);
    }
}

/// The open menu's item `label`: not a label, as the sidebar's names and types are.
fn menu_item<'h>(harness: &'h Harness<'_, KymoApp>, label: &'h str) -> egui_kittest::Node<'h> {
    let mut items = harness
        .query_all_by_label(label)
        .filter(|n| n.accesskit_node().role() != egui::accesskit::Role::Label);
    let item = items
        .next()
        .unwrap_or_else(|| panic!("no menu item {label}"));
    assert!(items.next().is_none(), "two menu items {label}");
    item
}

/// Changes the type of channel `name` (and those selected with it) by its menu: Change Type
/// To, then `path` ("EEG", or "Other Types" and a type).
fn change_type(harness: &mut Harness<'_, KymoApp>, name: &str, path: &[&str]) {
    channel_options(harness, name);
    menu_path(harness, &[CHANGE_TYPE]);
    menu_path(harness, path);
}

/// A channel given another type follows it everywhere: its type, its place in the order
/// (the traces and the list, under its new type's row, by its place in the file), the
/// amplitude it shares with its new type's channels, its color and DC removal; the type it
/// leaves goes from the list. From File, enabled only then, gives the file's type back.
#[test]
fn a_channel_given_a_type_follows_it() {
    use egui::accesskit::Role;
    let file = typed_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    let [a1, ecg] = [0, 3].map(kymo_core::ChannelId);
    assert_eq!(order(&harness), [0, 1, 2, 3, 4]);
    channel_options(&mut harness, "ECG");
    menu_path(&mut harness, &[CHANGE_TYPE]);
    let from_file = harness.get_by_label("From File (ECG)");
    assert!(from_file.accesskit_node().is_disabled());
    // The recording's types but stim, ECG's own checked.
    let checked = |h: &Harness<'_, KymoApp>, item: &str| {
        menu_item(h, item).accesskit_node().toggled() == Some(egui::accesskit::Toggled::True)
    };
    assert!(checked(&harness, "ECG"));
    assert!(!checked(&harness, "EEG"));
    assert!(
        harness
            .query_by_role_and_label(Role::RadioButton, "Stim")
            .is_none()
    );
    menu_path(&mut harness, &["EEG"]);
    assert!(
        harness.query_by_label(CHANGE_TYPE).is_none(),
        "the menu stayed open"
    );

    let view = viewer(&harness).view();
    assert_eq!(*view.kind_of(ecg), ChannelKind::EEG);
    assert_eq!(*view.file_kind(ecg), ChannelKind::ECG);
    assert_eq!(view.group_of(ecg), (None, ChannelKind::EEG));
    assert_eq!(view.scale_group(ecg), view.scale_group(a1));
    assert_eq!(view.channel_amplitude(ecg), view.channel_amplitude(a1));
    assert!(view.dc_applies(ecg));
    assert_eq!(order(&harness), [0, 1, 3, 2, 4]);
    assert_eq!(shown(&harness), [0, 1, 3, 2, 4]);
    let state = harness.state();
    let types = &state.settings().types;
    let colors = |h: &Harness<'_, KymoApp>| {
        let view = viewer(h).view();
        let visuals = h.ctx.global_style().visuals.clone();
        [a1, ecg].map(|c| kymo::ui::palette::type_color(view.kind_of(c), types, &visuals))
    };
    let [eeg_color, ecg_color] = colors(&harness);
    assert_eq!(eeg_color, ecg_color);
    // In the list under EEG, which has three channels, and ECG's row is gone.
    assert!(
        harness
            .query_by_role_and_label(Role::CheckBox, "Show ECG channels")
            .is_none()
    );
    let eeg = harness.get_by_label("Show EEG channels").rect().center().y;
    let row = |label: &str| harness.get_by_label(label).rect().center().y;
    assert!(eeg < row("Show A2") && row("Show A2") < row("Show ECG"));
    assert!(row("Show ECG") < row("Show EOG channels"));
    harness.get_by_label("3 \u{b7} 512 Hz");

    change_type(&mut harness, "ECG", &["From File (ECG)"]);
    let view = viewer(&harness).view();
    assert_eq!(*view.kind_of(ecg), ChannelKind::ECG);
    assert!(!view.retyped(ecg));
    assert_eq!(order(&harness), [0, 1, 2, 3, 4]);
    harness.get_by_label("Show ECG channels");
}

/// A changed channel type, DC removal or Scale Each Channel shows complete in the frame
/// after the click: they act when drawing, so no tile is missing and nothing rebuilds.
#[test]
fn type_changes_show_complete_in_the_next_frame() {
    let file = typed_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    let ecg = kymo_core::ChannelId(3);
    let complete = |h: &Harness<'_, KymoApp>, what: &str| {
        let viewer = viewer(h);
        assert!(viewer.complete(), "{what}: a tile was missing");
        assert!(
            !viewer.status().right.contains("Building"),
            "{what}: the overview builds again"
        );
    };
    complete(&harness, "the start");

    // A menu item acts as the pointer is released: the frame after it is drawn changed.
    channel_options(&mut harness, "ECG");
    menu_path(&mut harness, &[CHANGE_TYPE]);
    menu_item(&harness, "EEG").click();
    harness.step();
    harness.step();
    assert_eq!(*viewer(&harness).view().kind_of(ecg), ChannelKind::EEG);
    complete(&harness, "Change Type");

    type_options(&mut harness, "EEG channels");
    let before = viewer(&harness).view().dc_applies(ecg);
    harness.get_by_label("Remove DC of EEG").click();
    harness.step();
    harness.step();
    assert_ne!(viewer(&harness).view().dc_applies(ecg), before);
    complete(&harness, "Remove DC");

    let label = kymo::ui::types::own_scale_label(&ChannelKind::EEG);
    let group = viewer(&harness).view().scale_group(ecg);
    // The click on Remove DC closed the menu.
    type_options(&mut harness, "EEG channels");
    harness.get_by_label(&label).click();
    harness.step();
    harness.step();
    assert_ne!(viewer(&harness).view().scale_group(ecg), group);
    complete(&harness, "Scale Each Channel");
}

/// A click in a channel's row selects it, ⌘-click adds or takes one out, ⇧-click selects
/// the rows from the last clicked; the selection bar's Type menu acts on every selected
/// channel (Other Types, filtered), a row's menu on its channel alone, a type's menu on its
/// channels. Esc and a click on the only selected row clear the selection.
#[test]
fn selected_channels_change_type_together() {
    use egui::accesskit::Role;
    let file = typed_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    let selected = |h: &Harness<'_, KymoApp>| -> Vec<u32> {
        viewer(h).view().selected().iter().map(|c| c.0).collect()
    };
    click_in_list(&mut harness, Role::Label, "A1");
    assert_eq!(selected(&harness), [0]);
    harness
        .get_by_role_and_label(Role::Label, "EOG L")
        .click_modifiers(Modifiers::SHIFT);
    harness.run_steps(2);
    assert_eq!(selected(&harness), [0, 1, 2]);
    harness
        .get_by_role_and_label(Role::Label, "A2")
        .click_modifiers(command());
    harness.run_steps(2);
    assert_eq!(selected(&harness), [0, 2]);
    // A row's menu acts on itself alone, selected or not; the bar on the selection.
    for name in ["A2", "A1"] {
        channel_options(&mut harness, name);
        assert!(harness.query_by_label("Mark 2 Channels as Bad").is_none());
        close_menu(&mut harness);
    }
    assert_eq!(selected(&harness), [0, 2]);
    harness.get_by_label("2 channels selected");
    menu_path(&mut harness, &["Type", OTHER_TYPES]);
    harness.get_by_label("Filter Other Types").type_text("temp");
    harness.run_steps(2);
    // Not the recording's types.
    let filtered = |h: &Harness<'_, KymoApp>, label: &str| {
        h.query_all_by_role_and_label(Role::Button, label).count()
    };
    assert_eq!(filtered(&harness, "Temperature"), 1);
    harness.get_by_label("Filter Other Types").focus();
    for _ in 0..4 {
        harness.key_press(Key::Backspace);
    }
    harness.get_by_label("Filter Other Types").type_text("e");
    harness.run_steps(2);
    assert_eq!(filtered(&harness, "EMG"), 1);
    assert_eq!(filtered(&harness, "EEG") + filtered(&harness, "ECG"), 0);
    harness.get_by_label("Filter Other Types").type_text("mp");
    harness.run_steps(2);
    menu_path(&mut harness, &["Temperature"]);
    let temperature = ChannelKind::named("Temperature");
    let view = viewer(&harness).view();
    assert_eq!(*view.kind_of(kymo_core::ChannelId(0)), temperature);
    assert_eq!(*view.kind_of(kymo_core::ChannelId(2)), temperature);
    assert_eq!(*view.kind_of(kymo_core::ChannelId(1)), ChannelKind::EEG);
    // Temperature comes after the types EEG, EOG (now none) and ECG.
    assert_eq!(order(&harness), [1, 3, 0, 2, 4]);

    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(selected(&harness).is_empty());
    click_in_list(&mut harness, Role::Label, "A2");
    assert_eq!(selected(&harness), [1]);
    click_in_list(&mut harness, Role::Label, "A2");
    assert!(selected(&harness).is_empty());

    // The type's menu returns them both, its file's type differing.
    type_options(&mut harness, "Temperature channels");
    menu_path(&mut harness, &[CHANGE_TYPE, FROM_FILE]);
    assert_eq!(order(&harness), [0, 1, 2, 3, 4]);
}

/// A channel row's menu ("⋯") shows only while the pointer is on its row, selected or not;
/// it takes its room either way, in line with the type rows' menus.
#[test]
fn a_channels_menu_shows_on_its_row_hovered() {
    use egui::accesskit::Role;
    let file = typed_preset();
    let mut harness = fonts_window(file.path(), options());
    ready(&mut harness);
    // How much brighter the button's brightest pixel is than the row beside it, the pointer
    // at `pointer`.
    let contrast = |h: &mut Harness<'_, KymoApp>, pointer: egui::Pos2| {
        hover(h, pointer);
        h.run_steps(2);
        let r = h.get_by_label("Options for A2").rect();
        let image = h.render().unwrap();
        let value = |x: u32, y: u32| i32::from(*image.get_pixel(x, y).0[..3].iter().max().unwrap());
        let beside = value(r.left() as u32 - 4, r.center().y as u32);
        let dots = (r.left() as u32..r.right() as u32)
            .flat_map(|x| (r.top() as u32..r.bottom() as u32).map(move |y| (x, y)))
            .map(|(x, y)| value(x, y))
            .max()
            .unwrap();
        dots - beside
    };
    let away = egui::pos2(900.0, 650.0);
    assert!(contrast(&mut harness, away) < 5);
    let row = harness.get_by_label("Show A2").rect().center() + egui::vec2(60.0, 0.0);
    assert!(contrast(&mut harness, row) > 40);
    let menu = harness.get_by_label("Options for A2").rect();
    let type_menu = harness.get_by_label("Options for EEG channels").rect();
    assert!((menu.right() - type_menu.right()).abs() < 0.5);
    // Selected, it acts on its row alone all the same: not shown without the pointer.
    click_in_list(&mut harness, Role::Label, "A2");
    assert!(viewer(&harness).view().is_selected(kymo_core::ChannelId(1)));
    assert!(contrast(&mut harness, away) < 5);
}

/// "Bad" shows on a bad channel's row alone, hovered or not, and isn't a button. The
/// selection bar marks the selected channels bad or good, saying how many it marks; a
/// channel's menu marks it alone.
#[test]
fn bad_shows_on_bad_rows_and_menus_mark_bad() {
    use egui::accesskit::Role;
    let file = typed_preset();
    let mut harness = fonts_window(file.path(), options());
    ready(&mut harness);
    let row = harness.get_by_label("Show A2").rect().center() + egui::vec2(60.0, 0.0);
    for pointer in [egui::pos2(900.0, 650.0), row] {
        hover(&mut harness, pointer);
        harness.run_steps(2);
        assert!(harness.query_by_label("A2 bad").is_none());
    }
    mark_in_list(&mut harness, "A2", true);
    let label = harness.get_by_label("A2 bad");
    assert_eq!(label.accesskit_node().role(), Role::Label);
    // A click on it does nothing (but select the row).
    label.click();
    harness.run_steps(2);
    assert!(
        viewer(&harness)
            .view()
            .is_bad(channel_named(&harness, "A2"))
    );
    harness.key_press(Key::Escape);
    harness.run_steps(2);

    // A2 and EOG L bad, A1 good: the selection bar's Mark menu marks either.
    let (a1, a2, eog) = (
        channel_named(&harness, "A1"),
        channel_named(&harness, "A2"),
        channel_named(&harness, "EOG L"),
    );
    mark_in_list(&mut harness, "EOG L", true);
    click_in_list(&mut harness, Role::Label, "A1");
    for name in ["A2", "EOG L"] {
        harness
            .get_by_role_and_label(Role::Label, name)
            .click_modifiers(command());
        harness.run_steps(2);
    }
    assert_eq!(viewer(&harness).view().selected().len(), 3);
    let bad = |h: &Harness<'_, KymoApp>| {
        let view = viewer(h).view();
        [a1, a2, eog].map(|c| view.is_bad(c))
    };
    menu_path(&mut harness, &["Mark"]);
    assert!(harness.query_by_label("Mark 2 Channels as Good").is_some());
    menu_path(&mut harness, &["Mark 1 Channel as Bad"]);
    assert_eq!(bad(&harness), [true; 3]);
    menu_path(&mut harness, &["Mark"]);
    assert!(harness.query_by_label("Mark 3 Channels as Bad").is_none());
    menu_path(&mut harness, &["Mark 3 Channels as Good"]);
    assert_eq!(bad(&harness), [false; 3]);

    // A row out of the selection marks itself alone.
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    channel_options(&mut harness, "A2");
    assert!(harness.query_by_label("Mark as Good").is_none());
    menu_path(&mut harness, &["Mark as Bad"]);
    assert_eq!(bad(&harness), [false, true, false]);
}

/// ⌘A selects every channel, Edit → Select Bad Channels the bad ones and Deselect All
/// Channels none; a type's menu selects its channels.
#[test]
fn channels_are_selected_all_bad_or_by_type() {
    use egui::accesskit::Role;
    let file = typed_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    let selected = |h: &Harness<'_, KymoApp>| -> Vec<u32> {
        viewer(h).view().selected().iter().map(|c| c.0).collect()
    };
    let perform = |h: &mut Harness<'_, KymoApp>, action: Action| {
        let ctx = h.ctx.clone();
        h.state_mut().perform(&ctx, action);
        h.run_steps(2);
    };
    press(&mut harness, command(), Key::A);
    assert_eq!(selected(&harness), [0, 1, 2, 3, 4]);
    harness.get_by_label("5 channels selected");
    perform(&mut harness, Action::DeselectChannels);
    assert!(selected(&harness).is_empty());
    // Hidden ones too.
    click_in_list(&mut harness, Role::CheckBox, "Show Status");
    press(&mut harness, command(), Key::A);
    assert_eq!(selected(&harness), [0, 1, 2, 3, 4]);

    mark_in_list(&mut harness, "A2", true);
    mark_in_list(&mut harness, "ECG", true);
    perform(&mut harness, Action::SelectBadChannels);
    assert_eq!(selected(&harness), [1, 3]);

    type_options(&mut harness, "EEG channels");
    menu_path(&mut harness, &["Select 2 EEG Channels"]);
    assert_eq!(selected(&harness), [0, 1]);
}

/// The selection bar is one row while it only shows how to select; with a selection, its
/// buttons wrap in a narrow sidebar rather than going past its edge.
#[test]
fn the_selection_bar_fits_the_sidebar() {
    use egui::accesskit::Role;
    let file = typed_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    let hint = harness.get_by_label_contains("to select.").rect();
    let row = harness.get_by_label("Show A1").rect().height();
    assert!(hint.height() < 1.5 * row, "{hint:?}");
    // The narrowest sidebar.
    sidebar_to(&mut harness, 0.0);
    click_in_list(&mut harness, Role::Label, "A1");
    let edge = viewer(&harness).layout().unwrap().names.left() - 8.0;
    let hide = harness.get_by_label("Hide").rect();
    let scale = harness.get_by_label("Scale").rect();
    assert!(scale.right() <= edge, "{scale:?} past {edge}");
    assert!(scale.top() > hide.top(), "{scale:?} beside {hide:?}");
}

/// Closes an open menu, as Esc does.
fn close_menu(harness: &mut Harness<'_, KymoApp>) {
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    assert!(
        harness.query_by_label(CHANGE_TYPE).is_none(),
        "the menu stayed open"
    );
}

/// New Type… names a type: a known spelling is the known type ("ekg" as ECG), stim can't be
/// named, and a name kymo doesn't know is kept, so Other Types offers it from then on.
#[test]
fn a_new_type_is_named_and_kept() {
    let file = typed_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    channel_options(&mut harness, "A2");
    menu_path(&mut harness, &[CHANGE_TYPE, NEW_TYPE]);
    let change =
        |h: &Harness<'_, KymoApp>| !h.get_by_label("Change").accesskit_node().is_disabled();
    assert!(!change(&harness));
    harness.get_by_label("New Type Name").type_text("stim");
    harness.run_steps(2);
    harness.get_by_label("Stim channels come from the file.");
    assert!(!change(&harness));
    harness.get_by_label("New Type Name").focus();
    for _ in 0..4 {
        harness.key_press(Key::Backspace);
    }
    harness.get_by_label("New Type Name").type_text("ekg");
    harness.run_steps(2);
    harness.get_by_label("As ECG");
    harness.get_by_label("New Type Name").focus();
    for _ in 0..3 {
        harness.key_press(Key::Backspace);
    }
    harness.get_by_label("New Type Name").type_text("Pulse");
    harness.run_steps(2);
    assert!(change(&harness));
    harness.key_press(Key::Enter);
    harness.run_steps(3);
    assert!(
        harness.query_by_label(CHANGE_TYPE).is_none(),
        "the menu stayed open"
    );
    let pulse = ChannelKind::named("pulse");
    assert_eq!(
        *viewer(&harness).view().kind_of(kymo_core::ChannelId(1)),
        pulse
    );
    assert_eq!(
        viewer(&harness)
            .view()
            .kind_of(kymo_core::ChannelId(1))
            .name(),
        "Pulse"
    );
    assert!(harness.state().settings().types.stored(&pulse));
    harness.get_by_label("Show Pulse channels");

    // Offered under Other Types for another channel, until it is the recording's.
    change_type(&mut harness, "A2", &["From File (EEG)"]);
    change_type(&mut harness, "A1", &[OTHER_TYPES]);
    harness.get_by_label("Filter Other Types").type_text("pul");
    harness.run_steps(2);
    menu_path(&mut harness, &["Pulse"]);
    assert_eq!(
        *viewer(&harness).view().kind_of(kymo_core::ChannelId(0)),
        pulse
    );
}

/// A stim channel given another type is drawn as a signal (a scale bar of its own, its DC
/// removed if its type's is) and its stim annotations go; From File makes it stim again,
/// with them.
#[test]
fn a_stim_channel_made_misc_loses_its_annotations_until_from_file() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let status = channel_named(&harness, "Status");
    let before = stim_annotations(&harness);
    assert!(!before.is_empty());
    let bars = |h: &Harness<'_, KymoApp>| {
        viewer(h)
            .scale_bars()
            .iter()
            .filter(|(b, _)| b.kind == ChannelKind::MISC)
            .count()
    };
    assert_eq!(bars(&harness), 0);

    change_type(&mut harness, "Status", &[OTHER_TYPES]);
    harness.get_by_label("Filter Other Types").type_text("misc");
    harness.run_steps(2);
    menu_path(&mut harness, &["Misc"]);
    harness.run_steps(2);
    assert_eq!(*viewer(&harness).view().kind_of(status), ChannelKind::MISC);
    assert!(stim_annotations(&harness).is_empty());
    assert_eq!(bars(&harness), 1);
    // Stim is never offered, not even under Other Types.
    channel_options(&mut harness, "Status");
    menu_path(&mut harness, &[CHANGE_TYPE]);
    assert!(harness.query_by_label("Stim").is_none());
    menu_path(&mut harness, &[OTHER_TYPES]);
    harness.get_by_label("Filter Other Types").type_text("stim");
    harness.run_steps(2);
    harness.get_by_label("No channel types match.");
    harness.key_press(Key::Escape);
    harness.run_steps(3);

    change_type(&mut harness, "Status", &["From File (Stim)"]);
    harness.run_steps(2);
    assert!(viewer(&harness).view().kind_of(status).is_stim());
    assert_stim(&stim_annotations(&harness), &before);
    assert_eq!(bars(&harness), 0);
}

/// File → Save Channels… writes every channel's type as shown and its bad mark as a BIDS
/// `channels.tsv`, suggested next to the recording; Load Channels… in a new window gives
/// them back, in the list's order too.
#[test]
fn channels_save_and_load_through_the_file_menu() {
    let file = typed_preset();
    let dir = TempDir::new();
    let mut harness = window(file.path());
    ready(&mut harness);
    let [a2, ecg] = [1, 3].map(kymo_core::ChannelId);
    let suggested = viewer(&harness).channels_path();
    let stem = file.path().file_stem().unwrap().to_str().unwrap();
    assert_eq!(
        suggested,
        file.path().with_file_name(format!("{stem}_channels.tsv"))
    );

    change_type(&mut harness, "ECG", &["EEG"]);
    mark_in_list(&mut harness, "A2", true);
    assert!(viewer(&harness).view().is_bad(a2));
    // Without an extension, .tsv is added.
    harness
        .state_mut()
        .answer_file_dialogs([Some(dir.path().join("mine"))]);
    choose(&mut harness, "File", "Save Channels…");
    let saved = dir.path().join("mine.tsv");
    assert!(harness.state().notice().is_none());
    let text = std::fs::read_to_string(&saved).unwrap();
    let rows: Vec<Vec<&str>> = text.lines().map(|l| l.split('\t').collect()).collect();
    assert_eq!(
        rows[0],
        ["name", "type", "units", "sampling_frequency", "status"]
    );
    let row = |name: &str| rows.iter().find(|r| r[0] == name).unwrap().clone();
    assert_eq!(row("A2")[1..], ["EEG", "\u{b5}V", "512", "bad"]);
    assert_eq!(row("ECG")[1..2], ["EEG"]);
    assert_eq!(row("EOG L")[1..2], ["EOG"]);
    assert_eq!(row("Status")[1..2], ["TRIG"]);
    assert_eq!(rows.len(), 6);
    // The next save suggests the same file.
    assert_eq!(viewer(&harness).channels_path(), saved);
    drop(harness);

    let mut harness = window(file.path());
    ready(&mut harness);
    harness
        .state_mut()
        .answer_file_dialogs([Some(saved.clone())]);
    choose(&mut harness, "File", "Load Channels…");
    assert!(harness.state().notice().is_none());
    let view = viewer(&harness).view();
    assert_eq!(*view.kind_of(ecg), ChannelKind::EEG);
    assert!(view.retyped(ecg));
    assert!(view.is_bad(a2));
    assert_eq!(order(&harness), [0, 1, 3, 2, 4]);
    assert!(harness.query_by_label("Show ECG channels").is_none());
}

/// A loaded `channels.tsv` changes only what it says: rows naming no channel are skipped,
/// a channel made stim stays as it is (only the file makes one), a stim channel made Misc
/// loses its annotations, a name kymo doesn't know is kept as a type; the notice says what
/// was left out. A broken file changes nothing and says where it broke.
#[test]
fn loading_channels_takes_what_it_can_and_says_what_it_left() {
    let file = typed_preset();
    let dir = TempDir::new();
    let mut harness = window(file.path());
    ready(&mut harness);
    let [a1, a2, ecg] = [0, 1, 3].map(kymo_core::ChannelId);
    let status = channel_named(&harness, "Status");
    assert!(!stim_annotations(&harness).is_empty());
    mark_in_list(&mut harness, "A2", true);

    let path = dir.path().join("sub-01_channels.tsv");
    std::fs::write(
        &path,
        "name\ttype\tunits\tstatus\tkymo_type\n\
         A1\tTRIG\tuV\tn/a\tn/a\n\
         ECG\tMISC\tn/a\tgood\tWheel\n\
         X9\tEEG\tuV\tgood\tn/a\n\
         A2\tEEG\tmS\tgood\tn/a\n\
         Status\tMISC\tn/a\tn/a\tn/a\n",
    )
    .unwrap();
    assert!(harness.state_mut().load_channels_from(&path));
    harness.run_steps(2);
    let view = viewer(&harness).view();
    assert_eq!(*view.kind_of(a1), ChannelKind::EEG);
    assert_eq!(view.kind_of(ecg).name(), "Wheel");
    assert_eq!(*view.kind_of(status), ChannelKind::MISC);
    assert!(!view.is_bad(a2));
    assert!(stim_annotations(&harness).is_empty());
    assert!(
        harness
            .state()
            .settings()
            .types
            .stored(&ChannelKind::named("wheel"))
    );
    let (title, text) = harness.state().notice().unwrap();
    assert_eq!(title, "Channels Loaded");
    assert_eq!(
        text,
        "Skipped 1 row naming channels the recording doesn't have. Left 1 channel as it \
         is: only the file makes a channel Stim. 1 channel has other units in \
         sub-01_channels.tsv; their values come from the recording."
    );
    harness.run_steps(2);
    harness.get_by_label("OK").click();
    harness.run_steps(2);

    // Dropped, it loads as channels too: back to the file's types.
    std::fs::write(&path, "name\ttype\nECG\tECG\nStatus\tTRIG\n").unwrap();
    drop_files(&mut harness, &[&path]);
    assert!(harness.state().notice().is_none());
    let view = viewer(&harness).view();
    assert_eq!(*view.kind_of(ecg), ChannelKind::ECG);
    assert!(view.kind_of(status).is_stim());
    harness.run_steps(2);
    assert!(!stim_annotations(&harness).is_empty());

    // A broken file changes nothing.
    let broken = dir.path().join("broken.tsv");
    std::fs::write(&broken, "name\tunits\nECG\tmV\n").unwrap();
    assert!(!harness.state_mut().load_channels_from(&broken));
    let (title, text) = harness.state().notice().unwrap();
    assert_eq!(title, "Could Not Load the Channels");
    assert_eq!(text, "broken.tsv: no column \"type\"");
    assert_eq!(*viewer(&harness).view().kind_of(ecg), ChannelKind::ECG);
}

/// Save Channels… suggests BIDS' name for a recording named as BIDS names them.
#[test]
fn channels_are_suggested_by_bids_name() {
    let dir = TempDir::new();
    let path = dir.path().join("sub-01_task-rest_eeg.bdf");
    let spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    kymo_testgen::write(&spec, &path).unwrap();
    let mut harness = window(&path);
    ready(&mut harness);
    assert_eq!(
        viewer(&harness).channels_path(),
        dir.path().join("sub-01_task-rest_channels.tsv")
    );
}

/// File → Recording Info…, ⌘I and the format's button open the Recording Info window; ⌘I
/// and Esc close it.
#[test]
fn recording_info_opens_from_the_menu_the_shortcut_and_the_format() {
    let file = TempFile::preset("bv-int16-multiplexed");
    let mut harness = window(file.path());
    ready(&mut harness);
    let open = |harness: &Harness<'_, KymoApp>| {
        let open = harness.state().info_open();
        assert_eq!(harness.query_by_label("Orientation").is_some(), open);
        open
    };
    assert!(!open(&harness));
    choose(&mut harness, "File", "Recording Info…");
    assert!(open(&harness));
    harness.key_press_modifiers(command(), Key::I);
    harness.run_steps(2);
    assert!(!open(&harness));
    harness.key_press_modifiers(command(), Key::I);
    harness.run_steps(2);
    assert!(open(&harness));
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(!open(&harness));

    harness.get_by_label(kymo::ui::header::INFO_LABEL).click();
    harness.run_steps(2);
    assert!(open(&harness));
}

/// The Recording Info window shows what `--info` prints: the format's fields, their names
/// as keys in line with the others, the streams (a rate's unit on its line), every grid with
/// its exact rate and channels, and the warnings.
#[test]
fn recording_info_shows_fields_grids_and_warnings() {
    let presets: [(&str, &[&str]); 3] = [
        (
            "bvrf-int16",
            &[
                "Amplifiers",
                "kymo-testgen (serial 0)",
                "Power line frequency",
                "Grid 1",
                "500\u{a0}Hz, 60000 samples, 9 channels",
                "Photo_AUX1",
                "Temp",
            ],
        ),
        (
            "xdf-mixed",
            &[
                "Stream 3",
                "\"Amp\" (EEG), 3 × int16, 256\u{a0}Hz, 23040 samples (channels); effective \
                 rate 256.000000\u{a0}Hz, 1 segment",
                "Grid 1 \u{b7} Muse (ACC)",
                "Grid 6 \u{b7} Aux (AUX)",
                "Aux_0",
                "Aux_1",
            ],
        ),
        (
            "bvrf-float64",
            &[
                "channel \"GSR\": its coefficients aren't linear, so it shows the sensor \
                 output in mV rather than \u{b5}S",
            ],
        ),
    ];
    for (preset, texts) in presets {
        let file = TempFile::preset(preset);
        let mut harness = window(file.path());
        ready(&mut harness);
        harness.key_press_modifiers(command(), Key::I);
        harness.run_steps(2);
        unfold_grids(&mut harness);
        let window = info_window(&harness);
        for text in texts {
            assert!(window.query_by_label(text).is_some(), "{preset}: {text}");
        }
        // The fields' names start where the other keys do.
        let left = |key: &str| window.get_by_label(key).rect().left();
        if preset == "bvrf-int16" {
            assert_eq!(left("Amplifiers"), left("Format"));
        }
    }
}

/// Unfolds every grid's table in the Recording Info window, folded at first.
fn unfold_grids(harness: &mut Harness<'_, KymoApp>) {
    let grids: Vec<String> = info_window(harness)
        .query_all_by_label_contains("Grid ")
        .filter_map(|n| n.accesskit_node().label())
        .collect();
    assert!(!grids.is_empty());
    // Through accessibility, as unfolding one moves the others below the scrolled view.
    for grid in grids {
        info_window(harness).get_by_label(&grid).click_accesskit();
        harness.run_steps(2);
    }
}

/// The Recording Info window.
fn info_window<'h>(harness: &'h Harness<'_, KymoApp>) -> egui_kittest::Node<'h> {
    harness.get_by_role_and_label(egui::accesskit::Role::Window, "Recording Info")
}

/// Recording Info shows each grid's channels as a table, a row per channel with its number,
/// name, type and unit, and its physical range, prefilter and transducer where the file has
/// them, rather than `--info`'s list of names.
#[test]
fn recording_info_tables_the_channels() {
    use kymo::ui::info::COLUMNS;
    let cells = |harness: &Harness<'_, KymoApp>, texts: &[&str]| {
        let window = info_window(harness);
        texts
            .iter()
            .map(|t| window.query_all_by_label(t).count())
            .collect::<Vec<_>>()
    };
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    harness.key_press_modifiers(command(), Key::I);
    harness.run_steps(2);
    // Folded at first.
    assert_eq!(cells(&harness, &COLUMNS), [0; 7]);
    unfold_grids(&mut harness);
    // Every column, once each; four EEG channels in µV from AgAgCl electrodes, with the
    // file's prefilter and range; the Status channel as the file describes it.
    assert_eq!(cells(&harness, &COLUMNS), [1; 7]);
    assert_eq!(cells(&harness, &["EEG", "AgAgCl electrode"]), [4, 4]);
    assert_eq!(
        cells(&harness, &["HP:0.1Hz LP:100Hz", "-262144 \u{2026} 262143"]),
        [4, 3]
    );
    assert_eq!(
        cells(
            &harness,
            &["Status", "Stim", "Boolean", "Triggers and Status"]
        ),
        [1, 1, 1, 1]
    );
    // The channels numbered (5 is also the Channels row).
    assert_eq!(cells(&harness, &["1", "4", "5"]), [1, 1, 2]);
    assert!(harness.query_by_label_contains("A1, A2").is_none());

    // Without ranges, prefilters and transducers (XDF), without their columns.
    let file = TempFile::preset("xdf-mixed");
    let mut harness = window(file.path());
    ready(&mut harness);
    harness.key_press_modifiers(command(), Key::I);
    harness.run_steps(2);
    unfold_grids(&mut harness);
    assert_eq!(cells(&harness, &COLUMNS[..4]), [6; 4]);
    assert_eq!(cells(&harness, &COLUMNS[4..]), [0; 3]);
    assert!(harness.query_by_label_contains("Aux_0, Aux_1").is_none());
}

/// Recording Info's grids start folded whenever the window opens, whatever was unfolded
/// before.
#[test]
fn recording_info_grids_start_folded() {
    let file = TempFile::preset("xdf-mixed");
    let toggle_info = |harness: &mut Harness<'_, KymoApp>| {
        harness.key_press_modifiers(command(), Key::I);
        harness.run_steps(10);
    };
    let unfolded = |harness: &Harness<'_, KymoApp>| {
        info_window(harness)
            .query_all_by_label("#")
            .next()
            .is_some()
    };
    let mut harness = window(file.path());
    ready(&mut harness);
    toggle_info(&mut harness);
    assert!(!unfolded(&harness));
    unfold_grids(&mut harness);
    assert!(unfolded(&harness));
    // Closed and opened again: folded.
    toggle_info(&mut harness);
    toggle_info(&mut harness);
    assert!(!unfolded(&harness));
}

/// The Recording Info window's Copy button puts exactly what `kymo --info` prints on the
/// clipboard.
#[test]
fn recording_info_copies_what_info_prints() {
    let file = TempFile::preset("bv-int16-multiplexed");
    let mut harness = window(file.path());
    ready(&mut harness);
    harness.key_press_modifiers(command(), Key::I);
    harness.run_steps(10);
    harness.get_by_label(kymo::ui::info::COPY).click();
    harness.step();
    let copied = harness
        .output()
        .platform_output
        .commands
        .iter()
        .find_map(|c| match c {
            egui::OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        });
    let summary = kymo::document::open(file.path()).unwrap().summary();
    let printed = kymo::document::info_text(file.path(), &summary);
    assert!(
        printed.contains("\n             binary format: INT_16\n"),
        "{printed}"
    );
    assert_eq!(copied.as_deref(), Some(printed.as_str()));
}

/// The shortcut overview's scroll bar is painted at the window's right border, a gap after
/// the rows, not inside them (as it was while a scroll area of ours sat inside the window's
/// content, which egui clips a margin short of the border).
#[test]
fn the_shortcut_overview_scroll_bar_sits_at_the_border() {
    let mut builder = Harness::builder()
        .with_size(egui::vec2(800.0, 500.0))
        .with_context_setup(system_fonts);
    // The app draws only its own platform's shortcuts; the Mac's ⌫ may be in no font here,
    // as on Windows, and a box is as good for the bar's place.
    if Platform::current() != Platform::Mac {
        builder = builder.allow_missing_glyphs();
    }
    if let Some(gpu) = shared_gpu() {
        builder = builder.wgpu_setup(egui_wgpu::WgpuSetup::Existing(gpu.clone()));
    }
    let mut harness = builder
        .build_ui(|ui| kymo::ui::menus::shortcuts_window(ui.ctx(), &mut true, Platform::Mac));
    harness.run_steps(3);
    let window = harness
        .ctx
        .memory(|m| m.area_rect(kymo::ui::Overlay::Shortcuts.id()))
        .unwrap();
    // The pointer on the bar widens it to its full width.
    let title = harness.get_by_label("Trackpad and Mouse").rect();
    harness.hover_at(egui::pos2(window.right() - 4.0, title.center().y));
    harness.run_steps(10);
    let style = harness.ctx.global_style();
    let fill = style.visuals.window_fill.to_array();
    let stroke = style.visuals.window_stroke.width;
    let scroll = style.spacing.scroll;
    let image = harness.render().unwrap();
    let pixel = |x: f32| image.get_pixel(x as u32, title.center().y as u32).0;
    // The bar's middle, a half bar in from the border, is painted; between the rows' end
    // (the keys' plus their padding of 6 points) and the bar is the window's fill.
    let bar = window.right() - stroke - scroll.bar_width / 2.0;
    assert_ne!(
        pixel(bar),
        fill,
        "no bar at {bar}, the window ends at {}",
        window.right()
    );
    let keys = harness.get_by_label("Scroll up or down").rect();
    let gap = keys.right() + 6.0 + scroll.bar_inner_margin / 2.0;
    assert_eq!(pixel(gap), fill, "something painted at {gap}");
}

/// Each channel type on screen but stim has a scale bar at the traces' right edge,
/// centered on its first row; View → Show Scale Bars hides them, and is remembered.
#[test]
fn scale_bars_show_beside_their_rows_and_hide() {
    let file = typed_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    // A1 and A2 (EEG), EOG L, ECG and Status.
    let kinds = |h: &Harness<'_, KymoApp>| {
        viewer(h)
            .scale_bars()
            .iter()
            .map(|(b, _)| (b.kind.clone(), b.channel.0))
            .collect::<Vec<_>>()
    };
    use crate::kinds::{Ecg, Eeg, Eog};
    assert_eq!(kinds(&harness), [(Eeg, 0), (Eog, 2), (Ecg, 3)]);
    let traces = viewer(&harness).layout().unwrap().traces;
    let view = viewer(&harness).view();
    let row_height = view.row_height(f64::from(traces.height())) as f32;
    // On whole pixels: within a pixel of where they belong, as long as they should be.
    let pixel = 1.0 / harness.ctx.pixels_per_point();
    for (bar, rect) in viewer(&harness).scale_bars() {
        let middle = traces.top() + bar.middle as f32 * row_height;
        assert!(
            (rect.center().y - middle).abs() <= pixel,
            "{rect:?} vs {middle}"
        );
        let length = bar.fraction as f32 * row_height;
        assert!(
            (rect.height() - length).abs() <= pixel,
            "{rect:?} vs {length}"
        );
        assert!(traces.right() - 20.0 < rect.left() && rect.right() < traces.right());
    }
    assert!(checked(&mut harness, "View", "Show Scale Bars"));
    choose(&mut harness, "View", "Show Scale Bars");
    assert!(viewer(&harness).scale_bars().is_empty());
    assert!(!harness.state().settings().scale_bars);
    assert!(!checked(&mut harness, "View", "Show Scale Bars"));
    choose(&mut harness, "View", "Show Scale Bars");
    assert_eq!(kinds(&harness).len(), 3);
}

/// The reader's warnings show as one line in the sidebar, "1 Warning", and in full, as
/// sentences, only on hovering it.
#[test]
fn warnings_show_as_one_line_with_the_text_on_hover() {
    use std::io::Write;
    let file = TempFile::preset("bdf+c-small");
    // Bytes after the last record, which the reader ignores with a warning.
    let mut out = std::fs::OpenOptions::new()
        .append(true)
        .open(file.path())
        .unwrap();
    out.write_all(&[0; 5]).unwrap();
    drop(out);
    let mut harness = window(file.path());
    ready(&mut harness);
    let warnings = &viewer(&harness).document().recording.warnings().to_vec();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("the file has 5 bytes"),
        "{warnings:?}"
    );
    let full = "The file has 5 bytes";
    assert!(harness.query_by_label_contains(full).is_none());
    let line = harness.get_by_label("1 Warning").rect();
    // The whole line answers the pointer, the sign at its left as well.
    hover(&mut harness, egui::pos2(line.left() - 6.0, line.center().y));
    let shown = Instant::now() + Duration::from_secs(2);
    while harness.query_by_label_contains(full).is_none() && Instant::now() < shown {
        harness.step();
    }
    harness.get_by_label_contains(full);
}

/// The legend lists every description with its count; its checkboxes show and hide
/// annotations by description, or all at once.
#[test]
fn the_legend_shows_and_hides_annotations() {
    use egui::accesskit::{Role, Toggled};
    // In the first 20 s: "event 0" (an event, color 0), "sleep stage N2" (2.5 s, color 1),
    // and "eyes closed" and "Elektrodenprüfung µV" (events at 15 s, colors 2 and 3; one
    // label, as the second would overlap the first).
    let file = without_triggers("edf+d-mixed");
    let mut harness = window(file.path());
    ready(&mut harness);
    // 20 "event k", 7 "stim k", and three more.
    assert_eq!(viewer(&harness).legend().shown_count(), (30, 30));
    let colors = |harness: &Harness<'_, KymoApp>| {
        let marks = viewer(harness).marks();
        let mut bands: Vec<_> = marks.bands.iter().map(|m| m.color).collect();
        let mut lines: Vec<_> = marks.lines.iter().map(|m| m.color).collect();
        bands.sort();
        lines.sort();
        (bands, lines, marks.labels.len())
    };
    use AnnotationColor::Palette;
    assert_eq!(
        colors(&harness),
        (
            vec![Palette(1)],
            vec![Palette(0), Palette(2), Palette(3)],
            3
        )
    );

    // The legend is at the bottom of the sidebar, below the channels and their types, and shows
    // 8 rows before it scrolls.
    let top = |harness: &Harness<'_, KymoApp>, label: &str| {
        harness
            .get_by_role_and_label(Role::CheckBox, label)
            .rect()
            .top()
    };
    let legend = top(&harness, "Show Annotations");
    let names: Vec<String> = viewer(&harness)
        .document()
        .recording
        .channels()
        .iter()
        .map(|c| format!("Show {}", c.name))
        .collect();
    let channels: Vec<f32> = names
        .iter()
        .filter_map(|n| harness.query_by_role_and_label(Role::CheckBox, n))
        .map(|n| n.rect().top())
        .collect();
    assert!(!channels.is_empty());
    assert!(
        top(&harness, "Show EEG channels") < channels[0],
        "{channels:?}"
    );
    assert!(channels[0] < legend, "{channels:?} {legend}");
    let row = harness.ctx.global_style().spacing.interact_size.y;
    // The reading under the pointer marks the status line.
    let traces = viewer(&harness).layout().unwrap().traces;
    hover(&mut harness, traces.center());
    harness.step();
    let status = harness
        .get_by_label(&viewer(&harness).status().left)
        .rect()
        .top();
    assert!(
        status - legend > 9.0 * row,
        "legend at {legend}, status at {status}"
    );

    assert!(harness.query_by_label("sleep stage N2").is_some());
    assert!(harness.query_by_label("Elektrodenprüfung µV").is_some());
    assert_eq!(
        viewer(&harness)
            .legend()
            .entry(None, "sleep stage N2")
            .unwrap()
            .count,
        20
    );
    // The Annotations row counts them all, right-aligned with the descriptions' counts.
    let in_row = |harness: &Harness<'_, KymoApp>, checkbox: &str, text: &str| {
        let row = harness
            .get_by_role_and_label(Role::CheckBox, checkbox)
            .rect();
        let found: Vec<_> = harness
            .query_all_by_label(text)
            .map(|n| n.rect())
            .filter(|r| row.y_range().contains(r.center().y))
            .collect();
        assert_eq!(found.len(), 1, "{text} in the row of {checkbox}");
        found[0]
    };
    let total = viewer(&harness).document().annotations.len();
    let total = in_row(&harness, "Show Annotations", &total.to_string());
    let count = in_row(&harness, "Show sleep stage N2", "20");
    assert!(
        (total.right() - count.right()).abs() < 0.5,
        "{total} {count}"
    );

    click_in_list(&mut harness, Role::CheckBox, "Show sleep stage N2");
    assert_eq!(
        colors(&harness),
        (vec![], vec![Palette(0), Palette(2), Palette(3)], 2)
    );
    assert_eq!(viewer(&harness).legend().shown_count(), (29, 30));
    assert_eq!(
        toggled(&harness, Role::CheckBox, "Show Annotations"),
        Toggled::Mixed
    );

    // A mixed box shows them all; then it hides them all.
    click_in_list(&mut harness, Role::CheckBox, "Show Annotations");
    assert_eq!(
        colors(&harness),
        (
            vec![Palette(1)],
            vec![Palette(0), Palette(2), Palette(3)],
            3
        )
    );
    click_in_list(&mut harness, Role::CheckBox, "Show Annotations");
    assert_eq!(colors(&harness), (vec![], vec![], 0));
    assert_eq!(
        toggled(&harness, Role::CheckBox, "Show event 0"),
        Toggled::False
    );
}

/// The status line names the shown annotations under the pointer.
#[test]
fn the_status_line_names_the_annotations_under_the_pointer() {
    // "sleep stage N2" from 7.625 s to 10.125 s; "eyes closed" and "Elektrodenprüfung µV",
    // events at 15 s.
    let file = without_triggers("edf+d-mixed");
    let mut harness = window(file.path());
    ready(&mut harness);
    let traces = viewer(&harness).layout().unwrap().traces;
    let at = |harness: &Harness<'_, KymoApp>, t: f64| {
        let x = viewer(harness).view().x_at(t, f64::from(traces.width()));
        egui::pos2(traces.left() + x as f32, traces.top() + 20.0)
    };
    let status = |harness: &Harness<'_, KymoApp>| viewer(harness).status().left.clone();

    let p = at(&harness, 9.0);
    hover(&mut harness, p);
    assert!(
        status(&harness).ends_with(" · sleep stage N2"),
        "{}",
        status(&harness)
    );
    let p = at(&harness, 12.0);
    hover(&mut harness, p);
    assert!(!status(&harness).contains("N2"), "{}", status(&harness));
    // An event line is hit from a point away.
    let p = at(&harness, 15.0) + egui::vec2(1.0, 0.0);
    hover(&mut harness, p);
    assert!(
        status(&harness).ends_with(" · eyes closed, Elektrodenprüfung µV"),
        "{}",
        status(&harness)
    );

    // Hidden annotations aren't named.
    click_in_list(
        &mut harness,
        egui::accesskit::Role::CheckBox,
        "Show eyes closed",
    );
    hover(&mut harness, p);
    assert!(
        status(&harness).ends_with(" · Elektrodenprüfung µV"),
        "{}",
        status(&harness)
    );

    // Off the traces, they go away with the time.
    hover(&mut harness, egui::pos2(5.0, 5.0));
    assert_eq!(status(&harness), "");
}

/// Without annotations or stim channels there is no legend.
#[test]
fn recordings_without_annotations_have_no_legend() {
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.annotations.clear();
    spec.channels.retain(|c| !c.trigger);
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, file.path()).unwrap();
    let mut harness = window(file.path());
    ready(&mut harness);
    let role = egui::accesskit::Role::CheckBox;
    assert!(
        harness
            .query_by_role_and_label(role, "Show Annotations")
            .is_none()
    );
    assert_eq!(
        viewer(&harness).marks(),
        &kymo::annotations::Marks::default()
    );
}

/// The stim annotations of `spec`'s Status channel (channel 4, 512 Hz), brute force from the
/// codes it was written with: onsets and codes, in time order, where `event(before, now)`
/// says whether a change is one.
fn expected_stim(
    spec: &kymo_testgen::Spec,
    event: fn(i32, i32) -> bool,
    initial: bool,
) -> Vec<(f64, String)> {
    let mut events = Vec::new();
    let mut before = None;
    for i in 0..60 * 512 {
        let code = spec.trigger_code(4, i).unwrap();
        let is_event = match before {
            None => initial && code != 0,
            Some(b) => b != code && code != 0 && event(b, code),
        };
        if is_event {
            events.push((i as f64 / 512.0, code.to_string()));
        }
        before = Some(code);
    }
    events
}

/// The viewer's stim annotations, onsets and codes; each must belong to Status alone and last
/// no time.
fn stim_annotations(harness: &Harness<'_, KymoApp>) -> Vec<(f64, String)> {
    viewer(harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin == kymo_core::Origin::Stim)
        .map(|a| {
            assert_eq!(a.data.channels, [kymo_core::ChannelId(4)]);
            assert_eq!(a.data.duration, 0.0);
            (a.data.onset, a.data.description.clone())
        })
        .collect()
}

fn assert_stim(got: &[(f64, String)], want: &[(f64, String)]) {
    assert_eq!(got.len(), want.len(), "{got:?}\n{want:?}");
    for (g, w) in got.iter().zip(want) {
        assert!(
            (g.0 - w.0).abs() < 1e-9 && g.1 == w.1,
            "{g:?} instead of {w:?}"
        );
    }
}

fn file_annotations(harness: &Harness<'_, KymoApp>) -> Vec<kymo_core::Annotation> {
    viewer(harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin == kymo_core::Origin::File)
        .cloned()
        .collect()
}

/// Opens View → Stim Annotations.
fn open_stim_menu(harness: &mut Harness<'_, KymoApp>) {
    harness.get_by_label("View").click();
    harness.run_steps(2);
    harness.get_by_label("Stim Annotations").click();
    harness.run_steps(2);
}

/// Whether `item` in View → Stim Annotations is enabled and checked; leaves the menus closed.
fn stim_item(harness: &mut Harness<'_, KymoApp>, item: &str) -> (bool, bool) {
    open_stim_menu(harness);
    let node = harness.get_by_label(item);
    let enabled = !node.accesskit_node().is_disabled();
    let checked = node.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True);
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(
        harness.query_by_label(item).is_none(),
        "the menu stayed open"
    );
    (enabled, checked)
}

fn choose_stim(harness: &mut Harness<'_, KymoApp>, item: &str) {
    open_stim_menu(harness);
    harness.get_by_label(item).click();
    harness.run_steps(2);
}

/// Whether a stim channel's code changes to a non-zero one are events, by rule.
fn every_change(_: i32, _: i32) -> bool {
    true
}
fn from_zero(before: i32, _: i32) -> bool {
    before == 0
}
fn increasing(before: i32, now: i32) -> bool {
    now > before
}

#[test]
fn stim_annotations_follow_the_rule_and_keep_the_file_annotations() {
    use egui::accesskit::Role;
    // A seed whose first code is not 0, so Initial Code makes a difference.
    let seed = (0..)
        .find(|&seed| {
            let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
            spec.seed ^= seed;
            spec.trigger_code(4, 0) != Some(0)
        })
        .unwrap();
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.seed ^= seed;
    let file = seeded("bdf+c-small", seed);
    let mut harness = window(file.path());
    ready(&mut harness);

    let want = expected_stim(&spec, every_change, false);
    assert!(want.len() > 20, "{} events", want.len());
    assert_stim(&stim_annotations(&harness), &want);
    assert_eq!(stim_item(&mut harness, "Every Change"), (true, true));
    assert_eq!(stim_item(&mut harness, "Initial Code"), (true, false));
    // The legend lists the codes after the file's descriptions, all shown.
    let legend = viewer(&harness).legend();
    let codes: std::collections::BTreeSet<&str> = want.iter().map(|w| w.1.as_str()).collect();
    for code in &codes {
        let entry = legend.entry(None, code).unwrap();
        assert!(entry.shown && entry.count > 0, "{entry:?}");
    }
    assert_eq!(legend.shown_count(), (12 + codes.len(), 12 + codes.len()));

    // The file's annotations and the legend's choices stay through rule changes.
    click_in_list(&mut harness, Role::CheckBox, "Show trigger 0");
    let files = file_annotations(&harness);
    assert_eq!(files.len(), 12);
    for (item, event) in [
        ("From Zero", from_zero as fn(i32, i32) -> bool),
        ("Increasing", increasing),
    ] {
        choose_stim(&mut harness, item);
        assert_stim(
            &stim_annotations(&harness),
            &expected_stim(&spec, event, false),
        );
        assert_eq!(stim_item(&mut harness, item), (true, true));
        assert_eq!(stim_item(&mut harness, "Every Change"), (true, false));
        assert_eq!(file_annotations(&harness), files);
        assert!(
            !viewer(&harness)
                .legend()
                .entry(None, "trigger 0")
                .unwrap()
                .shown
        );
    }
    choose_stim(&mut harness, "Initial Code");
    let initial = expected_stim(&spec, increasing, true);
    assert_eq!(initial[0].0, 0.0);
    assert_stim(&stim_annotations(&harness), &initial);
    assert_eq!(stim_item(&mut harness, "Initial Code"), (true, true));

    // The recording opened next keeps the rule.
    let ctx = harness.ctx.clone();
    harness.state_mut().open(&ctx, file.path().to_owned());
    ready(&mut harness);
    assert_stim(&stim_annotations(&harness), &initial);
    assert_eq!(stim_item(&mut harness, "Increasing"), (true, true));
}

#[test]
fn stim_stim_items_need_stim_channels() {
    let file = without_triggers("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    assert!(stim_annotations(&harness).is_empty());
    for item in ["Every Change", "From Zero", "Increasing", "Initial Code"] {
        assert!(!stim_item(&mut harness, item).0, "{item}");
    }
}

/// A reopened recording has its events from its sidecar in its first frame.
#[test]
fn stim_annotations_come_with_the_sidecar() {
    let file = TempFile::preset("bdf+c-small");
    let dir = TempDir::new();
    let open = |path: &Path| {
        let path = path.to_owned();
        let sidecars = dir.path().to_owned();
        Harness::builder().with_size(SIZE).build_eframe(move |cc| {
            KymoApp::new(
                &cc.egui_ctx,
                Gpu::none("no GPU in this test"),
                Options {
                    sidecar_dir: Some(sidecars),
                    ..options()
                },
                Some(path),
            )
        })
    };
    let spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    let want = expected_stim(&spec, every_change, false);
    {
        let mut harness = open(file.path());
        ready(&mut harness);
        assert_stim(&stim_annotations(&harness), &want);
        // Until the sidecar is complete: closed while it builds, the next open would build
        // it again (slow runners got there).
        step_until(&mut harness, "the sidecar", |app| {
            app.viewer()
                .is_some_and(|v| !v.status().right.contains("Building"))
        });
    }
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "no sidecar"
    );

    let mut harness = open(file.path());
    step_until(&mut harness, "the recording", |app| app.viewer().is_some());
    assert!(!viewer(&harness).status().right.contains("Building"));
    assert_stim(&stim_annotations(&harness), &want);
    drop(harness);
}

#[test]
fn the_sidebar_hides_and_shows() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let width = |h: &Harness<'_, KymoApp>| viewer(h).layout().unwrap().traces.width();
    let with_sidebar = width(&harness);
    harness.get_by_label("Format");
    choose(&mut harness, "View", "Show Sidebar");
    assert!(!harness.state().sidebar_shown());
    assert!(harness.query_by_label("Format").is_none());
    assert!(width(&harness) > with_sidebar + 100.0);
    choose(&mut harness, "View", "Show Sidebar");
    harness.get_by_label("Format");
    assert_eq!(width(&harness), with_sidebar);
}

/// The window opening `path` with `settings`, without a GPU.
fn window_with(path: &Path, settings: Settings) -> Harness<'static, KymoApp> {
    let path = path.to_owned();
    Harness::builder().with_size(SIZE).build_eframe(move |cc| {
        KymoApp::new(
            &cc.egui_ctx,
            Gpu::none("no GPU in this test"),
            Options {
                settings,
                ..options()
            },
            Some(path),
        )
    })
}

/// Sets the number field labeled `label` in the Settings window.
fn set_field(harness: &mut Harness<'_, KymoApp>, label: &str, value: f64) {
    harness
        .get_by_role_and_label(egui::accesskit::Role::SpinButton, label)
        .set_value_accesskit(value);
    harness.run_steps(2);
}

/// Picks `choice` from the menu labeled `label` in the Settings window.
fn pick_budget(harness: &mut Harness<'_, KymoApp>, label: &str, choice: &str) {
    harness
        .get_by_role_and_label(egui::accesskit::Role::ComboBox, label)
        .click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, choice)
        .click();
    harness.run_steps(2);
}

#[test]
fn settings_apply_at_once() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    harness.key_press_modifiers(command(), Key::Comma);
    harness.step();
    assert!(harness.state().settings_open());

    // The tile cache: the service's budget changes, and the cached tiles stay within it.
    pick_budget(&mut harness, "Tile Cache", "128 MB");
    assert_eq!(harness.state().settings().tile_cache_mb, 128);
    let cache = viewer(&harness).cache();
    assert_eq!(cache.budget(), 128 << 20);
    assert!(cache.bytes() <= cache.budget());

    // The default window: ⌘0 shows it from now on; the window on screen stays.
    set_field(&mut harness, "Default Window", 12.5);
    assert_eq!(viewer(&harness).view().time(), 0.0..20.0);
    harness.key_press(Key::ArrowRight);
    harness.key_press_modifiers(command(), Key::Num0);
    harness.step();
    assert_eq!(viewer(&harness).view().time(), 5.0..17.5);

    pick_budget(&mut harness, "GPU Memory", "1 GB");
    assert_eq!(harness.state().settings().gpu_memory_mb, 1024);
    // The default is offered, and marked.
    pick_budget(&mut harness, "Tile Cache", "256 MB (Default)");
    assert_eq!(viewer(&harness).cache().budget(), 256 << 20);

    // Restore Defaults, at once too.
    harness.get_by_label("Restore Defaults").click();
    harness.run_steps(2);
    assert_eq!(*harness.state().settings(), Settings::default());
    assert_eq!(viewer(&harness).cache().budget(), 256 << 20);
    harness.key_press_modifiers(command(), Key::Num0);
    harness.step();
    assert_eq!(viewer(&harness).view().time(), 5.0..25.0);
}

/// The Default Window's stepper: its arrows step a second, held they repeat, the arrow keys
/// step while the field has focus, and typed text applies on Return, never past the range.
#[test]
fn default_window_steps_and_takes_typed_values() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    harness.key_press_modifiers(command(), Key::Comma);
    harness.step();
    let seconds = |harness: &Harness<'_, KymoApp>| harness.state().settings().default_window_s;
    let arrow = |harness: &Harness<'_, KymoApp>, name: &str| {
        harness
            .get_by_role_and_label(egui::accesskit::Role::Button, name)
            .rect()
            .center()
    };

    let up = arrow(&harness, "Up");
    harness.hover_at(up);
    harness.event(egui::Event::PointerButton {
        pos: up,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: up,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(2);
    assert_eq!(seconds(&harness), 21.0);

    // Held, it repeats, for as long as it is held (egui's frames here are a quarter second
    // apart) and while the pointer is on it.
    let press = |harness: &mut Harness<'_, KymoApp>, pos, pressed| {
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    };
    press(&mut harness, up, true);
    harness.run_steps(12);
    let held = seconds(&harness);
    assert!(held > 21.0 + 20.0, "{held}");
    harness.hover_at(up + egui::vec2(60.0, 0.0));
    harness.run_steps(4);
    let off = seconds(&harness);
    harness.run_steps(4);
    assert_eq!(seconds(&harness), off);
    press(&mut harness, up + egui::vec2(60.0, 0.0), false);
    harness.run_steps(2);
    assert_eq!(seconds(&harness), off);
    set_field(&mut harness, "Default Window", 21.0);
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Down")
        .click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Down")
        .click();
    harness.run_steps(2);
    assert_eq!(seconds(&harness), 19.0);

    // Typed, with a decimal comma.
    let field = || egui::accesskit::Role::SpinButton;
    harness
        .get_by_role_and_label(field(), "Default Window")
        .focus();
    harness.step();
    harness.key_press_modifiers(command(), Key::A);
    harness
        .get_by_role_and_label(field(), "Default Window")
        .type_text("12,5");
    harness.step();
    // Not before Return.
    assert_eq!(seconds(&harness), 19.0);
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    assert_eq!(seconds(&harness), 12.5);

    // The arrow keys, in the field.
    harness
        .get_by_role_and_label(field(), "Default Window")
        .focus();
    harness.step();
    harness.key_press(Key::ArrowUp);
    harness.run_steps(2);
    assert_eq!(seconds(&harness), 13.0);
    for _ in 0..2 {
        harness.key_press(Key::ArrowDown);
        harness.step();
    }
    assert_eq!(seconds(&harness), 11.0);

    // Past the range, typed: its end; unreadable: as it was.
    harness.key_press_modifiers(command(), Key::A);
    harness
        .get_by_role_and_label(field(), "Default Window")
        .type_text("99999");
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    assert_eq!(seconds(&harness), 3600.0);
    harness
        .get_by_role_and_label(field(), "Default Window")
        .focus();
    harness.step();
    harness.key_press_modifiers(command(), Key::A);
    harness
        .get_by_role_and_label(field(), "Default Window")
        .type_text("soon");
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    assert_eq!(seconds(&harness), 3600.0);
    assert!(harness.state().settings_open());
}

/// Steps until a label containing `text` shows.
fn step_until_label(harness: &mut Harness<'_, KymoApp>, text: &str) {
    let start = Instant::now();
    while harness.query_by_label_contains(text).is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for {text:?}"
        );
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Settings shows the disk cache's size and clears it, but for the open recording's sidecar
/// and files kymo didn't write; settings stay. Without a cache directory there is no row.
#[test]
fn settings_clear_the_disk_cache() {
    let file = TempFile::preset("bdf+c-small");
    let dir = TempDir::new();
    let hash = "0d5284d4f6dad0bd4380c50b1337de3dbb3982bdc8522b48e7e6ef04bebbaafd";
    let other = dir.path().join(format!("{hash}.kymo-pyramid"));
    std::fs::write(&other, vec![0u8; 3 << 20]).unwrap();
    let foreign = dir.path().join("notes.txt");
    std::fs::write(&foreign, b"mine").unwrap();
    let settings = Settings {
        tile_cache_mb: 1024,
        ..Settings::default()
    };
    let path = file.path().to_owned();
    let sidecars = dir.path().to_owned();
    // Tall enough for the last row of Settings to show without scrolling.
    let size = egui::vec2(SIZE.x, 900.0);
    let mut harness = Harness::builder().with_size(size).build_eframe(move |cc| {
        KymoApp::new(
            &cc.egui_ctx,
            Gpu::none("no GPU in this test"),
            Options {
                sidecar_dir: Some(sidecars),
                settings,
                ..options()
            },
            Some(path),
        )
    });
    ready(&mut harness);
    let open = viewer(&harness).sidecar_path().unwrap();
    assert!(open.exists());

    choose(&mut harness, "Edit", "Settings");
    step_until_label(&mut harness, " in 2 files");
    // The window takes a few frames to grow to its content.
    harness.run_steps(10);
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Disk Cache")
        .click();
    step_until_label(&mut harness, "3 MB in 1 file cleared, 1 file in use kept");
    assert!(!other.exists());
    assert!(open.exists());
    assert!(foreign.exists());
    assert_eq!(harness.state().settings().tile_cache_mb, 1024);

    // Closed and opened again, it is measured again.
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert!(!harness.state().settings_open());
    choose(&mut harness, "Edit", "Settings");
    step_until_label(&mut harness, " in 1 file");
    assert!(harness.query_by_label_contains("cleared").is_none());
    drop(harness);

    let mut harness = window(file.path());
    ready(&mut harness);
    choose(&mut harness, "Edit", "Settings");
    harness.run_steps(2);
    harness.get_by_label("Tile Cache");
    assert!(harness.query_by_label("Disk Cache").is_none());
}

/// Settings → Font puts the system's font in front of egui's for text, where the platform
/// has one (macOS, Windows), and takes it out again; off by default. Elsewhere there is no
/// such setting. Widths measured once (the names column's, the channel list's columns) are
/// measured again in the new font, as a recording opened in it has them.
#[test]
fn the_system_font_setting_switches_the_fonts() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    // The first font of text.
    let first = |h: &Harness<'_, KymoApp>| {
        h.ctx
            .fonts(|f| f.definitions().families[&egui::FontFamily::Proportional][0].clone())
    };
    // The names column's width, and where the channel list's unit column starts.
    let widths = |h: &Harness<'_, KymoApp>| {
        let names = viewer(h).layout().unwrap().names.width();
        let unit = h
            .query_all_by_role_and_label(egui::accesskit::Role::Label, "\u{b5}V")
            .next()
            .unwrap()
            .rect()
            .left();
        (names, unit)
    };
    let builtin = first(&harness);
    assert_ne!(builtin, "System");
    let builtin_widths = widths(&harness);
    harness.key_press_modifiers(command(), Key::Comma);
    harness.step();
    let Some(_) = kymo::ui::fonts::SYSTEM_FONT_NAME else {
        assert!(harness.query_by_label("System Font").is_none());
        return;
    };
    harness.get_by_label("System Font").click();
    harness.run_steps(3);
    assert!(harness.state().settings().system_font);
    // Where the system's fonts are where Kymo looks for them, as on every Mac.
    if cfg!(target_os = "macos") {
        assert_eq!(first(&harness), "System");
    }
    if first(&harness) == "System" {
        let mut fresh = window_with(
            file.path(),
            Settings {
                system_font: true,
                ..Settings::default()
            },
        );
        ready(&mut fresh);
        let system_widths = widths(&fresh);
        assert_ne!(system_widths, builtin_widths);
        assert_eq!(widths(&harness), system_widths);
    }
    harness.get_by_label("System Font").click();
    harness.run_steps(3);
    assert!(!harness.state().settings().system_font);
    assert_eq!(first(&harness), builtin);
    assert_eq!(widths(&harness), builtin_widths);
}

#[test]
fn anti_aliasing_follows_the_menu_and_shortcut() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    // Off by default.
    assert!(!harness.state().settings().antialias);
    assert!(!checked(&mut harness, "View", "Anti-Aliasing"));
    choose(&mut harness, "View", "Anti-Aliasing");
    assert!(harness.state().settings().antialias);
    assert!(checked(&mut harness, "View", "Anti-Aliasing"));
    choose(&mut harness, "View", "Anti-Aliasing");
    assert!(!harness.state().settings().antialias);

    // ⇧⌘A switches it too.
    harness.key_press_modifiers(command() | Modifiers::SHIFT, Key::A);
    harness.step();
    assert!(harness.state().settings().antialias);

    // A setting, so it can change without a recording.
    harness.key_press_modifiers(command(), Key::W);
    harness.step();
    assert!(enabled(&mut harness, "View", "Anti-Aliasing"));
    choose(&mut harness, "View", "Anti-Aliasing");
    assert!(!harness.state().settings().antialias);
}

/// Each type's amplitude per channel spacing, in [`KINDS`] order.
fn amplitudes(harness: &Harness<'_, KymoApp>) -> [f64; KINDS.len()] {
    let view = viewer(harness).view();
    KINDS.map(|kind| view.amplitude(&kind))
}

#[test]
fn auto_scale_fits_the_window_from_the_menu_keys_and_setting() {
    let file = typed_preset();
    // Not on opening, which is the default: the fixed defaults first, then ⌘R.
    let mut harness = window_with(
        file.path(),
        Settings {
            auto_scale_on_open: false,
            ..Settings::default()
        },
    );
    ready(&mut harness);
    let defaults = amplitudes(&harness);

    // ⌘R: the generated signals reach 15 uV, their spikes (to the digital limits) are rare.
    harness.key_press_modifiers(command(), Key::R);
    harness.run_steps(2);
    let fitted = amplitudes(&harness);
    let signal = kymo_testgen::EEG_AMPLITUDE * 1e-6;
    for kind in [ChannelKind::EEG, ChannelKind::EOG, ChannelKind::ECG] {
        let amplitude = fitted[kind_index(kind.clone())];
        assert!(
            (signal / 4.0..=signal * 2.0).contains(&amplitude),
            "{kind:?}: {amplitude}"
        );
    }
    // Stim keeps its scale, and so do types without channels.
    for kind in [ChannelKind::STIM, ChannelKind::EMG, ChannelKind::MISC] {
        assert_eq!(
            fitted[kind_index(kind.clone())],
            defaults[kind_index(kind.clone())],
            "{kind:?}"
        );
    }

    // Once fitted, the scale stays as the view moves on.
    harness.key_press(Key::ArrowRight);
    harness.run_steps(2);
    step_until(&mut harness, "the tiles", |app| {
        app.viewer().unwrap().complete()
    });
    assert_eq!(amplitudes(&harness), fitted);
    harness.key_press(Key::ArrowLeft);
    harness.step();

    // Default Scale (⌥⌘0) brings the defaults back; the menu item fits again.
    harness.key_press_modifiers(command() | Modifiers::ALT, Key::Num0);
    harness.step();
    assert_eq!(amplitudes(&harness), defaults);
    choose(&mut harness, "View", "Auto Scale");
    harness.run_steps(2);
    assert_eq!(amplitudes(&harness), fitted);
    assert!(enabled(&mut harness, "View", "Default Scale"));

    // The setting: a recording opens fitted, as ⌘R fits it.
    let settings = Settings {
        auto_scale_on_open: true,
        ..Settings::default()
    };
    let mut harness = window_with(file.path(), settings);
    ready(&mut harness);
    assert_eq!(amplitudes(&harness), fitted);

    // The Settings checkbox shows and changes it.
    harness.key_press_modifiers(command(), Key::Comma);
    harness.run_steps(3);
    // Below the fold in a window this small; scrolled to once the window has its size,
    // and clicked once the scroll's animation ends.
    harness.get_by_label("Automatic on Opening").scroll_to_me();
    harness.run_steps(10);
    harness.get_by_label("Automatic on Opening").click();
    harness.run_steps(2);
    assert!(!harness.state().settings().auto_scale_on_open);
}

#[test]
fn auto_scale_waits_for_the_windows_tiles() {
    // Long enough that its pyramid takes a while: 16 channels at 1000 Hz, 30 min (58 MB).
    let mut spec = kymo_testgen::Spec::preset("edf-64ch-1h").unwrap();
    spec.channels.truncate(16);
    spec.n_records = 1800;
    let file = TempFile::written(&spec);
    let mut harness = window(file.path());
    // One frame per step, so the view is still the first one when the viewer appears.
    while harness.state().viewer().is_none() {
        harness.step();
    }
    let defaults = amplitudes(&harness);
    // The whole recording, from the pyramid's top levels, which the build makes last; Fit
    // and Auto Scale in one frame, so the tiles can't arrive between them.
    let input = harness.input_mut();
    for key in [Key::Num9, Key::R] {
        input.events.push(egui::Event::Key {
            key,
            pressed: true,
            modifiers: command(),
            repeat: false,
            physical_key: None,
        });
    }
    let mut incomplete = 0;
    loop {
        harness.step();
        if viewer(&harness).complete() {
            break;
        }
        assert_eq!(amplitudes(&harness), defaults);
        incomplete += 1;
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(incomplete > 0, "the whole recording had its tiles at once");
    assert_eq!(
        viewer(&harness).view().time(),
        0.0..viewer(&harness).view().duration()
    );
    let fitted = amplitudes(&harness);
    assert_ne!(fitted, defaults);
    // What it fits then is what the complete window gives.
    harness.key_press_modifiers(command() | Modifiers::ALT, Key::Num0);
    harness.key_press_modifiers(command(), Key::R);
    harness.run_steps(2);
    assert_eq!(amplitudes(&harness), fitted);
}

#[test]
fn auto_scale_ignores_a_dc_offset() {
    let eeg = kind_index(ChannelKind::EEG);
    let mut fitted = Vec::new();
    for offset in [0.0, 21_000.0] {
        let file = offset_preset("bdf+c-small", offset);
        let mut harness = window_with(
            file.path(),
            Settings {
                auto_scale_on_open: false,
                ..Settings::default()
            },
        );
        ready(&mut harness);
        let default = amplitudes(&harness)[eeg];
        harness.key_press_modifiers(command(), Key::R);
        harness.run_steps(2);
        fitted.push(amplitudes(&harness)[eeg]);
        assert_ne!(fitted.last(), Some(&default));
    }
    assert!(
        (fitted[0] - fitted[1]).abs() <= fitted[0] * 1e-6,
        "{fitted:?}"
    );
}

/// eframe's storage, in memory.
#[derive(Default)]
struct Memory(std::collections::HashMap<String, String>);

impl eframe::Storage for Memory {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), value);
    }
    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }
    fn flush(&mut self) {}
}

/// The window opening `path` with `options`, with the system fonts the app has: egui's
/// color picker draws a 📋.
fn fonts_window(path: &Path, options: Options) -> Harness<'static, KymoApp> {
    let path = path.to_owned();
    Harness::builder()
        .with_size(SIZE)
        .with_context_setup(system_fonts)
        .build_eframe(move |cc| {
            KymoApp::new(
                &cc.egui_ctx,
                Gpu::none("no GPU in this test"),
                options,
                Some(path),
            )
        })
}

/// Opens the color picker of the swatch labeled `label`, unless a picker is open, and picks
/// `rgb` in it.
fn pick(harness: &mut Harness<'_, KymoApp>, label: &str, rgb: [u8; 3]) {
    let open = harness
        .query_by_role_and_label(egui::accesskit::Role::SpinButton, "R")
        .is_some();
    if !open {
        harness.get_by_label(label).click();
        harness.run_steps(2);
    }
    for (channel, value) in ["R", "G", "B"].into_iter().zip(rgb) {
        harness
            .get_by_role_and_label(egui::accesskit::Role::SpinButton, channel)
            .set_value_accesskit(f64::from(value));
        harness.run_steps(2);
    }
}

/// A type's color is picked in the Channel Types window, starting at its color, and reset by
/// its row's Reset (only while it isn't the default) or Settings' Restore Defaults.
#[test]
fn channel_type_colors_are_picked_and_reset() {
    let file = typed_preset();
    let mut harness = fonts_window(file.path(), options());
    ready(&mut harness);
    let picked = |h: &Harness<'_, KymoApp>| h.state().settings().types.clone();
    open_channel_types(&mut harness);

    // The picker starts at the type's color; Reset is off while it is the default.
    harness.get_by_label("Color of EOG").click();
    harness.run_steps(2);
    let visuals = harness.ctx.global_style().visuals.clone();
    let default = trace_color(&ChannelKind::EOG, &visuals);
    let value = |h: &Harness<'_, KymoApp>, channel: &str| {
        h.get_by_role_and_label(egui::accesskit::Role::SpinButton, channel)
            .accesskit_node()
            .numeric_value()
    };
    assert_eq!(value(&harness, "B"), Some(f64::from(default.b())));
    assert!(harness.query_by_label("Reset EOG").is_none());

    pick(&mut harness, "Color of EOG", [10, 200, 30]);
    let mut want = TypeSettings::default();
    want.set_color(
        &ChannelKind::EOG,
        Some(egui::Color32::from_rgb(10, 200, 30)),
    );
    assert_eq!(picked(&harness), want);
    close_picker(&mut harness);
    assert!(harness.state().channel_types_open());
    harness.get_by_label("Reset EOG").click();
    harness.run_steps(2);
    assert_eq!(picked(&harness), TypeSettings::default());

    // Restore Defaults resets them too.
    pick(&mut harness, "Color of EOG", [10, 200, 30]);
    assert_eq!(picked(&harness), want);
    close_picker(&mut harness);
    harness.key_press_modifiers(command(), Key::Comma);
    harness.run_steps(10);
    harness.get_by_label("Restore Defaults").click();
    harness.run_steps(2);
    assert_eq!(picked(&harness), TypeSettings::default());
}

/// Opens the Channel Types window from Settings' Channel Types row (Settings stays open).
fn open_channel_types(harness: &mut Harness<'_, KymoApp>) {
    harness.key_press_modifiers(command(), Key::Comma);
    // Until the window has faded in, which takes no clicks before.
    harness.run_steps(10);
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Channel Types")
        .click();
    harness.run_steps(10);
    assert!(harness.state().channel_types_open());
}

/// Closes an open color picker, as Esc does, leaving the window it is in open.
fn close_picker(harness: &mut Harness<'_, KymoApp>) {
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    assert!(
        harness
            .query_by_role_and_label(egui::accesskit::Role::SpinButton, "R")
            .is_none()
    );
}

#[test]
fn annotation_colors_are_picked_per_description_and_kept() {
    use AnnotationColor::{Custom, Palette};
    let file = without_triggers("edf+d-mixed");
    let mut harness = fonts_window(file.path(), options());
    ready(&mut harness);
    let color =
        |h: &Harness<'_, KymoApp>, d: &str| viewer(h).legend().entry(None, d).unwrap().color;
    let line_colors = |h: &Harness<'_, KymoApp>| {
        let mut colors: Vec<_> = viewer(h).marks().lines.iter().map(|m| m.color).collect();
        colors.sort();
        colors
    };
    let green = Custom([10, 200, 30]);
    assert_eq!(line_colors(&harness), [Palette(0), Palette(2), Palette(3)]);

    // "event 0" (an event at the start) turns green; the others keep their colors.
    pick(&mut harness, "Color of event 0", [10, 200, 30]);
    assert_eq!(
        harness.state().annotation_colors(),
        &AnnotationColors::from([("event 0".to_owned(), [10, 200, 30])])
    );
    assert_eq!(color(&harness, "event 0"), green);
    assert_eq!(color(&harness, "sleep stage N2"), Palette(1));
    assert_eq!(line_colors(&harness), [Palette(2), Palette(3), green]);

    // Reset: the palette color again.
    harness.get_by_label("Reset").click();
    harness.run_steps(2);
    assert!(harness.state().annotation_colors().is_empty());
    assert_eq!(color(&harness, "event 0"), Palette(0));
    assert_eq!(line_colors(&harness), [Palette(0), Palette(2), Palette(3)]);

    // Kept for the next run, and for every recording with the description.
    pick(&mut harness, "Color of event 0", [10, 200, 30]);
    let mut storage = Memory::default();
    eframe::App::save(harness.state_mut(), &mut storage);
    let other = TempFile::preset("edf+d-mixed");
    let options = Options {
        annotation_colors: load_annotation_colors(Some(&storage)),
        ..options()
    };
    let mut harness = fonts_window(other.path(), options);
    ready(&mut harness);
    assert_eq!(color(&harness, "event 0"), green);
}

#[test]
fn settings_are_kept_between_runs() {
    let file = TempFile::preset("bdf+c-small");
    let mut storage = Memory::default();
    {
        let mut harness = window(file.path());
        ready(&mut harness);
        harness.key_press_modifiers(command(), Key::Comma);
        harness.step();
        set_field(&mut harness, "Default Window", 8.0);
        pick_budget(&mut harness, "Tile Cache", "1 GB");
        eframe::App::save(harness.state_mut(), &mut storage);
    }

    // The next run starts with them: a recording opens with an 8 s window and a 1 GB cache.
    let settings = Settings::load(Some(&storage));
    assert_eq!(settings.default_window_s, 8.0);
    assert_eq!(settings.tile_cache_mb, 1024);
    let mut harness = window_with(file.path(), settings);
    ready(&mut harness);
    assert_eq!(viewer(&harness).view().time(), 0.0..8.0);
    assert_eq!(viewer(&harness).cache().budget(), 1024 << 20);
    assert!(!harness.state().settings_open());
}

#[test]
fn the_shortcut_overview_and_about_open() {
    let file = TempFile::new("bdf");
    // A missing file: both work without a recording.
    let mut harness = window(file.path());
    step_until(&mut harness, "the error", |app| app.error().is_some());
    harness.key_press_modifiers(command(), Key::Slash);
    harness.step();
    assert!(harness.state().shortcuts_open());
    harness.get_by_label("Trackpad and Mouse");
    harness.get_by_label("Scroll Channels");
    harness.key_press_modifiers(command(), Key::Slash);
    harness.step();
    assert!(!harness.state().shortcuts_open());
    assert!(harness.query_by_label("Scroll Channels").is_none());

    choose(&mut harness, "Help", "About Kymo");
    assert!(harness.state().about_open());
    harness.get_by_label(kymo::ui::menus::ABOUT);
    harness.get_by_label(kymo::ui::menus::COPYRIGHT);
}

#[test]
fn esc_and_close_close_the_window_in_front() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let open = |h: &Harness<'_, KymoApp>| (h.state().settings_open(), h.state().shortcuts_open());
    let press = |h: &mut Harness<'_, KymoApp>, modifiers, key| {
        h.key_press_modifiers(modifiers, key);
        h.run_steps(2);
    };

    // The one opened last is in front.
    press(&mut harness, command(), Key::Comma);
    press(&mut harness, command(), Key::Slash);
    assert_eq!(open(&harness), (true, true));
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(open(&harness), (true, false));
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(open(&harness), (false, false));

    // A click brings one to the front.
    press(&mut harness, command(), Key::Slash);
    press(&mut harness, command(), Key::Comma);
    harness.get_by_label("Pan Time").click();
    harness.run_steps(2);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(open(&harness), (true, false));

    // ⌘W closes the window in front, and with none the recording.
    press(&mut harness, command(), Key::W);
    assert_eq!(open(&harness), (false, false));
    assert!(harness.state().viewer().is_some());
    press(&mut harness, command(), Key::W);
    assert!(harness.state().viewer().is_none());

    // Without a recording, ⌘W still closes Settings.
    press(&mut harness, command(), Key::Comma);
    assert!(harness.state().settings_open());
    press(&mut harness, command(), Key::W);
    assert!(!harness.state().settings_open());
}

/// The point in the traces at time `t`, `dy` points below their top.
fn at_time(harness: &Harness<'_, KymoApp>, t: f64, dy: f32) -> egui::Pos2 {
    let traces = viewer(harness).layout().unwrap().traces;
    let x = viewer(harness).view().x_at(t, f64::from(traces.width()));
    egui::pos2(traces.left() + x as f32, traces.top() + dy)
}

/// A click in the traces at time `t`, near their top.
fn click_at_time(harness: &mut Harness<'_, KymoApp>, t: f64) {
    let p = at_time(harness, t, 20.0);
    click(harness, p);
}

/// A primary drag from `from` to `to`, through the middle.
fn drag(harness: &mut Harness<'_, KymoApp>, from: egui::Pos2, to: egui::Pos2) {
    hover(harness, from);
    harness.drag_at(from);
    harness.step();
    for pos in [from + (to - from) / 2.0, to] {
        hover(harness, pos);
    }
    harness.drop_at(to);
    harness.step();
}

/// The annotations made in the viewer: onset, duration and description.
fn made(harness: &Harness<'_, KymoApp>) -> Vec<(f64, f64, String)> {
    viewer(harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin == kymo_core::Origin::Viewer)
        .inspect(|a| assert!(a.data.channels.is_empty()))
        .map(|a| (a.data.onset, a.data.duration, a.data.description.clone()))
        .collect()
}

fn press(harness: &mut Harness<'_, KymoApp>, modifiers: Modifiers, key: Key) {
    harness.key_press_modifiers(modifiers, key);
    harness.step();
}

/// Types `text` into the description of new annotations, in place of what it held,
/// deselecting first: with a selection, the field edits the selected annotation's.
fn describe(harness: &mut Harness<'_, KymoApp>, text: &str) {
    press(harness, Modifiers::NONE, Key::Escape);
    let field = "Description of new annotations";
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    press(harness, command(), Key::A);
    harness.get_by_label(field).type_text(text);
    harness.run_steps(2);
}

fn selected_data(harness: &Harness<'_, KymoApp>) -> Option<kymo_core::AnnotationData> {
    let viewer = viewer(harness);
    let id = viewer.selected()?;
    Some(viewer.document().annotations.get(id).unwrap().data.clone())
}

#[test]
fn annotation_mode_creates_selects_and_deletes_with_undo() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let count = viewer(&harness).document().annotations.len();
    assert!(!checked(&mut harness, "Edit", "Edit Annotations"));
    assert!(!enabled(&mut harness, "Edit", "Undo"));
    assert!(!enabled(&mut harness, "Edit", "Delete"));
    press(&mut harness, command(), Key::E);
    assert!(viewer(&harness).editing());
    assert!(checked(&mut harness, "Edit", "Edit Annotations"));
    let p = at_time(&harness, 12.0, 20.0);
    hover(&mut harness, p);
    let status = viewer(&harness).status().left.clone();
    assert!(status.starts_with("Editing annotations · "), "{status}");

    // A drag from right to left creates an annotation of all channels over its range,
    // selected, and doesn't pan.
    let (from, to) = (at_time(&harness, 4.0, 40.0), at_time(&harness, 2.0, 60.0));
    drag(&mut harness, from, to);
    let both = made(&harness);
    assert_eq!(both.len(), 1, "{both:?}");
    let x_tolerance = 1e-4;
    assert!((both[0].0 - 2.0).abs() < x_tolerance, "{both:?}");
    assert!((both[0].1 - 2.0).abs() < x_tolerance, "{both:?}");
    assert_eq!(both[0].2, "BAD");
    assert_eq!(viewer(&harness).document().annotations.len(), count + 1);
    assert_eq!(viewer(&harness).view().time(), 0.0..20.0);
    let id = viewer(&harness)
        .selected()
        .expect("the new annotation is selected");
    let entry = viewer(&harness)
        .legend()
        .entry(None, "BAD")
        .unwrap()
        .clone();
    assert_eq!((entry.count, entry.shown), (1, true));
    assert!(enabled(&mut harness, "Edit", "Undo"));
    assert!(enabled(&mut harness, "Edit", "Delete"));
    // Outlined, and named in the status line.
    let outline = viewer(&harness).outline().unwrap();
    let traces = viewer(&harness).layout().unwrap().traces;
    assert!((outline.left() - to.x).abs() <= 1.0, "{outline:?} {to:?}");
    assert!(
        (outline.right() - from.x).abs() <= 1.0,
        "{outline:?} {from:?}"
    );
    assert_eq!(outline.y_range(), traces.y_range());
    hover(&mut harness, p);
    let status = viewer(&harness).status().left.clone();
    assert!(
        status.starts_with("BAD at 0:02.000 for 2.000 s · "),
        "{status}"
    );
    // With a swatch in its color before it.
    let color = viewer(&harness).legend().entry(None, "BAD").unwrap().color;
    assert_eq!(viewer(&harness).status().swatch, Some(color));

    // A click away from annotations deselects; on it, selects.
    click_at_time(&mut harness, 12.0);
    assert_eq!(viewer(&harness).selected(), None);
    assert_eq!(viewer(&harness).outline(), None);
    assert_eq!(viewer(&harness).status().swatch, None);
    assert!(!enabled(&mut harness, "Edit", "Delete"));
    click_at_time(&mut harness, 3.0);
    assert_eq!(viewer(&harness).selected(), Some(id));

    // ⌫ deletes it; undo brings it back, selected; redo deletes it again.
    press(&mut harness, Modifiers::NONE, Key::Backspace);
    assert!(made(&harness).is_empty());
    assert_eq!(viewer(&harness).selected(), None);
    assert!(
        viewer(&harness)
            .legend()
            .entries()
            .all(|e| e.description != "BAD")
    );
    let shift_command = command() | Modifiers::SHIFT;
    press(&mut harness, command(), Key::Z);
    assert_eq!(made(&harness), both);
    assert_eq!(viewer(&harness).selected(), Some(id));
    press(&mut harness, shift_command, Key::Z);
    assert!(made(&harness).is_empty());
    assert!(!enabled(&mut harness, "Edit", "Redo"));
    assert!(enabled(&mut harness, "Edit", "Undo"));
    // ⌘Y redoes too, Delete deletes too, and the menu items act.
    choose(&mut harness, "Edit", "Undo");
    assert_eq!(made(&harness), both);
    press(&mut harness, Modifiers::NONE, Key::Delete);
    assert!(made(&harness).is_empty());
    choose(&mut harness, "Edit", "Undo");
    click_at_time(&mut harness, 3.0);
    choose(&mut harness, "Edit", "Delete");
    assert!(made(&harness).is_empty());
    press(&mut harness, command(), Key::Z);
    press(&mut harness, command(), Key::Z);
    // The creation undone: nothing left to undo.
    assert!(made(&harness).is_empty());
    assert_eq!(viewer(&harness).selected(), None);
    assert!(!viewer(&harness).history().can_undo());
    assert_eq!(viewer(&harness).document().annotations.len(), count);
    press(&mut harness, command(), Key::Y);
    assert_eq!(made(&harness), both);
    assert_eq!(viewer(&harness).selected(), Some(id));
    // Esc deselects.
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(viewer(&harness).selected(), None);

    // Out of annotation mode, clicks don't select, a drag pans, and undo still works.
    click_at_time(&mut harness, 3.0);
    assert_eq!(viewer(&harness).selected(), Some(id));
    choose(&mut harness, "Edit", "Edit Annotations");
    assert!(!viewer(&harness).editing());
    assert_eq!(viewer(&harness).selected(), None);
    click_at_time(&mut harness, 3.0);
    assert_eq!(viewer(&harness).selected(), None);
    hover(&mut harness, p);
    assert!(
        !viewer(&harness).status().left.contains("Editing"),
        "{}",
        viewer(&harness).status().left
    );
    let (from, to) = (at_time(&harness, 8.0, 40.0), at_time(&harness, 6.0, 40.0));
    drag(&mut harness, from, to);
    assert_eq!(made(&harness), both);
    let time = viewer(&harness).view().time();
    assert!((time.start - 2.0).abs() < 1e-3, "{time:?}");
    press(&mut harness, command(), Key::Z);
    assert!(made(&harness).is_empty());
}

#[test]
fn short_drags_and_an_empty_description_create_nothing() {
    let file = without_triggers("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    // Far enough to be a drag, but less than 3 points sideways.
    let from = at_time(&harness, 12.0, 20.0);
    drag(&mut harness, from, from + egui::vec2(2.5, 30.0));
    assert!(made(&harness).is_empty());
    assert!(!viewer(&harness).history().can_undo());
    // Back to where it started: what it made is gone again.
    let to = from + egui::vec2(40.0, 0.0);
    hover(&mut harness, from);
    harness.drag_at(from);
    harness.step();
    hover(&mut harness, to);
    assert_eq!(made(&harness).len(), 1);
    hover(&mut harness, from + egui::vec2(1.0, 20.0));
    assert!(made(&harness).is_empty());
    harness.drop_at(from + egui::vec2(1.0, 20.0));
    harness.step();
    assert!(made(&harness).is_empty());
    assert!(!viewer(&harness).history().can_undo());
    assert!(
        viewer(&harness)
            .legend()
            .entries()
            .all(|e| e.description != "BAD")
    );

    // Without a description, a drag makes nothing.
    let field = "Description of new annotations";
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    press(&mut harness, command(), Key::A);
    press(&mut harness, Modifiers::NONE, Key::Backspace);
    assert_eq!(viewer(&harness).description(), "");
    drag(&mut harness, from, to);
    assert!(made(&harness).is_empty());
    // A drag past the recording's start stops there.
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    harness.get_by_label(field).type_text("x");
    harness.run_steps(2);
    let start = at_time(&harness, 0.0, 20.0) - egui::vec2(30.0, 0.0);
    let from = at_time(&harness, 1.0, 20.0);
    drag(&mut harness, from, start);
    let got = made(&harness);
    assert_eq!(got.len(), 1);
    assert_eq!((got[0].0, got[0].2.as_str()), (0.0, "x"));
    assert!((got[0].1 - 1.0).abs() < 1e-4, "{got:?}");
    // And past its end, at the end.
    press(&mut harness, Modifiers::NONE, Key::End);
    let from = at_time(&harness, 59.0, 20.0);
    let end = at_time(&harness, 60.0, 20.0) + egui::vec2(30.0, 0.0);
    drag(&mut harness, from, end);
    let got = made(&harness);
    assert_eq!(got.len(), 2);
    assert!((got[1].0 - 59.0).abs() < 1e-4, "{got:?}");
    assert_eq!(got[1].0 + got[1].1, 60.0, "{got:?}");
}

#[test]
fn clicks_select_the_shortest_editable_annotation() {
    // File annotations "trigger 3" from 15.5 s to 16.5 s, events at 10.5 s and 20.5 s, and
    // stim annotations from the Status channel.
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    // Of another description, or the two would merge.
    describe(&mut harness, "long");
    let (a, b) = (at_time(&harness, 12.0, 40.0), at_time(&harness, 19.0, 40.0));
    drag(&mut harness, a, b);
    describe(&mut harness, "BAD");
    // Inside the selected one, a drag would move it.
    press(&mut harness, Modifiers::NONE, Key::Escape);
    let (a, b) = (at_time(&harness, 13.0, 40.0), at_time(&harness, 14.0, 40.0));
    drag(&mut harness, a, b);
    assert_eq!(made(&harness).len(), 2);
    let pick = |harness: &mut Harness<'_, KymoApp>, t: f64| {
        click_at_time(harness, t);
        selected_data(harness).map(|d| (d.description, d.duration.round(), d.origin))
    };
    use kymo_core::Origin::{File, Viewer};
    // The new one is selected, so a click on it selects the next longer one there.
    assert_eq!(selected_data(&harness).unwrap().duration.round(), 1.0);
    assert_eq!(pick(&mut harness, 13.5), Some(("long".into(), 7.0, Viewer)));
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(pick(&mut harness, 13.5), Some(("BAD".into(), 1.0, Viewer)));
    assert_eq!(pick(&mut harness, 12.5), Some(("long".into(), 7.0, Viewer)));
    // A file annotation inside the long one is shorter.
    assert_eq!(
        pick(&mut harness, 16.0),
        Some(("trigger 3".into(), 1.0, File))
    );

    // Stim annotations can't be selected: a click on one alone deselects.
    let stim = viewer(&harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin == kymo_core::Origin::Stim)
        .map(|a| a.data.onset)
        .find(|&t| (1.0..9.0).contains(&t) && (t - 5.5).abs() > 0.5)
        .expect("a stim annotation away from the file's");
    let p = at_time(&harness, stim, 20.0);
    hover(&mut harness, p);
    assert!(
        viewer(&harness).status().left.contains(" · "),
        "{}",
        viewer(&harness).status().left
    );
    assert_eq!(pick(&mut harness, stim), None);

    // A hidden description can't be selected, and hiding it deselects.
    assert_eq!(pick(&mut harness, 13.5).unwrap().0, "BAD");
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show BAD");
    harness.step();
    assert_eq!(viewer(&harness).selected(), None);
    assert_eq!(pick(&mut harness, 13.5).unwrap().0, "long");
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show BAD");
    assert!(pick(&mut harness, 13.5).is_some());
    // Creating one with a hidden description shows them.
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show BAD");
    let (a, b) = (at_time(&harness, 2.0, 40.0), at_time(&harness, 3.0, 40.0));
    drag(&mut harness, a, b);
    assert!(viewer(&harness).legend().entry(None, "BAD").unwrap().shown);
    assert!(pick(&mut harness, 13.5).is_some());

    // An event, "trigger 2", gets an outline around its line.
    assert_eq!(
        pick(&mut harness, 10.5),
        Some(("trigger 2".into(), 0.0, File))
    );
    let outline = viewer(&harness).outline().unwrap();
    let x = at_time(&harness, 10.5, 0.0).x;
    assert!(outline.width() >= 4.0, "{outline:?}");
    assert!(outline.x_range().contains(x), "{outline:?} {x}");
}

/// An annotation lying wholly over shorter ones, as over back-to-back trials, is reached
/// by clicking again: each click on the selected one selects the next longer one there.
#[test]
fn repeated_clicks_reach_every_annotation_under_the_pointer() {
    let file = without_triggers("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    // "a" from 2 to 4.1 s and "b" from 4 to 6 s, back to back; "long" from 3 to 5.5 s,
    // longer than either and nowhere on its own.
    for (description, from, to) in [("a", 2.0, 4.1), ("b", 4.0, 6.0), ("long", 3.0, 5.5)] {
        describe(&mut harness, description);
        press(&mut harness, Modifiers::NONE, Key::Escape);
        let (a, b) = (at_time(&harness, from, 40.0), at_time(&harness, to, 40.0));
        drag(&mut harness, a, b);
    }
    assert_eq!(made(&harness).len(), 3);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    let pick = |harness: &mut Harness<'_, KymoApp>, t: f64| {
        click_at_time(harness, t);
        selected_data(harness).map(|d| d.description)
    };
    assert_eq!(pick(&mut harness, 4.6).as_deref(), Some("b"));
    assert_eq!(pick(&mut harness, 4.6).as_deref(), Some("long"));
    // After the longest, the shortest again.
    assert_eq!(pick(&mut harness, 4.6).as_deref(), Some("b"));
    // Elsewhere, the shortest there first, whatever was selected.
    assert_eq!(pick(&mut harness, 3.5).as_deref(), Some("a"));
    assert_eq!(pick(&mut harness, 3.5).as_deref(), Some("long"));
    // Where three lie, all three in turn.
    assert_eq!(pick(&mut harness, 4.05).as_deref(), Some("b"));
    assert_eq!(pick(&mut harness, 4.05).as_deref(), Some("a"));
    assert_eq!(pick(&mut harness, 4.05).as_deref(), Some("long"));
    assert_eq!(pick(&mut harness, 4.05).as_deref(), Some("b"));
}

#[test]
fn the_description_field_names_new_annotations() {
    // Neither annotations nor stim channels: no legend until annotation mode.
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.annotations.clear();
    spec.channels.retain(|c| !c.trigger);
    let file = TempFile::written(&spec);
    let mut harness = window(file.path());
    ready(&mut harness);
    let field = "Description of new annotations";
    assert!(harness.query_by_label(field).is_none());
    press(&mut harness, command(), Key::E);
    let role = egui::accesskit::Role::CheckBox;
    assert!(
        harness
            .query_by_role_and_label(role, "Show Annotations")
            .is_none()
    );
    assert_eq!(
        harness.get_by_label(field).value().as_deref(),
        Some(kymo::viewer::DEFAULT_DESCRIPTION)
    );

    // Typed: new annotations take it.
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    press(&mut harness, command(), Key::A);
    harness.get_by_label(field).type_text("spike");
    harness.run_steps(2);
    assert_eq!(viewer(&harness).description(), "spike");
    // ⌘A selected the field's text, not the channels.
    assert!(viewer(&harness).view().selected().is_empty());
    // ⌘Z and ⇧⌘Z in the field undo and redo the typing (egui undoes quick typing at
    // once); so do the menu's Undo and Redo, which hand the field their keys.
    let shift_command = command() | Modifiers::SHIFT;
    press(&mut harness, command(), Key::Z);
    assert_eq!(viewer(&harness).description(), "BAD");
    press(&mut harness, shift_command, Key::Z);
    assert_eq!(viewer(&harness).description(), "spike");
    harness.event(kymo::app::text_field_key(Action::Undo).unwrap());
    harness.step();
    assert_eq!(viewer(&harness).description(), "BAD");
    harness.event(kymo::app::text_field_key(Action::Redo).unwrap());
    harness.step();
    assert_eq!(viewer(&harness).description(), "spike");
    // Straight from the field to the traces.
    assert!(harness.get_by_label(field).is_focused());
    let (a, b) = (at_time(&harness, 2.0, 40.0), at_time(&harness, 3.0, 40.0));
    drag(&mut harness, a, b);
    let got = made(&harness);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].2, "spike");
    // The drag took the keyboard from the field, which now shows the new annotation's
    // description: ⌫ deletes it, and undo brings it back.
    let selected = "Description of the selected annotation";
    assert!(!harness.get_by_label(selected).is_focused());
    assert_eq!(
        harness.get_by_label(selected).value().as_deref(),
        Some("spike")
    );
    press(&mut harness, Modifiers::NONE, Key::Backspace);
    assert!(made(&harness).is_empty());
    assert_eq!(viewer(&harness).description(), "spike");
    press(&mut harness, command(), Key::Z);
    assert_eq!(made(&harness).len(), 1);
    // Typing new annotations' description doesn't touch the annotations: ⌘Z there leaves
    // them.
    press(&mut harness, Modifiers::NONE, Key::Escape);
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    harness.get_by_label(field).type_text("!");
    harness.run_steps(2);
    press(&mut harness, command(), Key::Z);
    assert_eq!(made(&harness).len(), 1);
    assert_eq!(viewer(&harness).description(), "spike");
    harness.key_press(Key::Enter);
    harness.step();

    // Picked from the menu of descriptions, after typing another.
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    press(&mut harness, command(), Key::A);
    harness.get_by_label(field).type_text("x");
    harness.run_steps(2);
    harness.key_press(Key::Enter);
    harness.step();
    assert_eq!(viewer(&harness).description(), "x");
    harness.get_by_label("Descriptions").click();
    harness.run_steps(2);
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "spike")
        .click();
    harness.run_steps(2);
    assert_eq!(viewer(&harness).description(), "spike");
    let (a, b) = (at_time(&harness, 7.0, 40.0), at_time(&harness, 8.0, 40.0));
    drag(&mut harness, a, b);
    let got = made(&harness);
    assert_eq!(got.len(), 2);
    assert_eq!(got[1].2, "spike");
    assert_eq!(
        viewer(&harness)
            .legend()
            .entry(None, "spike")
            .unwrap()
            .count,
        2
    );

    // Out of annotation mode the field goes, the legend stays.
    press(&mut harness, command(), Key::E);
    assert!(harness.query_by_label(field).is_none());
    harness.get_by_label("Show spike");
}

/// The wgpu device on Metal, Vulkan or DX12 (traces need storage buffers in vertex shaders,
/// which OpenGL lacks), created once and shared by every window rendered on a GPU, as
/// `render`'s tests share theirs; `None` without such an adapter.
///
/// One instance and device per window, created and dropped by several tests at once,
/// crashed lavapipe (Mesa's software Vulkan driver, which CI uses on Linux) now and then
/// with a segfault that took the whole test binary down.
fn shared_gpu() -> Option<&'static egui_wgpu::WgpuSetupExisting> {
    use eframe::egui_wgpu::wgpu;
    static GPU: OnceLock<Option<egui_wgpu::WgpuSetupExisting>> = OnceLock::new();
    GPU.get_or_init(|| {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        desc.backends = wgpu::Backends::PRIMARY;
        let instance = wgpu::Instance::new(desc);
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        // The device eframe would ask for.
        let device_descriptor =
            egui_wgpu::WgpuSetupCreateNew::without_display_handle().device_descriptor;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&device_descriptor(&adapter))).ok()?;
        Some(egui_wgpu::WgpuSetupExisting {
            instance,
            adapter,
            device,
            queue,
        })
    })
    .as_ref()
}

/// kittest's wgpu setup on the shared GPU (see [`shared_gpu`]), or `None` without one: the
/// test then passes with a message, unless `KYMO_REQUIRE_GPU` is set.
fn gpu_setup() -> Option<egui_wgpu::WgpuSetup> {
    let Some(gpu) = shared_gpu() else {
        assert!(
            std::env::var_os("KYMO_REQUIRE_GPU").is_none_or(|v| v.is_empty()),
            "no GPU adapter, and KYMO_REQUIRE_GPU is set"
        );
        eprintln!("skipped: no GPU adapter");
        return None;
    };
    Some(egui_wgpu::WgpuSetup::Existing(gpu.clone()))
}

/// The window opening `path` on a GPU, at one pixel per point.
fn gpu_window(path: &Path, setup: egui_wgpu::WgpuSetup) -> Harness<'static, KymoApp> {
    let path = path.to_owned();
    Harness::builder()
        .with_size(SIZE)
        .with_context_setup(system_fonts)
        .wgpu_setup(setup)
        .build_eframe(move |cc| {
            let gpu = Gpu::new(cc.wgpu_render_state.as_ref());
            assert_eq!(gpu.error(), None);
            KymoApp::new(&cc.egui_ctx, gpu, options(), Some(path))
        })
}

/// Turns anti-aliasing off, so every trace pixel has its color exactly (anti-aliased edges
/// blend with the background and the annotations behind).
fn whole_pixels(harness: &mut Harness<'_, KymoApp>) {
    let settings = harness.state().settings().clone();
    harness.state_mut().set_settings(Settings {
        antialias: false,
        ..settings
    });
    harness.run_steps(2);
}

/// The trace area's pixels.
fn traces_image(harness: &mut Harness<'_, KymoApp>) -> Vec<[u8; 4]> {
    let traces = viewer(harness).layout().unwrap().traces;
    let image = harness.render().unwrap();
    let (x0, y0) = (traces.left() as u32, traces.top() as u32);
    let (x1, y1) = (traces.right() as u32, traces.bottom() as u32);
    (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .map(|(x, y)| image.get_pixel(x, y).0)
        .collect()
}

#[test]
fn anti_aliased_traces_blend_their_edges() {
    let Some(setup) = gpu_setup() else { return };
    let file = TempFile::preset("bdf+c-small");
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    // Distinct colors in the trace area: whole pixels give the background, the grid and
    // the traces' colors; anti-aliased edges give many shades between them.
    let colors = |harness: &mut Harness<'_, KymoApp>| {
        traces_image(harness)
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
    };
    let off = colors(&mut harness);
    let ctx = harness.ctx.clone();
    harness.state_mut().perform(&ctx, Action::ToggleAntialias);
    harness.run_steps(2);
    assert!(harness.state().settings().antialias);
    let on = colors(&mut harness);
    assert!(
        on >= off + 20,
        "{on} colors with anti-aliasing, {off} without"
    );
}

#[test]
fn opening_another_recording_never_draws_the_last_ones_tiles() {
    let Some(setup) = gpu_setup() else { return };
    // The same layout, so the same tile keys, with different values.
    let first = seeded("bdf+c-small", 1);
    let second = seeded("bdf+c-small", 2);
    // The traces alone: the annotations' labels come from egui's font atlas, which holds
    // whatever text the window showed before (status lines while loading), so their
    // pixels may differ a little between windows.
    let traces = |harness: &mut Harness<'_, KymoApp>| {
        click_in_list(harness, egui::accesskit::Role::CheckBox, "Show Annotations");
        traces_image(harness)
    };

    let mut fresh = gpu_window(second.path(), setup.clone());
    ready(&mut fresh);
    let want = traces(&mut fresh);

    let mut switched = gpu_window(first.path(), setup);
    ready(&mut switched);
    let before = traces(&mut switched);
    assert_ne!(before, want, "the two recordings look the same");
    let ctx = switched.ctx.clone();
    switched.state_mut().open(&ctx, second.path().to_owned());
    ready(&mut switched);
    let after = traces(&mut switched);
    let width = viewer(&switched).layout().unwrap().traces.width() as usize;
    assert!(
        after == want,
        "tiles of the first recording drawn: {}",
        differences(&after, &want, width)
    );
}

/// Where two images of `width` pixels per row differ, for messages.
fn differences(a: &[[u8; 4]], b: &[[u8; 4]], width: usize) -> String {
    let mut count = 0;
    let mut most = 0;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (i, (p, q)) in a.iter().zip(b).enumerate() {
        if p != q {
            count += 1;
            most = most.max((0..4).map(|c| p[c].abs_diff(q[c])).max().unwrap_or(0));
            let (x, y) = (i % width, i / width);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    format!(
        "{count} of {} pixels differ, by up to {most}, in x {x0}..={x1}, y {y0}..={y1}",
        a.len()
    )
}

#[test]
fn dc_removal_brings_an_offset_trace_into_its_row() {
    let Some(setup) = gpu_setup() else { return };
    let file = offset_preset("bdf+c-small", -21_000.0);
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    dc_off(&mut harness);
    // Without the annotations' labels, some of which are near the trace color.
    click_in_list(
        &mut harness,
        egui::accesskit::Role::CheckBox,
        "Show Annotations",
    );
    let trace = trace_color(&ChannelKind::EEG, &harness.ctx.global_style().visuals);
    // Pixels of the first row (A1) in the trace color.
    let a1_pixels = |harness: &mut Harness<'_, KymoApp>| {
        let traces = viewer(harness).layout().unwrap().traces;
        let rows = viewer(harness).view().rows();
        let width = traces.width() as usize;
        let row_height = traces.height() as usize / rows;
        traces_image(harness)[..width * row_height]
            .iter()
            .filter(|p| {
                let d = |a: u8, b: u8| i32::from(a).abs_diff(i32::from(b));
                d(p[0], trace.r()) + d(p[1], trace.g()) + d(p[2], trace.b()) < 60
            })
            .count()
    };
    // 21 mV below its row's middle, far outside it (a spike may cross it).
    let off = a1_pixels(&mut harness);
    harness.key_press_modifiers(command(), Key::D);
    harness.step();
    step_until(&mut harness, "the means", baselines_are_window_means);
    let on = a1_pixels(&mut harness);
    assert!(
        on > 500 && on > 20 * off,
        "{off} pixels without DC removal, {on} with"
    );
}

#[test]
fn the_gpu_budget_follows_the_settings() {
    let Some(setup) = gpu_setup() else { return };
    let file = TempFile::preset("bdf+c-small");
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    let gpu = |h: &Harness<'_, KymoApp>| h.state().gpu().budget();
    assert_eq!(gpu(&harness), Some(256 << 20));
    harness.key_press_modifiers(command(), Key::Comma);
    harness.step();
    pick_budget(&mut harness, "GPU Memory", "128 MB");
    assert_eq!(gpu(&harness), Some(128 << 20));
    // The traces stay drawn.
    assert!(viewer(&harness).complete());
}

#[test]
fn traces_take_their_types_colors() {
    let Some(setup) = gpu_setup() else { return };
    // TRIGGER, a flat EEG signal, is stim too, but without a trigger mask.
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.channels[1].label = "EOG L".into();
    spec.channels[2].label = "ECG".into();
    spec.channels[3].label = "TRIGGER".into();
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, file.path()).unwrap();
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    whole_pixels(&mut harness);
    // Without the annotations' bands, whose colors traces may share.
    harness.get_by_label("Show Annotations").click();
    harness.run_steps(2);
    let visuals = harness.ctx.global_style().visuals.clone();
    // The rows, top to bottom; the last is Status.
    let kinds = [
        kymo_core::ChannelKind::EEG,
        kymo_core::ChannelKind::EOG,
        kymo_core::ChannelKind::ECG,
        kymo_core::ChannelKind::STIM,
        kymo_core::ChannelKind::STIM,
    ];
    let colors: Vec<_> = kinds.iter().map(|k| trace_color(k, &visuals)).collect();
    // Per row, the pixels in each type's color.
    let counts = |harness: &mut Harness<'_, KymoApp>| {
        let traces = viewer(harness).layout().unwrap().traces;
        let width = traces.width() as usize;
        let row_height = traces.height() as usize / kinds.len();
        let image = traces_image(harness);
        (0..kinds.len())
            .map(|row| {
                let pixels = &image[row * row_height * width..(row + 1) * row_height * width];
                colors
                    .iter()
                    .map(|c| pixels.iter().filter(|p| near(p, *c)).count())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let before = counts(&mut harness);
    for (row, kind) in kinds.iter().enumerate() {
        let own = before[row][row];
        assert!(own > 200, "row {row} ({kind:?}): {:?}", before[row]);
        for (other, count) in before[row].iter().enumerate() {
            if colors[other] != colors[row] {
                assert!(*count < own / 20, "row {row} ({kind:?}): {:?}", before[row]);
            }
        }
    }

    // A bad channel is grayed out, whatever its type.
    mark_in_list(&mut harness, "ECG", true);
    let after = counts(&mut harness);
    assert!(after[2][2] < before[2][2] / 20, "{:?}", after[2]);
}

/// The title bar's themes the last frame asked for.
fn title_bar_themes(harness: &Harness<'_, KymoApp>) -> Vec<egui::SystemTheme> {
    harness
        .output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|viewport| {
            viewport
                .commands
                .iter()
                .filter_map(|c| match c {
                    egui::ViewportCommand::SetTheme(theme) => Some(*theme),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_theme_follows_the_setting_and_the_system() {
    use egui::accesskit::Role;
    use egui::{SystemTheme, Theme as Look};
    let file = without_triggers("edf+d-mixed");
    // Tall enough for legend rows below the Settings window (about 790 points with the
    // channel types' table).
    let size = egui::vec2(SIZE.x, 1100.0);
    let mut harness = screenshot_window_sized(file.path(), size);
    harness.input_mut().system_theme = Some(Look::Light);
    ready(&mut harness);
    // What the window shows: egui's theme, the trace area's background (its most common
    // color) and a legend swatch, against the colors of the theme egui uses now.
    let shown = |harness: &mut Harness<'_, KymoApp>| {
        let image = harness.render().unwrap();
        let pixel = |x: f32, y: f32| {
            egui::Color32::from_rgba_premultiplied(
                image.get_pixel(x as u32, y as u32)[0],
                image.get_pixel(x as u32, y as u32)[1],
                image.get_pixel(x as u32, y as u32)[2],
                255,
            )
        };
        let traces = viewer(harness).layout().unwrap().traces;
        let mut counts = std::collections::HashMap::new();
        for y in (traces.top() as u32 + 1..traces.bottom() as u32 - 1).step_by(3) {
            for x in (traces.left() as u32 + 1..traces.right() as u32 - 1).step_by(3) {
                *counts.entry(pixel(x as f32, y as f32)).or_insert(0) += 1;
            }
        }
        let background = counts.into_iter().max_by_key(|&(_, n)| n).unwrap().0;
        // A legend row clear of the Settings window and its shadow (below it), and of the
        // fade at the legend's bottom edge.
        let settings_bottom = harness.query_by_label("Close window").map_or(0.0, |_| {
            harness.get_by_label("Restore Defaults").rect().bottom() + 40.0
        });
        let descriptions: Vec<String> = viewer(harness)
            .legend()
            .entries()
            .map(|e| e.description.clone())
            .collect();
        let (description, swatch) = descriptions
            .iter()
            .filter_map(|d| {
                let rect = harness.query_by_label(&format!("Color of {d}"))?.rect();
                (rect.top() > settings_bottom && rect.bottom() < size.y - 60.0)
                    .then(|| (d.clone(), rect.center()))
            })
            .next()
            .expect("a legend swatch below the Settings window");
        let color = viewer(harness)
            .legend()
            .entry(None, &description)
            .unwrap()
            .color;
        let visuals = harness.ctx.global_style().visuals.clone();
        assert_eq!(background, visuals.extreme_bg_color, "trace area");
        // Within rounding: the swatch's variants differ by far more.
        let swatch = pixel(swatch.x, swatch.y);
        let want = annotation_color(color, &visuals);
        assert!(
            near(&swatch.to_array(), want),
            "swatch {swatch:?}, not {want:?}"
        );
        assert_eq!(visuals.dark_mode, harness.ctx.theme() == Look::Dark);
        harness.ctx.theme()
    };
    // Kymo's own colors differ between the themes.
    let [light, dark] = [egui::Visuals::light(), egui::Visuals::dark()];
    assert_ne!(light.extreme_bg_color, dark.extreme_bg_color);
    let palette0 = AnnotationColor::Palette(0);
    assert_ne!(
        annotation_color(palette0, &light),
        annotation_color(palette0, &dark)
    );

    // System: the system's theme as it changes, the title bar left to the system.
    assert_eq!(harness.state().settings().theme, Theme::System);
    assert_eq!(shown(&mut harness), Look::Light);
    harness.input_mut().system_theme = Some(Look::Dark);
    harness.step();
    assert_eq!(title_bar_themes(&harness), [SystemTheme::SystemDefault]);
    harness.run_steps(2);
    assert_eq!(shown(&mut harness), Look::Dark);

    // Light or Dark from Settings, whatever the system's theme; the title bar is set again
    // when the system's changes, as Windows puts it back to the system's mode then.
    harness.key_press_modifiers(command(), Key::Comma);
    harness.run_steps(2);
    for (theme, look, title_bar) in [
        (Theme::Light, Look::Light, SystemTheme::Light),
        (Theme::Dark, Look::Dark, SystemTheme::Dark),
    ] {
        harness
            .get_by_role_and_label(Role::RadioButton, theme.label())
            .click();
        harness.step();
        assert_eq!(harness.state().settings().theme, theme);
        assert_eq!(title_bar_themes(&harness), [title_bar], "{theme:?}");
        harness.run_steps(2);
        assert_eq!(title_bar_themes(&harness), [], "{theme:?} again");
        assert_eq!(shown(&mut harness), look, "{theme:?}");
        for system in [Look::Light, Look::Dark] {
            harness.input_mut().system_theme = Some(system);
            harness.step();
            harness.step();
            assert_eq!(
                shown(&mut harness),
                look,
                "{theme:?} on a {system:?} system"
            );
        }
        harness.input_mut().system_theme = Some(Look::Light);
        harness.step();
        assert_eq!(
            title_bar_themes(&harness),
            [title_bar],
            "{theme:?}, system changed"
        );
        harness.run_steps(2);
    }

    // Kept for the next run, which starts in it.
    let mut storage = Memory::default();
    eframe::App::save(harness.state_mut(), &mut storage);
    let settings = Settings::load(Some(&storage));
    assert_eq!(settings.theme, Theme::Dark);
    let path = file.path().to_owned();
    let mut next = Harness::builder().with_size(SIZE).build_eframe(move |cc| {
        KymoApp::new(
            &cc.egui_ctx,
            Gpu::none("no GPU in this test"),
            Options {
                settings,
                ..options()
            },
            Some(path),
        )
    });
    next.input_mut().system_theme = Some(Look::Light);
    next.step();
    assert_eq!(next.ctx.theme(), Look::Dark);
    assert_eq!(title_bar_themes(&next), [SystemTheme::Dark]);

    // Back to System: the system's theme again, and the title bar the system's.
    harness
        .get_by_role_and_label(Role::RadioButton, "System")
        .click();
    harness.step();
    assert_eq!(title_bar_themes(&harness), [SystemTheme::SystemDefault]);
    harness.run_steps(2);
    assert_eq!(shown(&mut harness), Look::Light);
}

#[test]
fn system_follows_the_app_mode_where_it_is_read() {
    // Windows: System follows the default app mode as Kymo reads it, not the system's
    // theme as winit reports it (which a forced theme skews there).
    use egui::{SystemTheme, Theme as Look};
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    let mode = AppMode::new();
    mode.set(Some(false));
    harness.state_mut().follow_app_mode(mode.clone());
    harness.input_mut().system_theme = Some(Look::Dark);
    ready(&mut harness);
    assert_eq!(harness.ctx.theme(), Look::Light);

    // The app mode changes: all of Kymo follows, the title bar set as such.
    mode.set(Some(true));
    harness.step();
    assert_eq!(harness.ctx.theme(), Look::Dark);
    assert_eq!(title_bar_themes(&harness), [SystemTheme::Dark]);
    // winit's report changes alone: the app mode still rules, the title bar set again.
    harness.input_mut().system_theme = Some(Look::Light);
    harness.step();
    assert_eq!(harness.ctx.theme(), Look::Dark);
    assert_eq!(title_bar_themes(&harness), [SystemTheme::Dark]);

    // Light, whatever the app mode.
    let set = |harness: &mut Harness<'_, KymoApp>, theme| {
        let settings = harness.state().settings().clone();
        harness
            .state_mut()
            .set_settings(Settings { theme, ..settings });
        harness.step();
    };
    set(&mut harness, Theme::Light);
    assert_eq!(harness.ctx.theme(), Look::Light);
    assert_eq!(title_bar_themes(&harness), [SystemTheme::Light]);
    mode.set(Some(false));
    harness.step();
    mode.set(Some(true));
    harness.step();
    assert_eq!(harness.ctx.theme(), Look::Light);

    // Back to System: the app mode's, whatever winit reported meanwhile.
    set(&mut harness, Theme::System);
    assert_eq!(harness.ctx.theme(), Look::Dark);
    assert_eq!(title_bar_themes(&harness), [SystemTheme::Dark]);

    // An app mode that can't be read: the system's theme as winit reports it.
    mode.set(None);
    harness.step();
    assert_eq!(harness.ctx.theme(), Look::Light);
    assert_eq!(title_bar_themes(&harness), [SystemTheme::SystemDefault]);
}

#[test]
fn the_theme_is_in_force_before_the_first_frame() {
    // The startup file dialog opens while the second frame is built and leaves the first
    // on screen, so that has to be in the theme already (egui's default is dark).
    use egui::Theme as Look;
    let app = |theme| {
        let options = Options {
            settings: Settings {
                theme,
                ..Settings::default()
            },
            ..options()
        };
        let ctx = egui::Context::default();
        let app = KymoApp::new(&ctx, Gpu::none("no GPU in this test"), options, None);
        (ctx, app)
    };
    let (ctx, mut forced) = app(Theme::Light);
    assert_eq!(ctx.theme(), Look::Dark);
    forced.start(&ctx);
    assert_eq!(ctx.theme(), Look::Light);

    // System with an app mode that is read (Linux, Windows): its mode, light here.
    let (ctx, mut system) = app(Theme::System);
    let mode = AppMode::new();
    mode.set(Some(false));
    system.follow_app_mode(mode);
    system.start(&ctx);
    assert_eq!(ctx.theme(), Look::Light);
}

#[test]
fn traces_take_the_themes_colors() {
    let Some(setup) = gpu_setup() else { return };
    // Rows A1, A2 (EEG), "EOG L", ECG, Status.
    let file = typed_preset();
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    whole_pixels(&mut harness);
    let [light, dark] = [egui::Visuals::light(), egui::Visuals::dark()];
    // Pixels of the EOG row near `color`.
    let count = |harness: &mut Harness<'_, KymoApp>, color: egui::Color32| {
        let traces = viewer(harness).layout().unwrap().traces;
        let width = traces.width() as usize;
        let row_height = traces.height() as usize / 5;
        traces_image(harness)[2 * row_height * width..3 * row_height * width]
            .iter()
            .filter(|p| near(p, color))
            .count()
    };
    for (theme, own, other) in [(Theme::Light, &light, &dark), (Theme::Dark, &dark, &light)] {
        let settings = harness.state().settings().clone();
        harness
            .state_mut()
            .set_settings(Settings { theme, ..settings });
        harness.run_steps(2);
        let ours = count(&mut harness, trace_color(&ChannelKind::EOG, own));
        let theirs = count(&mut harness, trace_color(&ChannelKind::EOG, other));
        assert!(
            ours > 200 && theirs < ours / 20,
            "{theme:?}: {ours} and {theirs}"
        );
    }
}

#[test]
fn traces_take_picked_type_colors() {
    let Some(setup) = gpu_setup() else { return };
    // Rows A1, A2 (EEG), "EOG L", ECG, Status.
    let file = typed_preset();
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    whole_pixels(&mut harness);
    let green = egui::Color32::from_rgb(10, 200, 30);
    let visuals = harness.ctx.global_style().visuals.clone();
    let blue = trace_color(&ChannelKind::EOG, &visuals);
    // Pixels of the EOG row near `color`.
    let count = |harness: &mut Harness<'_, KymoApp>, color: egui::Color32| {
        let traces = viewer(harness).layout().unwrap().traces;
        let width = traces.width() as usize;
        let row_height = traces.height() as usize / 5;
        let image = traces_image(harness);
        image[2 * row_height * width..3 * row_height * width]
            .iter()
            .filter(|p| near(p, color))
            .count()
    };
    // Green pixels of the sidebar's EOG type row and channel name.
    let sidebar = |harness: &mut Harness<'_, KymoApp>| {
        let rects = [
            kind_name_rect(harness, "EOG"),
            harness.get_by_label("EOG L").rect(),
        ];
        let image = harness.render().unwrap();
        rects.map(|r| {
            (r.left() as u32..r.right() as u32)
                .flat_map(|x| (r.top() as u32..r.bottom() as u32).map(move |y| (x, y)))
                .filter(|&(x, y)| near(&image.get_pixel(x, y).0, green))
                .count()
        })
    };
    let before = (count(&mut harness, blue), count(&mut harness, green));
    assert!(before.0 > 200 && before.1 == 0, "{before:?}");
    assert_eq!(sidebar(&mut harness), [0, 0]);

    pick(&mut harness, "Color of EOG channels", [10, 200, 30]);
    close_popup(&mut harness);
    let after = (count(&mut harness, blue), count(&mut harness, green));
    assert!(
        after.0 < before.0 / 20 && after.1 > before.0 / 2,
        "{after:?}"
    );
    let named = sidebar(&mut harness);
    assert!(named.iter().all(|&n| n > 5), "{named:?}");

    // A bad channel stays gray.
    mark_in_list(&mut harness, "EOG L", true);
    let bad = count(&mut harness, green);
    assert!(bad < after.1 / 20, "{bad}");
}

#[test]
fn stim_traces_stay_in_their_row() {
    let Some(setup) = gpu_setup() else { return };
    // Status, the last row, holds codes up to 255 under status bits around 2^20; at a stim
    // scale of 1 per row, either would reach far beyond the row.
    let file = TempFile::preset("bdf+c-small");
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    dc_off(&mut harness);
    whole_pixels(&mut harness);
    let visuals = harness.ctx.global_style().visuals.clone();
    let gray = trace_color(&kymo_core::ChannelKind::STIM, &visuals);
    let text = trace_color(&kymo_core::ChannelKind::EEG, &visuals);
    // Per row, the pixels in the stim color and in EEG's gray; and the pixel line of
    // the last row with the most stim-colored pixels, from the row's top.
    let counts = |harness: &mut Harness<'_, KymoApp>| {
        let traces = viewer(harness).layout().unwrap().traces;
        let rows = viewer(harness).view().rows();
        assert_eq!(rows, 5);
        let width = traces.width() as usize;
        let row_height = traces.height() as usize / rows;
        let image = traces_image(harness);
        let counts: Vec<(usize, usize)> = (0..rows)
            .map(|row| {
                let pixels = &image[row * row_height * width..(row + 1) * row_height * width];
                let count = |c| pixels.iter().filter(|p| near(p, c)).count();
                (count(gray), count(text))
            })
            .collect();
        let lines = image[4 * row_height * width..5 * row_height * width].chunks_exact(width);
        let busiest = lines
            .map(|line| line.iter().filter(|p| near(p, gray)).count())
            .enumerate()
            .max_by_key(|&(_, n)| n)
            .unwrap()
            .0;
        (counts, busiest as f32 / row_height as f32)
    };
    let check = |(counts, busiest): &(Vec<(usize, usize)>, f32)| {
        let (status, _) = counts[4];
        assert!(status > 500, "{counts:?}");
        for &(gray, text) in &counts[..4] {
            assert!(gray < status / 50, "{counts:?}");
            assert!(text > 200, "{counts:?}");
        }
        // Mostly code 0, in the middle of the row.
        assert!(
            (busiest - 0.5).abs() < 0.05,
            "busiest line at {busiest} of the row"
        );
    };
    check(&counts(&mut harness));
    harness.key_press_modifiers(command(), Key::D);
    harness.step();
    step_until(&mut harness, "the means", baselines_are_window_means);
    check(&counts(&mut harness));
}

/// Bands tint the background behind the traces, which keep their color; event lines are
/// solid.
#[test]
fn annotations_lie_behind_the_traces() {
    let Some(setup) = gpu_setup() else { return };
    // In the first 20 s: "trigger 0" at 0.5 s (color 0), "trigger 1" from 5.5 s to 6.5 s
    // (color 1), and so on.
    let file = TempFile::preset("bdf+c-small");
    let mut harness = gpu_window(file.path(), setup);
    ready(&mut harness);
    let visuals = harness.ctx.global_style().visuals.clone();
    let marks = viewer(&harness).marks().clone();
    let traces = viewer(&harness).layout().unwrap().traces;
    let (width, height) = (traces.width() as usize, traces.height() as usize);
    let image = traces_image(&mut harness);
    let column =
        |c: u32| -> Vec<[u8; 4]> { (0..height).map(|y| image[y * width + c as usize]).collect() };
    let mode = |pixels: &[[u8; 4]]| -> [u8; 4] {
        let mut counts = std::collections::HashMap::new();
        for p in pixels {
            *counts.entry(*p).or_insert(0) += 1;
        }
        counts.into_iter().max_by_key(|&(_, n)| n).unwrap().0
    };
    let distance = |p: [u8; 4], c: egui::Color32| {
        let d = |a: u8, b: u8| i32::from(a).abs_diff(i32::from(b));
        d(p[0], c.r()) + d(p[1], c.g()) + d(p[2], c.b())
    };

    let band = marks
        .bands
        .iter()
        .find(|b| b.color == AnnotationColor::Palette(1))
        .unwrap()
        .clone();
    assert!(band.columns.len() > 10, "{band:?}");
    let marked = |c: u32| {
        marks
            .bands
            .iter()
            .chain(&marks.lines)
            .any(|m| m.columns.start <= c + 2 && c < m.columns.end + 2)
    };
    let plain = (0..width as u32).find(|&c| !marked(c)).unwrap();
    let background = mode(&column(plain));
    let tinted = mode(&column((band.columns.start + band.columns.end) / 2));
    let band_color = annotation_color(AnnotationColor::Palette(1), &visuals);
    assert!(
        tinted != background && distance(tinted, band_color) < distance(background, band_color),
        "band {tinted:?}, background {background:?}"
    );
    // The traces cross the band in their own color.
    let trace = trace_color(&kymo_core::ChannelKind::EEG, &visuals);
    let on_top = band
        .columns
        .clone()
        .flat_map(column)
        .filter(|&p| distance(p, trace) < 12)
        .count();
    assert!(on_top > 20, "{on_top} trace pixels in the band");

    let line = marks
        .lines
        .iter()
        .find(|l| l.color == AnnotationColor::Palette(0))
        .unwrap();
    let solid = mode(&column(line.columns.start));
    assert!(
        distance(
            solid,
            annotation_color(AnnotationColor::Palette(0), &visuals)
        ) < 12,
        "line {solid:?}"
    );
}

/// Whether a pixel is within a small distance of `color`./// Whether a pixel is within a small distance of `color`.
fn near(pixel: &[u8; 4], color: egui::Color32) -> bool {
    let d = |a: u8, b: u8| i32::from(a).abs_diff(i32::from(b));
    d(pixel[0], color.r()) + d(pixel[1], color.g()) + d(pixel[2], color.b()) < 40
}

/// Renders the window on a GPU and writes it to `KYMO_DUMP` (a directory) if set, showing
/// `KYMO_UI_FILE` or a generated recording.
#[test]
fn renders_the_window() {
    if std::env::var_os("KYMO_DUMP").is_none() {
        return;
    }
    let generated = typed_preset();
    let path = std::env::var_os("KYMO_UI_FILE").map_or(generated.path().to_owned(), PathBuf::from);
    let mut harness = Harness::builder()
        .with_size(SIZE)
        .with_pixels_per_point(2.0)
        .with_context_setup(system_fonts)
        .wgpu()
        .build_eframe(|cc| {
            KymoApp::new(
                &cc.egui_ctx,
                Gpu::new(cc.wgpu_render_state.as_ref()),
                options(),
                Some(path.clone()),
            )
        });
    ready(&mut harness);
    for _ in 0..20 {
        harness.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    harness.hover_at(egui::pos2(800.0, 40.0));
    for _ in 0..3 {
        harness.step();
    }
    let image = harness.render().unwrap();
    eprintln!("status: {:?}", harness.state().viewer().unwrap().status());
    let dir = PathBuf::from(std::env::var_os("KYMO_DUMP").unwrap());
    let mut ppm = format!("P6 {} {} 255\n", image.width(), image.height()).into_bytes();
    ppm.extend(image.pixels().flat_map(|p| [p[0], p[1], p[2]]));
    std::fs::write(dir.join("window.ppm"), ppm).unwrap();
}

/// The selected annotation's onset and end.
fn selected_span(harness: &Harness<'_, KymoApp>) -> (f64, f64) {
    let d = selected_data(harness).expect("a selection");
    (d.onset, d.end())
}

#[track_caller]
fn assert_span(got: (f64, f64), want: (f64, f64)) {
    assert!(
        (got.0 - want.0).abs() < 1e-3 && (got.1 - want.1).abs() < 1e-3,
        "{got:?} != {want:?}"
    );
}

fn cursor(harness: &Harness<'_, KymoApp>) -> egui::CursorIcon {
    harness.output().platform_output.cursor_icon
}

#[test]
fn dragging_the_selected_annotation_moves_and_resizes_it() {
    let file = without_triggers("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    let at = |harness: &Harness<'_, KymoApp>, t| at_time(harness, t, 40.0);
    let (a, b) = (at(&harness, 2.0), at(&harness, 6.0));
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (2.0, 6.0));
    // Inside, it moves; at an edge, or just outside, the edge moves.
    let (a, b) = (at(&harness, 4.0), at(&harness, 5.0));
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (3.0, 7.0));
    let (a, b) = (at(&harness, 7.0), at(&harness, 8.0));
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (3.0, 8.0));
    // The edge keeps its distance to the pointer.
    let outside = egui::vec2(3.0, 0.0);
    let (a, b) = (at(&harness, 3.0) - outside, at(&harness, 2.5) - outside);
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (2.5, 8.0));
    // Past the other edge, they swap.
    let (a, b) = (at(&harness, 8.0), at(&harness, 1.0));
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (1.0, 2.5));
    assert_eq!(made(&harness).len(), 1);
    // Each drag is one step.
    for want in [(2.5, 8.0), (3.0, 8.0), (3.0, 7.0), (2.0, 6.0)] {
        press(&mut harness, command(), Key::Z);
        assert_span(selected_span(&harness), want);
    }
    press(&mut harness, Modifiers::SHIFT | command(), Key::Z);
    assert_span(selected_span(&harness), (3.0, 7.0));
    // Moving stops at the start, keeping the duration.
    let (a, b) = (at(&harness, 5.0), at(&harness, 0.0) - egui::vec2(60.0, 0.0));
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (0.0, 4.0));
    // So does an edge grabbed from inside, which keeps its distance to the pointer.
    let inside = egui::vec2(3.0, 0.0);
    let (a, b) = (
        at(&harness, 0.0) + inside,
        at(&harness, 0.0) - egui::vec2(60.0, 0.0),
    );
    drag(&mut harness, a, b);
    assert_eq!(selected_span(&harness).0, 0.0);
    // And at the end.
    press(&mut harness, Modifiers::NONE, Key::End);
    let (a, b) = (at(&harness, 50.0), at(&harness, 55.0));
    drag(&mut harness, a, b);
    let (a, b) = (
        at(&harness, 52.0),
        at(&harness, 60.0) + egui::vec2(60.0, 0.0),
    );
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (55.0, 60.0));
    // A resize stops there too.
    let (a, b) = (
        at(&harness, 55.0),
        at(&harness, 60.0) + egui::vec2(60.0, 0.0),
    );
    drag(&mut harness, a, b);
    assert_span(selected_span(&harness), (60.0, 60.0));
    press(&mut harness, command(), Key::Z);
    assert_eq!(made(&harness).len(), 2);

    // The pointer shows what a drag would do.
    for (t, icon) in [
        (57.5, egui::CursorIcon::Grab),
        (55.0, egui::CursorIcon::ResizeHorizontal),
        (60.0, egui::CursorIcon::ResizeHorizontal),
        (45.0, egui::CursorIcon::Crosshair),
    ] {
        let p = at(&harness, t) - egui::vec2(if t == 60.0 { 1.0 } else { 0.0 }, 0.0);
        hover(&mut harness, p);
        harness.step();
        assert_eq!(cursor(&harness), icon, "{t}");
    }
    let p = at(&harness, 57.5);
    harness.drag_at(p);
    harness.step();
    hover(&mut harness, p + egui::vec2(20.0, 0.0));
    assert_eq!(cursor(&harness), egui::CursorIcon::Grabbing);
    harness.drop_at(p + egui::vec2(20.0, 0.0));
    harness.step();
}

#[test]
fn a_drag_taken_back_or_cancelled_changes_nothing() {
    let file = without_triggers("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    let at = |harness: &Harness<'_, KymoApp>, t| at_time(harness, t, 40.0);
    let (a, b) = (at(&harness, 2.0), at(&harness, 6.0));
    drag(&mut harness, a, b);
    // Out and back: it moves while dragged, and ends where it was, with no step.
    let p = at(&harness, 4.0);
    hover(&mut harness, p);
    harness.drag_at(p);
    harness.step();
    let q = at(&harness, 5.0);
    hover(&mut harness, q);
    assert_span(selected_span(&harness), (3.0, 7.0));
    hover(&mut harness, p + egui::vec2(1.0, 0.0));
    harness.drop_at(p + egui::vec2(1.0, 0.0));
    harness.step();
    assert_span(selected_span(&harness), (2.0, 6.0));
    // Undo takes back the create, so the drag recorded nothing.
    press(&mut harness, command(), Key::Z);
    assert!(made(&harness).is_empty());
    press(&mut harness, Modifiers::SHIFT | command(), Key::Z);

    // Esc puts it back and ends the drag, keeping the selection; again, it deselects.
    harness.drag_at(p);
    harness.step();
    let q = at(&harness, 5.0);
    hover(&mut harness, q);
    // Undo and Delete wait for the drag to end.
    press(&mut harness, command(), Key::Z);
    press(&mut harness, Modifiers::NONE, Key::Backspace);
    assert_span(selected_span(&harness), (3.0, 7.0));
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_span(selected_span(&harness), (2.0, 6.0));
    let q = at(&harness, 6.0);
    hover(&mut harness, q);
    harness.drop_at(at(&harness, 6.0));
    harness.step();
    assert_span(selected_span(&harness), (2.0, 6.0));
    press(&mut harness, command(), Key::Z);
    assert!(made(&harness).is_empty());
    press(&mut harness, Modifiers::SHIFT | command(), Key::Z);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(viewer(&harness).selected(), None);

    // Leaving annotation mode during a drag keeps it, as one step.
    click_at_time(&mut harness, 4.0);
    let p = at(&harness, 4.0);
    hover(&mut harness, p);
    harness.drag_at(p);
    harness.step();
    let q = at(&harness, 5.0);
    hover(&mut harness, q);
    press(&mut harness, command(), Key::E);
    harness.drop_at(q);
    harness.step();
    assert_span(
        (
            made(&harness)[0].0,
            made(&harness)[0].0 + made(&harness)[0].1,
        ),
        (3.0, 7.0),
    );
    press(&mut harness, command(), Key::Z);
    assert_span(
        (
            made(&harness)[0].0,
            made(&harness)[0].0 + made(&harness)[0].1,
        ),
        (2.0, 6.0),
    );
    press(&mut harness, command(), Key::E);

    // Esc takes back a create as well.
    let p = at(&harness, 10.0);
    hover(&mut harness, p);
    harness.drag_at(p);
    harness.step();
    let q = at(&harness, 12.0);
    hover(&mut harness, q);
    assert_eq!(made(&harness).len(), 2);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(made(&harness).len(), 1);
    let q = at(&harness, 13.0);
    hover(&mut harness, q);
    harness.drop_at(at(&harness, 13.0));
    harness.step();
    assert_eq!(made(&harness).len(), 1);

    // An annotation that isn't selected isn't grabbed: a drag over it creates one.
    describe(&mut harness, "x");
    let (a, b) = (at(&harness, 3.0), at(&harness, 4.0));
    drag(&mut harness, a, b);
    let got = made(&harness);
    assert_eq!(got.len(), 2);
    assert_span((got[0].0, got[0].0 + got[0].1), (2.0, 6.0));
    assert_eq!(got[1].2, "x");
}

/// The viewer's and the file's annotations of `description`, as onset and end, by onset.
fn spans(harness: &Harness<'_, KymoApp>, description: &str) -> Vec<(f64, f64, kymo_core::Origin)> {
    viewer(harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.description == description)
        .map(|a| (a.data.onset, a.data.end(), a.data.origin))
        .collect()
}

#[track_caller]
fn assert_spans(got: Vec<(f64, f64, kymo_core::Origin)>, want: &[(f64, f64)]) {
    assert_eq!(got.len(), want.len(), "{got:?} != {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert_span((g.0, g.1), *w);
    }
}

#[test]
fn edits_merge_annotations_of_a_kind_they_reach() {
    use kymo_core::Origin::File;
    // BAD from the file, overlapping, from 22 to 24 s and from 23 to 25 s.
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.channels.retain(|c| !c.trigger);
    spec.annotations = [(22.0, "BAD"), (23.0, "BAD"), (30.0, "x")]
        .map(|(onset, text)| kymo_testgen::Tal {
            onset,
            duration: Some(2.0),
            texts: vec![text.into()],
        })
        .into();
    let file = TempFile::written(&spec);
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    let at = |harness: &Harness<'_, KymoApp>, t| at_time(harness, t, 40.0);
    let create = |harness: &mut Harness<'_, KymoApp>, description: &str, from, to| {
        describe(harness, description);
        press(harness, Modifiers::NONE, Key::Escape);
        let (a, b) = (at(harness, from), at(harness, to));
        drag(harness, a, b);
    };
    create(&mut harness, "BAD", 2.0, 4.0);
    create(&mut harness, "BAD", 6.0, 8.0);
    create(&mut harness, "BAD", 10.0, 11.0);
    create(&mut harness, "x", 4.5, 5.5);
    assert_spans(
        spans(&harness, "BAD"),
        &[
            (2.0, 4.0),
            (6.0, 8.0),
            (10.0, 11.0),
            (22.0, 24.0),
            (23.0, 25.0),
        ],
    );
    // A new one reaching two of them becomes one with them; "x" stays apart.
    create(&mut harness, "BAD", 3.0, 7.0);
    let merged = [(2.0, 8.0), (10.0, 11.0), (22.0, 24.0), (23.0, 25.0)];
    assert_spans(spans(&harness, "BAD"), &merged);
    assert_span(selected_span(&harness), (2.0, 8.0));
    assert_spans(spans(&harness, "x"), &[(4.5, 5.5), (30.0, 32.0)]);
    // One step: undo brings all three back.
    press(&mut harness, command(), Key::Z);
    assert_spans(
        spans(&harness, "BAD"),
        &[
            (2.0, 4.0),
            (6.0, 8.0),
            (10.0, 11.0),
            (22.0, 24.0),
            (23.0, 25.0),
        ],
    );
    press(&mut harness, Modifiers::SHIFT | command(), Key::Z);
    assert_spans(spans(&harness, "BAD"), &merged);
    assert_span(selected_span(&harness), (2.0, 8.0));
    // A resize that reaches one merges as well.
    let (a, b) = (at(&harness, 8.0), at(&harness, 10.5));
    drag(&mut harness, a, b);
    assert_spans(
        spans(&harness, "BAD"),
        &[(2.0, 11.0), (22.0, 24.0), (23.0, 25.0)],
    );
    press(&mut harness, command(), Key::Z);
    assert_spans(spans(&harness, "BAD"), &merged);

    // The file's overlapping ones stay apart until one of them is edited: moved, it
    // merges with the other, keeping its origin.
    press(&mut harness, Modifiers::NONE, Key::Escape);
    press(&mut harness, Modifiers::ALT, Key::ArrowRight);
    assert_eq!(viewer(&harness).view().time(), 20.0..40.0);
    click_at_time(&mut harness, 22.5);
    assert_span(selected_span(&harness), (22.0, 24.0));
    // Dragged out and back, it is unchanged and merges nothing.
    let p = at(&harness, 22.5);
    harness.drag_at(p);
    harness.step();
    let q = at(&harness, 23.5);
    hover(&mut harness, q);
    hover(&mut harness, p);
    harness.drop_at(p);
    harness.step();
    assert_eq!(spans(&harness, "BAD").len(), 4);
    let (a, b) = (at(&harness, 22.5), at(&harness, 22.0));
    drag(&mut harness, a, b);
    let got = spans(&harness, "BAD");
    assert_spans(got.clone(), &[(2.0, 8.0), (10.0, 11.0), (21.5, 25.0)]);
    assert_eq!(got[2].2, File);
    assert_span(selected_span(&harness), (21.5, 25.0));
}

#[test]
fn merge_annotations_merges_the_shown_ones_in_one_step() {
    use kymo_core::Origin::File;
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.channels.retain(|c| !c.trigger);
    spec.annotations = [
        // A chain of three BAD, and one apart.
        (22.0, 2.0, "BAD"),
        (23.0, 2.0, "BAD"),
        (24.5, 1.5, "BAD"),
        (30.0, 1.0, "BAD"),
        // Two "x" that overlap.
        (22.0, 2.0, "x"),
        (23.5, 1.5, "x"),
    ]
    .map(|(onset, duration, text)| kymo_testgen::Tal {
        onset,
        duration: Some(duration),
        texts: vec![text.into()],
    })
    .into();
    let file = TempFile::written(&spec);
    let mut harness = window(file.path());
    ready(&mut harness);
    let bad = [(22.0, 24.0), (23.0, 25.0), (24.5, 26.0), (30.0, 31.0)];
    let x = [(22.0, 24.0), (23.5, 25.0)];

    // Enabled while some shown description has something to merge.
    let merge = "Merge Annotations";
    assert!(enabled(&mut harness, "Edit", merge));
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show BAD");
    assert!(enabled(&mut harness, "Edit", merge));
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show x");
    assert!(!enabled(&mut harness, "Edit", merge));
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show BAD");
    assert!(enabled(&mut harness, "Edit", merge));
    // Also through the legend's checkbox for all: mixed, it shows all, then hides all.
    for shown in [true, false] {
        click_in_list(
            &mut harness,
            egui::accesskit::Role::CheckBox,
            "Show Annotations",
        );
        assert_eq!(enabled(&mut harness, "Edit", merge), shown);
    }
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show BAD");

    // With "x" hidden, only BAD merges: the chain becomes one, from the file still.
    choose(&mut harness, "Edit", merge);
    let merged = [(22.0, 26.0), (30.0, 31.0)];
    let got = spans(&harness, "BAD");
    assert_spans(got.clone(), &merged);
    assert!(got.iter().all(|s| s.2 == File));
    assert_spans(spans(&harness, "x"), &x);
    let count =
        |harness: &Harness<'_, KymoApp>, d| viewer(harness).legend().entry(None, d).unwrap().count;
    assert_eq!(count(&harness, "BAD"), 2);
    assert!(harness.state().title().contains("(edited)"));
    assert!(!enabled(&mut harness, "Edit", merge));

    // One step.
    press(&mut harness, command(), Key::Z);
    assert_spans(spans(&harness, "BAD"), &bad);
    assert!(!harness.state().title().contains("(edited)"));
    press(&mut harness, Modifiers::SHIFT | command(), Key::Z);
    assert_spans(spans(&harness, "BAD"), &merged);
    press(&mut harness, command(), Key::Z);

    // A selected annotation merged into another leaves that one selected.
    press(&mut harness, command(), Key::E);
    press(&mut harness, Modifiers::ALT, Key::ArrowRight);
    assert_eq!(viewer(&harness).view().time(), 20.0..40.0);
    click_at_time(&mut harness, 25.5);
    assert_span(selected_span(&harness), (24.5, 26.0));
    choose(&mut harness, "Edit", merge);
    assert_spans(spans(&harness, "BAD"), &merged);
    assert_span(selected_span(&harness), (22.0, 26.0));
    // Shown again, "x" merges too.
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show x");
    choose(&mut harness, "Edit", merge);
    assert_spans(spans(&harness, "x"), &[(22.0, 25.0)]);
    assert_span(selected_span(&harness), (22.0, 26.0));

    // It waits for a drag to end, even one that reaches another: moved over the BAD at
    // 30 s, the selected one merges only on release, as any edit does.
    let at = |harness: &Harness<'_, KymoApp>, t| at_time(harness, t, 40.0);
    let p = at(&harness, 24.0);
    hover(&mut harness, p);
    harness.drag_at(p);
    harness.step();
    let q = at(&harness, 28.5);
    hover(&mut harness, q);
    assert_spans(spans(&harness, "BAD"), &[(26.5, 30.5), (30.0, 31.0)]);
    let ctx = harness.ctx.clone();
    harness
        .state_mut()
        .perform(&ctx, kymo::action::Action::MergeAnnotations);
    assert_spans(spans(&harness, "BAD"), &[(26.5, 30.5), (30.0, 31.0)]);
    harness.drop_at(q);
    harness.step();
    assert_spans(spans(&harness, "BAD"), &[(26.5, 31.0)]);
}

#[test]
fn stim_annotations_never_merge() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    let stim = |harness: &Harness<'_, KymoApp>| {
        viewer(harness)
            .document()
            .annotations
            .iter()
            .filter(|a| a.data.origin == kymo_core::Origin::Stim)
            .map(|a| a.data.clone())
            .collect::<Vec<_>>()
    };
    let before = stim(&harness);
    let event = before
        .iter()
        .find(|a| (2.0..18.0).contains(&a.onset))
        .expect("a stim annotation in view")
        .clone();
    // A new annotation of its description over it.
    describe(&mut harness, &event.description);
    let (a, b) = (
        at_time(&harness, event.onset - 1.0, 40.0),
        at_time(&harness, event.onset + 1.0, 40.0),
    );
    drag(&mut harness, a, b);
    assert_eq!(stim(&harness), before);
    let d = selected_data(&harness).unwrap();
    assert_eq!(d.origin, kymo_core::Origin::Viewer);
    assert_span((d.onset, d.end()), (event.onset - 1.0, event.onset + 1.0));
}

/// The descriptions of the viewer's and the file's annotations, by onset.
fn descriptions(harness: &Harness<'_, KymoApp>) -> Vec<(f64, f64, String)> {
    viewer(harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin != kymo_core::Origin::Stim)
        .map(|a| (a.data.onset, a.data.end(), a.data.description.clone()))
        .collect()
}

#[test]
fn the_field_changes_the_selected_annotations_description() {
    // A file annotation "trigger 3" from 15.5 to 16.5 s, and events.
    let file = without_triggers("bdf+c-small");
    let name = file
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut harness = window(file.path());
    ready(&mut harness);
    assert_eq!(harness.state().title(), format!("{name} – Kymo"));
    assert_eq!(harness.state().window_title(), harness.state().title());
    press(&mut harness, command(), Key::E);
    let at = |harness: &Harness<'_, KymoApp>, t| at_time(harness, t, 40.0);
    let (a, b) = (at(&harness, 2.0), at(&harness, 4.0));
    drag(&mut harness, a, b);
    assert_eq!(harness.state().title(), format!("{name} (edited) – Kymo"));
    assert_eq!(harness.state().window_title(), harness.state().title());
    let field = "Description of the selected annotation";
    assert_eq!(harness.get_by_label(field).value().as_deref(), Some("BAD"));
    let first = |harness: &Harness<'_, KymoApp>| made(harness)[0].2.clone();
    let retype = |harness: &mut Harness<'_, KymoApp>, text: &str| {
        harness.get_by_label(field).focus();
        harness.run_steps(2);
        press(harness, command(), Key::A);
        harness.get_by_label(field).type_text(text);
        harness.run_steps(2);
    };

    // Typed, it applies on Enter, not before: no legend entry per letter.
    retype(&mut harness, "eye");
    assert_eq!(first(&harness), "BAD");
    assert!(viewer(&harness).legend().entry(None, "e").is_none());
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    assert_eq!(first(&harness), "eye");
    assert_eq!(
        viewer(&harness).legend().entry(None, "eye").unwrap().count,
        1
    );
    assert!(
        viewer(&harness)
            .legend()
            .entries()
            .all(|e| e.description != "BAD")
    );
    // New annotations keep theirs.
    assert_eq!(viewer(&harness).description(), "BAD");
    // One step.
    press(&mut harness, command(), Key::Z);
    assert_eq!(first(&harness), "BAD");
    assert_eq!(harness.get_by_label(field).value().as_deref(), Some("BAD"));
    press(&mut harness, command() | Modifiers::SHIFT, Key::Z);
    assert_eq!(first(&harness), "eye");
    // Esc in the field takes back the typing, and keeps the selection.
    retype(&mut harness, "zzz");
    press(&mut harness, Modifiers::NONE, Key::Escape);
    harness.run_steps(2);
    assert_eq!(first(&harness), "eye");
    assert_eq!(harness.get_by_label(field).value().as_deref(), Some("eye"));
    assert!(viewer(&harness).selected().is_some());
    // An empty field changes nothing.
    harness.get_by_label(field).focus();
    harness.run_steps(2);
    press(&mut harness, command(), Key::A);
    press(&mut harness, Modifiers::NONE, Key::Backspace);
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    assert_eq!(first(&harness), "eye");
    assert_eq!(harness.get_by_label(field).value().as_deref(), Some("eye"));

    // A click on another annotation applies the typing to the one it was typed for.
    retype(&mut harness, "blink");
    click_at_time(&mut harness, 16.0);
    harness.run_steps(2);
    assert_eq!(first(&harness), "blink");
    assert_eq!(selected_data(&harness).unwrap().description, "trigger 3");
    assert_eq!(
        harness.get_by_label(field).value().as_deref(),
        Some("trigger 3")
    );
    // Picked from the menu, it applies at once, to a file annotation as well.
    harness.get_by_label("Descriptions").click();
    harness.run_steps(2);
    // Below the menu's fold: the trigger descriptions come first.
    click_in_list(&mut harness, egui::accesskit::Role::Button, "blink");
    harness.run_steps(2);
    let d = selected_data(&harness).unwrap();
    assert_eq!(
        (d.description.as_str(), d.origin),
        ("blink", kymo_core::Origin::File)
    );
    press(&mut harness, command(), Key::Z);
    assert_eq!(selected_data(&harness).unwrap().description, "trigger 3");

    // Renamed into a kind it reaches, it merges, and a hidden description shows again.
    describe(&mut harness, "x");
    let (a, b) = (at(&harness, 3.0), at(&harness, 5.0));
    drag(&mut harness, a, b);
    click_in_list(&mut harness, egui::accesskit::Role::CheckBox, "Show blink");
    assert!(
        !viewer(&harness)
            .legend()
            .entry(None, "blink")
            .unwrap()
            .shown
    );
    retype(&mut harness, "blink");
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    assert!(
        viewer(&harness)
            .legend()
            .entry(None, "blink")
            .unwrap()
            .shown
    );
    let blinks: Vec<_> = descriptions(&harness)
        .into_iter()
        .filter(|d| d.2 == "blink")
        .collect();
    assert_eq!(blinks.len(), 1, "{blinks:?}");
    assert_span((blinks[0].0, blinks[0].1), (2.0, 5.0));
    assert_span(selected_span(&harness), (2.0, 5.0));

    // Undoing every edit takes the title back; redoing one marks it again.
    while viewer(&harness).history().can_undo() {
        press(&mut harness, command(), Key::Z);
    }
    assert_eq!(harness.state().title(), format!("{name} – Kymo"));
    press(&mut harness, command() | Modifiers::SHIFT, Key::Z);
    assert_eq!(harness.state().title(), format!("{name} (edited) – Kymo"));
    // Closed, the title is the app's.
    press(&mut harness, command(), Key::W);
    harness.get_by_label("Don't Save").click();
    harness.run_steps(2);
    assert_eq!(harness.state().title(), "Kymo");
    assert_eq!(harness.state().window_title(), "Kymo");
}

/// The viewer's annotations (not the file's or stim ones) as onset, end and description.
fn viewer_spans(harness: &Harness<'_, KymoApp>) -> Vec<(f64, f64, String)> {
    viewer(harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin == kymo_core::Origin::Viewer)
        .map(|a| (a.data.onset, a.data.end(), a.data.description.clone()))
        .collect()
}

#[test]
fn saved_annotations_load_into_another_window() {
    // 12 file annotations, "trigger 0" to "trigger 11"; the recording starts at
    // 2024-01-02 03:04:05, its first sample at 0.
    let file = without_triggers("bdf+c-small");
    let dir = TempDir::new();
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    let (a, b) = (at_time(&harness, 2.0, 40.0), at_time(&harness, 4.0, 40.0));
    drag(&mut harness, a, b);
    let made = viewer_spans(&harness);
    // The dialog's name gets `.tsv`: nothing reads it as another format.
    harness
        .state_mut()
        .answer_file_dialogs([Some(dir.path().join("saved"))]);
    press(&mut harness, command(), Key::S);
    let saved = dir.path().join("saved.tsv");
    let text = std::fs::read_to_string(&saved).unwrap();
    assert_eq!(text.lines().next(), Some("onset\tduration\ttrial_type"));
    assert_eq!(text.lines().count(), 1 + 13);
    assert!(
        text.contains("\ntrigger 3\t") || text.contains("\ttrigger 3\n"),
        "{text}"
    );
    assert!(!harness.state().title().contains("(edited)"));
    // Saving again suggests the same file.
    assert_eq!(viewer(&harness).save_path(), saved);
    drop(harness);

    // In a new window, loaded as the viewer's, as one step.
    let mut harness = window(file.path());
    ready(&mut harness);
    harness
        .state_mut()
        .answer_file_dialogs([Some(saved.clone())]);
    choose(&mut harness, "File", "Load Annotations…");
    assert!(harness.state().notice().is_none());
    let loaded = viewer_spans(&harness);
    assert_eq!(loaded.len(), 13);
    assert!(loaded.contains(&made[0]), "{loaded:?}");
    assert!(harness.state().title().contains("(edited)"));
    // The legend counts them.
    assert_eq!(
        viewer(&harness).legend().entry(None, "BAD").unwrap().count,
        1
    );
    assert_eq!(
        viewer(&harness)
            .legend()
            .entry(None, "trigger 3")
            .unwrap()
            .count,
        2
    );
    press(&mut harness, command(), Key::Z);
    assert!(viewer_spans(&harness).is_empty());
    assert!(
        viewer(&harness)
            .legend()
            .entries()
            .all(|e| e.description != "BAD")
    );
    assert!(!harness.state().title().contains("(edited)"));

    // MNE's CSV: dates and times; another day's annotation is skipped, and said so.
    let csv = dir.path().join("mne.csv");
    std::fs::write(
        &csv,
        "onset,duration,description\n\
         2024-01-02 03:04:15.500,1.0,from MNE\n\
         2024-01-03 00:00:00.000,1.0,elsewhere\n",
    )
    .unwrap();
    assert!(harness.state_mut().load_annotations_from(&csv));
    assert_eq!(viewer_spans(&harness), [(10.5, 11.5, "from MNE".into())]);
    let (title, text) = harness.state().notice().unwrap();
    assert_eq!(title, "Annotations Loaded");
    assert!(
        text.contains("Loaded 1 annotation from mne.csv. Skipped 1 outside"),
        "{text}"
    );
    harness.run_steps(2);
    harness.get_by_label("OK").click();
    harness.run_steps(2);
    assert!(harness.state().notice().is_none());

    // A broken file adds nothing, and says where it broke.
    let broken = dir.path().join("broken.tsv");
    std::fs::write(&broken, "onset\tduration\ttrial_type\n1\tlong\tx\n").unwrap();
    assert!(!harness.state_mut().load_annotations_from(&broken));
    let (_, text) = harness.state().notice().unwrap();
    assert!(
        text.contains("broken.tsv: line 2: duration \"long\""),
        "{text}"
    );
    assert_eq!(viewer_spans(&harness).len(), 1);
    // As does another kind of file; Esc dismisses the notice, and nothing else.
    harness.run_steps(2);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert!(harness.state().notice().is_none());
    assert!(!harness.state_mut().load_annotations_from(file.path()));
    assert!(
        harness
            .state()
            .notice()
            .unwrap()
            .1
            .contains("not a .tsv or .csv file")
    );
}

#[test]
fn export_writes_the_descriptions_shown() {
    // File annotations "trigger 0" to "trigger 11", and stim annotations.
    let file = TempFile::preset("bdf+c-small");
    let dir = TempDir::new();
    let mut harness = window(file.path());
    ready(&mut harness);
    let stim = viewer(&harness)
        .document()
        .annotations
        .iter()
        .filter(|a| a.data.origin == kymo_core::Origin::Stim)
        .count();
    assert!(stim > 0);
    press(&mut harness, command(), Key::E);
    let (a, b) = (at_time(&harness, 2.0, 40.0), at_time(&harness, 4.0, 40.0));
    drag(&mut harness, a, b);
    click_in_list(
        &mut harness,
        egui::accesskit::Role::CheckBox,
        "Show trigger 3",
    );
    let export = dir.path().join("shown.tsv");
    harness
        .state_mut()
        .answer_file_dialogs([Some(export.clone())]);
    choose(&mut harness, "File", "Export Shown Annotations…");
    let text = std::fs::read_to_string(&export).unwrap();
    // 11 of the file's, the new one, and the stim ones.
    assert_eq!(text.lines().count(), 1 + 11 + 1 + stim, "{text}");
    assert!(!text.contains("trigger 3"));
    assert!(text.contains("\tBAD"));
    // Not a save.
    assert!(harness.state().title().contains("(edited)"));
    // Saving writes them all, stim ones apart.
    let saved = dir.path().join("all.tsv");
    harness
        .state_mut()
        .answer_file_dialogs([Some(saved.clone())]);
    choose(&mut harness, "File", "Save Annotations…");
    let text = std::fs::read_to_string(&saved).unwrap();
    assert_eq!(text.lines().count(), 1 + 12 + 1, "{text}");

    // With every description hidden there is nothing to export, until one shows again.
    assert!(enabled(&mut harness, "File", "Export Shown"));
    // Mixed, as trigger 3 is hidden: the first click shows all, the second hides all.
    for _ in 0..2 {
        click_in_list(
            &mut harness,
            egui::accesskit::Role::CheckBox,
            "Show Annotations",
        );
    }
    assert!(!enabled(&mut harness, "File", "Export Shown"));
    click_in_list(
        &mut harness,
        egui::accesskit::Role::CheckBox,
        "Show trigger 3",
    );
    assert!(enabled(&mut harness, "File", "Export Shown"));
}

#[test]
fn unsaved_changes_are_asked_about() {
    let file = without_triggers("bdf+c-small");
    let other = TempFile::preset("bdf+c-small");
    let dir = TempDir::new();
    let mut harness = window(file.path());
    ready(&mut harness);
    // Without changes, Close closes.
    press(&mut harness, command(), Key::W);
    assert_eq!(harness.state().title(), "Kymo");

    let edit = |harness: &mut Harness<'_, KymoApp>| {
        harness
            .state_mut()
            .answer_file_dialogs([Some(file.path().to_owned())]);
        press(harness, command(), Key::O);
        ready(harness);
        press(harness, command(), Key::E);
        let (a, b) = (at_time(harness, 2.0, 40.0), at_time(harness, 4.0, 40.0));
        drag(harness, a, b);
        assert!(harness.state().title().contains("(edited)"));
    };
    let answer = |harness: &mut Harness<'_, KymoApp>, button: &str| {
        harness.step();
        harness.get_by_label(button).click();
        harness.run_steps(2);
    };
    edit(&mut harness);
    // Cancel, and Esc, keep it open; keys wait for the answer.
    press(&mut harness, command(), Key::W);
    assert_eq!(harness.state().leaving(), Some(kymo::app::Leave::Close));
    harness.get_by_label_contains("Save the changes to the annotations of");
    answer(&mut harness, "Cancel");
    assert_eq!(harness.state().leaving(), None);
    assert!(harness.state().title().contains("(edited)"));
    press(&mut harness, command(), Key::W);
    press(&mut harness, command(), Key::Z);
    assert_eq!(viewer_spans(&harness).len(), 1);
    harness.step();
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert_eq!(harness.state().leaving(), None);
    assert!(viewer(&harness).selected().is_some());
    // Save… with the save dialog canceled stays; saved, it closes.
    press(&mut harness, command(), Key::W);
    harness.state_mut().answer_file_dialogs([None]);
    answer(&mut harness, "Save…");
    assert!(harness.state().title().contains("(edited)"));
    let saved = dir.path().join("kept.tsv");
    press(&mut harness, command(), Key::W);
    harness
        .state_mut()
        .answer_file_dialogs([Some(saved.clone())]);
    answer(&mut harness, "Save…");
    assert_eq!(harness.state().title(), "Kymo");
    assert_eq!(
        std::fs::read_to_string(&saved).unwrap().lines().count(),
        1 + 13
    );

    // Opening another: Don't Save goes on to the dialog.
    edit(&mut harness);
    press(&mut harness, command(), Key::O);
    assert_eq!(harness.state().leaving(), Some(kymo::app::Leave::Open));
    harness
        .state_mut()
        .answer_file_dialogs([Some(other.path().to_owned())]);
    answer(&mut harness, "Don't Save");
    ready(&mut harness);
    assert_eq!(viewer(&harness).document().path, other.path());

    // Quitting, from the menu or the window's close button.
    edit(&mut harness);
    press(&mut harness, command(), Key::Q);
    assert_eq!(harness.state().leaving(), Some(kymo::app::Leave::Quit));
    assert!(!harness.state().closing());
    answer(&mut harness, "Cancel");
    harness
        .input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    harness.step();
    assert_eq!(harness.state().leaving(), Some(kymo::app::Leave::Quit));
    answer(&mut harness, "Don't Save");
    assert!(harness.state().closing());
}

/// The sidebar lists every type the recording has, in the sidebar's order, each in its
/// color; the Channel Types window has a swatch for every type kymo knows, alphabetically.
#[test]
fn every_channel_type_has_a_row_a_color_and_a_swatch() {
    use egui::accesskit::Role;
    let file = TempFile::preset("edf+c-kinds");
    // Tall enough for every type's row, collapsed, above the selection bar.
    let mut harness = screenshot_window_sized(file.path(), egui::vec2(1200.0, 800.0));
    ready(&mut harness);
    // Top to bottom, the types of edf+c-kinds: EEG, MEG, EOG, ECG, the other signals,
    // stim; none it lacks.
    let present = [
        "EEG", "MEG Mag", "MEG Grad", "EOG", "ECG", "EMG", "sEEG", "ECoG", "DBS", "Resp", "SaO2",
        "Stim",
    ];
    // Each collapsed, so all rows fit.
    for name in present {
        harness
            .get_by_label(&format!("Collapse {name} channels"))
            .click();
        harness.run_steps(2);
    }
    let tops: Vec<f32> = present
        .iter()
        .map(|name| {
            harness
                .get_by_role_and_label(Role::CheckBox, &format!("Show {name} channels"))
                .rect()
                .top()
        })
        .collect();
    assert!(tops.is_sorted_by(|a, b| a < b), "{tops:?}");
    for absent in ["MEG Ref", "Bio", "Misc"] {
        assert!(
            harness
                .query_by_role_and_label(Role::CheckBox, &format!("Show {absent} channels"))
                .is_none()
        );
    }
    // Each row's name in its type's color.
    let image = harness.render().unwrap();
    let visuals = harness.ctx.global_style().visuals.clone();
    for kind in [
        ChannelKind::MAG,
        ChannelKind::SEEG,
        ChannelKind::DBS,
        ChannelKind::EOG,
    ] {
        let r = kind_name_rect(&harness, kind.name());
        let color = trace_color(&kind, &visuals);
        let colored = (r.left() as u32..r.right() as u32)
            .flat_map(|x| (r.top() as u32..r.bottom() as u32).map(move |y| (x, y)))
            .filter(|&(x, y)| near(&image.get_pixel(x, y).0, color))
            .count();
        assert!(colored > 10, "{kind:?}: {colored} pixels of {color:?}");
    }

    // The Channel Types window lists every type kymo knows, alphabetically, each with its
    // swatch.
    open_channel_types(&mut harness);
    let mut names: Vec<&str> = kymo_core::KNOWN_KINDS.iter().map(|k| k.name).collect();
    names.sort_by_key(|n| n.to_lowercase());
    let tops: Vec<f32> = names
        .iter()
        .map(|name| {
            harness
                .get_by_label(&format!("Color of {name}"))
                .rect()
                .top()
        })
        .collect();
    assert!(tops.is_sorted_by(|a, b| a < b), "{tops:?}");
    for kind in &KINDS {
        harness.get_by_label(&format!("Color of {}", kind.name()));
    }
}

/// Settings lists how it looks first, then how a recording opens, then memory.
#[test]
fn settings_put_appearance_first_and_memory_last() {
    use egui::accesskit::Role;
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    harness.key_press_modifiers(command(), Key::Comma);
    harness.run_steps(2);
    let tops: Vec<f32> = [
        "Theme",
        "Channel Types",
        "Default Window",
        "Scale",
        "Tile Cache",
        "GPU Memory",
    ]
    .iter()
    .map(|label| {
        harness
            .get_by_role_and_label(Role::Label, label)
            .rect()
            .top()
    })
    .collect();
    assert!(tops.is_sorted(), "{tops:?}");
}

/// The sidebar has no Open button: File → Open and ⌘O open files, and the empty window
/// keeps its button.
#[test]
fn the_sidebar_has_no_open_button() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    assert!(harness.query_by_label("Open…").is_none());
    harness.key_press_modifiers(command(), Key::W);
    harness.run_steps(2);
    harness.get_by_label("Open…");
}

/// A recording with 60 channels, more than the channel list shows at once.
fn many_channels() -> TempFile {
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    let status = spec.channels.pop().unwrap();
    for k in 0..55 {
        spec.channels.push(kymo_testgen::ChannelSpec::eeg(
            format!("E{k}"),
            512,
            spec.format,
        ));
    }
    spec.channels.push(status);
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, file.path()).unwrap();
    file
}

/// Another recording's channel list starts at its top, not where the last one was.
#[test]
fn the_channel_list_starts_at_the_top_for_each_recording() {
    use egui::accesskit::Role;
    let (first, second) = (many_channels(), many_channels());
    let mut harness = window(first.path());
    ready(&mut harness);
    let a1 = harness
        .get_by_role_and_label(Role::Label, "A1")
        .rect()
        .center();
    harness.hover_at(a1);
    for _ in 0..10 {
        scroll(&mut harness, egui::vec2(0.0, -2000.0), Modifiers::NONE);
    }
    harness.run_steps(2);
    assert!(harness.query_by_label("Show A1").is_none(), "not scrolled");

    let ctx = harness.ctx.clone();
    harness.state_mut().open(&ctx, second.path().to_owned());
    ready(&mut harness);
    assert!(
        harness.query_by_label("Show A1").is_some(),
        "still scrolled"
    );
}

/// A file dropped on the window, as egui-winit hands it over.
#[derive(Debug)]
struct Dropped(std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// Drops `paths` on the window.
fn drop_files(harness: &mut Harness<'_, KymoApp>, paths: &[&Path]) {
    harness.input_mut().dropped_files = paths
        .iter()
        .map(|p| std::sync::Arc::new(Dropped(p.to_path_buf())) as egui::DroppedFileHandle)
        .collect();
    harness.run_steps(2);
}

#[test]
fn dropped_files_open_or_load_their_annotations() {
    let missing = TempFile::new("bdf");
    let file = without_triggers("bdf+c-small");
    let other = without_triggers("bdf+c-small");
    let dir = TempDir::new();
    let events = dir.path().join("events.tsv");
    std::fs::write(&events, "onset\tduration\ttrial_type\n1.5\t1\tdropped\n").unwrap();
    let mut harness = window(missing.path());
    step_until(&mut harness, "the error", |app| app.error().is_some());

    // Annotations need a recording first; the notice says so.
    drop_files(&mut harness, &[&events]);
    let (_, text) = harness.state().notice().unwrap();
    assert!(text.contains("Open a recording first"), "{text}");
    harness.get_by_label("OK").click();
    harness.run_steps(2);

    // A recording opens; of several files, only the first is taken.
    drop_files(&mut harness, &[file.path(), other.path()]);
    ready(&mut harness);
    assert_eq!(viewer(&harness).document().path, file.path());
    drop_files(&mut harness, &[&events]);
    assert_eq!(viewer_spans(&harness), [(1.5, 2.5, "dropped".into())]);
    assert!(harness.state().title().contains("(edited)"));

    // With unsaved changes, another recording waits for the answer.
    drop_files(&mut harness, &[other.path()]);
    assert_eq!(
        harness.state().leaving(),
        Some(kymo::app::Leave::OpenPath(other.path().to_owned()))
    );
    harness.get_by_label("Don't Save").click();
    harness.run_steps(2);
    ready(&mut harness);
    assert_eq!(viewer(&harness).document().path, other.path());
}

#[test]
fn files_handed_over_by_the_system_open_as_dropped_ones() {
    let file = without_triggers("bdf+c-small");
    let other = without_triggers("bdf+c-small");
    let dir = TempDir::new();
    let events = dir.path().join("events.tsv");
    std::fs::write(&events, "onset\tduration\ttrial_type\n1.5\t1\thanded\n").unwrap();
    let inbox = std::sync::Arc::new(kymo::opened::Inbox::new());
    // Started without a recording, as Finder starts an app bundle, with the file it was
    // started for already handed over: it opens instead of the file dialog (which would
    // pick `other`).
    inbox.push([file.path().to_owned()]);
    let receiving = inbox.clone();
    let picked = other.path().to_owned();
    let mut harness = Harness::builder().with_size(SIZE).build_eframe(move |cc| {
        system_fonts(&cc.egui_ctx);
        let mut app = KymoApp::new(
            &cc.egui_ctx,
            Gpu::none("no GPU in this test"),
            options(),
            None,
        );
        app.answer_file_dialogs([Some(picked)]);
        app.receive_opened(&cc.egui_ctx, receiving);
        app
    });
    ready(&mut harness);
    harness.run_steps(4);
    assert_eq!(viewer(&harness).document().path, file.path());

    // Annotations load into the open recording.
    inbox.push([events.clone()]);
    harness.run_steps(2);
    assert_eq!(viewer_spans(&harness), [(1.5, 2.5, "handed".into())]);

    // With unsaved changes, another recording waits for the answer, and a file handed over
    // meanwhile waits for it to be given.
    inbox.push([other.path().to_owned()]);
    harness.run_steps(2);
    assert_eq!(
        harness.state().leaving(),
        Some(kymo::app::Leave::OpenPath(other.path().to_owned()))
    );
    inbox.push([file.path().to_owned()]);
    harness.run_steps(2);
    assert!(!inbox.is_empty());
    // Given, the other recording opens, and then, without a question (nothing is edited
    // any more), the one that waited replaces it.
    harness.get_by_label("Don't Save").click();
    harness.run_steps(2);
    assert!(inbox.is_empty());
    assert_eq!(harness.state().leaving(), None);
    ready(&mut harness);
    assert_eq!(viewer(&harness).document().path, file.path());
}

/// Opens File → Open Recent and clicks `item` in it.
fn choose_recent(harness: &mut Harness<'_, KymoApp>, item: &str) {
    harness.get_by_label("File").click();
    harness.run_steps(2);
    harness.get_by_label("Open Recent").click();
    harness.run_steps(2);
    harness.get_by_label(item).click();
    harness.run_steps(2);
}

#[test]
fn open_recent_reopens_recordings_and_forgets_missing_ones() {
    let first = without_triggers("bdf+c-small");
    let second = TempFile::preset("bdf+c-small");
    let name = |file: &TempFile| {
        file.path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    };
    let (first_name, second_name) = (name(&first), name(&second));
    let mut harness = window(first.path());
    ready(&mut harness);
    harness
        .state_mut()
        .answer_file_dialogs([Some(second.path().to_owned())]);
    harness.key_press_modifiers(command(), Key::O);
    // Until the press is handled, the first recording is still the one ready.
    harness.run_steps(2);
    ready(&mut harness);
    assert_eq!(
        harness.state().recent().paths(),
        [second.path(), first.path()]
    );

    // Chosen from the menu, it opens and moves to the top.
    choose_recent(&mut harness, &first_name);
    ready(&mut harness);
    assert_eq!(viewer(&harness).document().path, first.path());
    assert_eq!(
        harness.state().recent().labels(),
        [first_name.clone(), second_name.clone()]
    );

    // One that has gone shows the error and leaves the list.
    let gone = second.path().to_owned();
    drop(second);
    choose_recent(&mut harness, &second_name);
    step_until(&mut harness, "the error", |app| app.error().is_some());
    assert_eq!(harness.state().recent().paths(), [first.path()]);

    // Kept for the next run.
    let mut storage = Memory::default();
    eframe::App::save(harness.state_mut(), &mut storage);
    let kept = kymo::recent::Recent::load(Some(&storage));
    assert_eq!(kept.paths(), [first.path()]);
    assert!(!kept.paths().contains(&gone));

    // Clear Menu empties it, and is disabled then.
    choose_recent(&mut harness, "Clear Menu");
    assert!(harness.state().recent().paths().is_empty());
    harness.get_by_label("File").click();
    harness.run_steps(2);
    harness.get_by_label("Open Recent").click();
    harness.run_steps(2);
    assert!(
        harness
            .get_by_label("Clear Menu")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn go_to_time_moves_the_window_there() {
    use egui::accesskit::Role;
    // A minute, 20 s in view.
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let press = |h: &mut Harness<'_, KymoApp>, modifiers, key| {
        h.key_press_modifiers(modifiers, key);
        h.run_steps(2);
    };
    let field =
        |h: &Harness<'_, KymoApp>| h.get_by_role(Role::TextInput).value().unwrap_or_default();

    // It opens with the current time, selected, so typing replaces it.
    press(&mut harness, command(), Key::G);
    assert!(harness.state().go_to_open());
    assert_eq!(field(&harness), "0 s");
    harness.get_by_role(Role::TextInput).type_text("0:25");
    harness.run_steps(2);
    assert_eq!(field(&harness), "0:25");
    press(&mut harness, Modifiers::NONE, Key::Enter);
    assert!(!harness.state().go_to_open());
    assert_eq!(viewer(&harness).view().start(), 25.0);

    // How to write a time is described below the field; something that isn't a time says
    // so, and stays.
    press(&mut harness, command(), Key::G);
    harness.get_by_label_contains("minutes:seconds (1:30)");
    assert_eq!(field(&harness), "25 s");
    harness.get_by_role(Role::TextInput).type_text("soon");
    harness.run_steps(2);
    press(&mut harness, Modifiers::NONE, Key::Enter);
    assert!(harness.state().go_to_open());
    harness.get_by_label("Not a time.");
    // Past the end, the window ends with the recording.
    harness.get_by_role(Role::TextInput).type_text("1:00:00");
    harness.run_steps(2);
    // The last "Go" button: the menu bar's comes first.
    harness
        .get_all_by_role_and_label(Role::Button, "Go")
        .last()
        .unwrap()
        .click();
    harness.run_steps(2);
    assert!(!harness.state().go_to_open());
    assert_eq!(viewer(&harness).view().start(), 40.0);

    // Esc closes it and goes nowhere.
    press(&mut harness, command(), Key::G);
    harness.get_by_role(Role::TextInput).type_text("10");
    harness.run_steps(2);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert!(!harness.state().go_to_open());
    assert_eq!(viewer(&harness).view().start(), 40.0);
}

/// Each label in Settings sits level with its setting's first line, rather than centered on
/// all of them.
#[test]
fn settings_labels_are_level_with_their_first_control() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let ctx = harness.ctx.clone();
    harness.state_mut().perform(&ctx, Action::Settings);
    harness.run_steps(3);
    let center = |label: &str| {
        // The leftmost, should a label come twice.
        let all = harness.get_all_by_label(label).collect::<Vec<_>>();
        all.iter()
            .map(|n| n.rect())
            .min_by(|a, b| a.left().total_cmp(&b.left()))
            .unwrap()
            .center()
            .y
    };
    for (label, control) in [
        ("Theme", "System"),
        ("Scale", "Automatic on Opening"),
        ("Updates", "Check at Startup"),
    ] {
        assert!(
            (center(label) - center(control)).abs() < 0.5,
            "{label} at {}, {control} at {}",
            center(label),
            center(control)
        );
    }
    // The button is named by its label (the leftmost of the two).
    let button = harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Channel Types")
        .rect();
    assert!((center("Channel Types") - button.center().y).abs() < 0.5);
}

/// An XDF file of four streams: "Amp" (EEG, three channels at 10 Hz), "Tracker" (Gaze, one
/// channel at 4 Hz), "Empty" (no samples) and "Markers" (strings).
fn streams_xdf() -> TempFile {
    use kymo_testgen::XdfFormat;
    use kymo_testgen::xdf::{Chunks, header_xml};
    let described = |labels: &[&str], kind: &str| -> Vec<(String, String, String)> {
        labels
            .iter()
            .map(|l| ((*l).to_owned(), kind.to_owned(), "microvolts".to_owned()))
            .collect()
    };
    let mut c = Chunks::new(None);
    let amp = described(&["Fz", "Cz", "Pz"], "EEG");
    c.stream_header(1, &header_xml("Amp", "EEG", 3, 10.0, "float32", Some(&amp)));
    let tracker = described(&["Pupil"], "Misc");
    c.stream_header(
        2,
        &header_xml("Tracker", "Gaze", 1, 4.0, "float32", Some(&tracker)),
    );
    c.stream_header(3, &header_xml("Empty", "EEG", 2, 10.0, "float32", None));
    c.stream_header(4, &header_xml("Markers", "Markers", 1, 0.0, "string", None));
    let samples = |n: usize, rate: f64, channels: usize| -> Vec<(Option<f64>, Vec<f64>)> {
        (0..n)
            .map(|i| (Some(i as f64 / rate), vec![i as f64; channels]))
            .collect()
    };
    c.numeric(1, XdfFormat::F32, &samples(300, 10.0, 3));
    c.numeric(2, XdfFormat::F32, &samples(120, 4.0, 1));
    c.strings(4, &[(Some(1.0), vec!["go".into()])]);
    for id in 1..=4 {
        c.footer(id, "<info/>");
    }
    let file = TempFile::new("xdf");
    std::fs::write(file.path(), &c.bytes).unwrap();
    file
}

/// A recording with several streams of channels groups its channel list by stream: a row
/// per stream shows and hides its channels, collapses their rows, scrolls the traces to it,
/// and has the rate the channels' rows leave out; streams that aren't shown are listed,
/// grayed, with the reason on hover; the summary counts the streams.
#[test]
fn several_streams_group_the_channel_list() {
    use egui::accesskit::{Role, Toggled};
    let file = streams_xdf();
    let mut harness = window(file.path());
    ready(&mut harness);
    harness.get_by_label("Streams");
    harness.get_by_label("4 (2 shown as channels)");
    assert_eq!(shown(&harness), [0, 1, 2, 3]);
    // The streams' rows, with the number of channels and the rate; none in the channels'.
    harness.get_by_label("3 \u{b7} 10 Hz");
    harness.get_by_label("1 \u{b7} 4 Hz");
    assert!(harness.query_by_label("10 Hz").is_none());
    assert_eq!(toggled(&harness, Role::CheckBox, "Show Amp"), Toggled::True);
    // Its type's box a level in, the channels' another.
    let left = |h: &Harness<'_, KymoApp>, label: &str| h.get_by_label(label).rect().left();
    let level = left(&harness, "Show EEG channels in Amp") - left(&harness, "Show Amp");
    assert!((level - LEVEL).abs() <= 0.5, "{level}");
    let level = left(&harness, "Show Fz") - left(&harness, "Show EEG channels in Amp");
    assert!((level - LEVEL).abs() <= 0.5, "{level}");
    assert!(left(&harness, "Collapse Amp") < left(&harness, "Show Amp"));

    // A stream's box hides and shows its channels; mixed while some are shown.
    click_in_list(&mut harness, Role::CheckBox, "Show Amp");
    assert_eq!(shown(&harness), [3]);
    assert_eq!(toggled(&harness, Role::CheckBox, "Show Fz"), Toggled::False);
    click_in_list(&mut harness, Role::CheckBox, "Show Amp");
    assert_eq!(shown(&harness), [0, 1, 2, 3]);
    click_in_list(&mut harness, Role::CheckBox, "Show Cz");
    assert_eq!(
        toggled(&harness, Role::CheckBox, "Show Amp"),
        Toggled::Mixed
    );
    assert_eq!(
        toggled(&harness, Role::CheckBox, "Show Tracker"),
        Toggled::True
    );
    click_in_list(&mut harness, Role::CheckBox, "Show Amp");
    assert_eq!(shown(&harness), [0, 1, 2, 3]);

    // Its arrow hides its channels' rows, not their traces.
    harness.get_by_label("Collapse Amp").click();
    harness.run_steps(2);
    assert!(harness.query_by_label("Show Fz").is_none());
    harness.get_by_label("Show Pupil");
    assert_eq!(shown(&harness), [0, 1, 2, 3]);
    harness.get_by_label("Expand Amp").click();
    harness.run_steps(2);
    harness.get_by_label("Show Fz");

    // Its name scrolls the traces to its first shown channel: two rows of four.
    for _ in 0..2 {
        harness.key_press_modifiers(command() | Modifiers::SHIFT, Key::Minus);
        harness.step();
    }
    assert_eq!(viewer(&harness).view().rows(), 2);
    click_in_list(&mut harness, Role::Label, "Tracker");
    assert_eq!(viewer(&harness).view().top(), 2.0);
    click_in_list(&mut harness, Role::Label, "Amp");
    assert_eq!(viewer(&harness).view().top(), 0.0);

    // The empty stream, grayed, says why; the marker stream's annotations are in the
    // legend.
    let empty = harness.get_by_role_and_label(Role::Label, "Empty").rect();
    let amp = harness.get_by_role_and_label(Role::Label, "Amp").rect();
    assert_eq!(empty.left(), amp.left());
    hover(&mut harness, empty.center());
    assert!(tooltip(
        &mut harness,
        "Empty isn't shown: it has no samples"
    ));
}

/// Grouped by stream, a stream's channels are under their types: each type a row in it, a
/// level in, that shows and hides and folds its channels, also in a stream of one type. The
/// columns after the names line up whatever the rows' levels.
#[test]
fn streams_of_several_types_group_their_channels_by_type() {
    use egui::accesskit::Role;
    // Amp: Fz (EEG), AUX1 and POR X (AUX); Tracker: IPD (Gaze).
    let file = long_names_xdf();
    let mut harness = window(file.path());
    ready(&mut harness);
    assert_eq!(
        folds(&harness),
        [
            "Collapse Amp",
            "Collapse EEG channels in Amp",
            "Collapse AUX channels in Amp",
            "Collapse Tracker",
            "Collapse Gaze channels in Tracker"
        ]
    );
    let left = |h: &Harness<'_, KymoApp>, label: &str| h.get_by_label(label).rect().left();
    let level = LEVEL;
    let near = |a: f32, b: f32| (a - b).abs() <= 0.5;
    let misc = left(&harness, "Show AUX channels in Amp");
    assert!(near(misc - left(&harness, "Show Amp"), level));
    assert!(near(left(&harness, "Show AUX1") - misc, level));
    // Its count, without the rate, which the stream's row has.
    let row = harness
        .get_by_label("Show AUX channels in Amp")
        .rect()
        .center()
        .y;
    let in_row: Vec<String> = harness
        .query_all_by_role(Role::Label)
        .filter(|n| (n.rect().center().y - row).abs() < 1.0)
        .filter_map(|n| n.accesskit_node().value())
        .collect();
    assert_eq!(in_row, ["AUX", "2"]);
    harness.get_by_label("3 \u{b7} 100 Hz");
    let tracker = left(&harness, "Show Tracker");
    let ipd = "IPD - Interpupillary distance, both eyes";
    // Under its stream's one type.
    assert!(near(
        left(&harness, &format!("Show {ipd}")) - tracker,
        2.0 * level
    ));
    // AUX's channels differ in unit, so each shows its own, in one column; Gaze's one
    // channel's is in its type's row.
    sidebar_to(&mut harness, 600.0);
    // The label `text` in the row of `of`.
    let unit = |h: &Harness<'_, KymoApp>, of: &str, text: &str| {
        let y = h.get_by_label(of).rect().center().y;
        h.query_all_by_role_and_label(Role::Label, text)
            .map(|n| n.rect())
            .find(|r| (r.center().y - y).abs() < 1.0)
            .unwrap()
    };
    let por = "POR X - (Point of regard), left eye";
    assert_eq!(
        unit(&harness, "Show AUX1", "\u{b5}V").left(),
        unit(&harness, &format!("Show {por}"), "normalized_signed").left()
    );
    let gaze = harness
        .get_by_label("Show Gaze channels in Tracker")
        .rect()
        .center()
        .y;
    let ipd = format!("Show {ipd}");
    assert!(
        (unit(&harness, "Show Gaze channels in Tracker", "mm")
            .center()
            .y
            - gaze)
            .abs()
            < 1.0
    );
    assert!(
        harness
            .query_all_by_role_and_label(Role::Label, "mm")
            .all(
                |n| (n.rect().center().y - harness.get_by_label(&ipd).rect().center().y).abs()
                    >= 1.0
            )
    );

    // Its box shows and hides only its channels in this stream.
    click_in_list(&mut harness, Role::CheckBox, "Show AUX channels in Amp");
    assert_eq!(shown(&harness), [0, 3]);
    click_in_list(&mut harness, Role::CheckBox, "Show AUX channels in Amp");
    assert_eq!(shown(&harness), [0, 1, 2, 3]);
    // Its arrow folds them.
    harness.get_by_label("Collapse AUX channels in Amp").click();
    harness.run_steps(2);
    assert!(harness.query_by_label("Show AUX1").is_none());
    harness.get_by_label("Show Fz");
    harness.get_by_label("Expand AUX channels in Amp").click();
    harness.run_steps(2);
    harness.get_by_label("Show AUX1");
}
/// An XDF file of two streams of 100 Hz: "Amp" (Fz, Cz and Pz, EEG) and "Joystick" (X, Y
/// and Fire, each its own type: PositionX, PositionY, Button, as joysticks type each axis).
fn joystick_xdf() -> TempFile {
    use kymo_testgen::{ChannelSpec, Spec, XdfFormat, XdfStream};
    let channel = |label: &str, kind: &str, unit: &str| {
        ChannelSpec::xdf(label, kind, unit, 100, XdfFormat::F32)
    };
    let channels = vec![
        channel("Fz", "EEG", "microvolts"),
        channel("Cz", "EEG", "microvolts"),
        channel("Pz", "EEG", "microvolts"),
        channel("X", "PositionX", "normalized"),
        channel("Y", "PositionY", "normalized"),
        channel("Fire", "Button", "normalized"),
    ];
    let mut spec = Spec::new(kymo_testgen::Format::Xdf, channels, 1.0, 30);
    spec.xdf.streams = vec![
        XdfStream::new(1, "Amp", "EEG", vec![0, 1, 2]),
        XdfStream::new(2, "Joystick", "Controller", vec![3, 4, 5]),
    ];
    TempFile::written(&spec)
}

/// Grouped by stream, every stream's channels are under their types' rows, whether the
/// stream has one type or one per channel (a joystick typing each axis), each type row with
/// its swatch and menu, its count lined up with the stream's; the channels two levels in.
#[test]
fn every_stream_has_type_rows() {
    let file = joystick_xdf();
    let mut harness = window(file.path());
    ready(&mut harness);
    assert_eq!(
        folds(&harness),
        [
            "Collapse Amp",
            "Collapse EEG channels in Amp",
            "Collapse Joystick",
            "Collapse PositionX channels in Joystick",
            "Collapse PositionY channels in Joystick",
            "Collapse Button channels in Joystick",
        ]
    );
    for kind in ["EEG channels in Amp", "Button channels in Joystick"] {
        harness.get_by_label(&format!("Options for {kind}"));
    }
    // A stream's count ends where its types' counts do, before their menus.
    let in_row = |h: &Harness<'_, KymoApp>, of: &str, text: &str| {
        let y = h.get_by_label(of).rect().center().y;
        h.query_all_by_role(egui::accesskit::Role::Label)
            .filter(|n| n.accesskit_node().value().is_some_and(|l| l.contains(text)))
            .map(|n| n.rect())
            .find(|r| (r.center().y - y).abs() < 2.0)
            .unwrap()
    };
    let stream = in_row(&harness, "Show Amp", " Hz").right();
    let kind = in_row(&harness, "Show EEG channels in Amp", "3").right();
    assert!((stream - kind).abs() <= 0.5, "{stream} {kind}");
    let menu = harness
        .get_by_label("Options for EEG channels in Amp")
        .rect();
    assert!(kind < menu.left(), "{kind} {menu:?}");
    let left = |h: &Harness<'_, KymoApp>, label: &str| h.get_by_label(label).rect().left();
    for (channel, stream) in [("Fz", "Amp"), ("X", "Joystick")] {
        let level =
            left(&harness, &format!("Show {channel}")) - left(&harness, &format!("Show {stream}"));
        assert!((level - 2.0 * LEVEL).abs() <= 0.5, "{channel}: {level}");
    }
}

/// A type's menu in the sidebar ("⋯"): its Remove DC and Scale Each Channel checked as they
/// are, each acting at once and closing the menu, as menu items do; Reset while anything
/// differs from the type's start (its color too), which puts it back; the button gray until
/// DC removal or scaling differs, a color alone leaving it so; stim's without the two items; All Types… opens the Channel
/// Types window and closes the menu. The swatch sets only the color.
#[test]
fn a_types_menu_sets_it_and_opens_all_types() {
    use egui::accesskit::{Role, Toggled};
    let file = typed_preset();
    let mut harness = fonts_window(file.path(), options());
    ready(&mut harness);
    let types = |h: &Harness<'_, KymoApp>| h.state().settings().types.clone();
    let eog = ChannelKind::EOG;
    let remove_dc = "Remove DC of EOG";
    let own_scale = "Scale Each EOG Channel";
    let reset_off =
        |h: &Harness<'_, KymoApp>| h.get_by_label("Reset EOG").accesskit_node().is_disabled();
    // The brightest channel value of the menu button's dots, the pointer away from it.
    let dots = |h: &mut Harness<'_, KymoApp>| {
        hover(h, egui::pos2(900.0, 650.0));
        h.run_steps(2);
        let r = h.get_by_label("Options for EOG channels").rect();
        let image = h.render().unwrap();
        (r.left() as u32..r.right() as u32)
            .flat_map(|x| (r.top() as u32..r.bottom() as u32).map(move |y| (x, y)))
            .map(|(x, y)| image.get_pixel(x, y).0[..3].iter().copied().max().unwrap())
            .max()
            .unwrap()
    };
    let gray = dots(&mut harness);

    // The swatch: a color picker with Reset, nothing else.
    harness.get_by_label("Color of EOG channels").click();
    harness.run_steps(2);
    harness.get_by_role_and_label(Role::SpinButton, "R");
    assert!(harness.query_by_label(remove_dc).is_none());
    assert!(harness.query_by_label(ALL_TYPES).is_none());
    harness.key_press(Key::Escape);
    harness.run_steps(3);

    type_options(&mut harness, "EOG channels");
    assert_eq!(toggled(&harness, Role::CheckBox, remove_dc), Toggled::True);
    assert_eq!(toggled(&harness, Role::CheckBox, own_scale), Toggled::False);
    assert!(reset_off(&harness));
    harness.get_by_label(own_scale).click();
    harness.run_steps(2);
    assert!(types(&harness).own_scale(&eog));
    assert!(viewer(&harness).view().type_settings().own_scale(&eog));
    assert!(
        harness.query_by_label(own_scale).is_none(),
        "the menu stayed open"
    );
    // Brighter while a setting differs from the type's start.
    let changed = dots(&mut harness);
    assert!(changed > gray + 20, "{gray} {changed}");
    type_options(&mut harness, "EOG channels");
    assert_eq!(toggled(&harness, Role::CheckBox, own_scale), Toggled::True);
    assert!(!reset_off(&harness));
    harness.get_by_label(remove_dc).click();
    harness.run_steps(2);
    assert!(!types(&harness).remove_dc(&eog));
    type_options(&mut harness, "EOG channels");
    harness.get_by_label("Reset EOG").click();
    harness.run_steps(2);
    assert_eq!(types(&harness), TypeSettings::default());
    assert!(!viewer(&harness).view().type_settings().own_scale(&eog));

    // A picked color alone enables Reset, which puts it back too.
    pick(&mut harness, "Color of EOG channels", [10, 200, 30]);
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    assert!(types(&harness).color(&eog).is_some());
    // A color alone leaves it gray, as the swatch shows it.
    assert_eq!(dots(&mut harness), gray);
    // The swatch's Reset puts the color back.
    harness.get_by_label("Color of EOG channels").click();
    harness.run_steps(2);
    harness.get_by_label("Reset").click();
    harness.run_steps(2);
    assert_eq!(types(&harness), TypeSettings::default());
    pick(&mut harness, "Color of EOG channels", [10, 200, 30]);
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    type_options(&mut harness, "EOG channels");
    assert!(!reset_off(&harness));
    harness.get_by_label("Reset EOG").click();
    harness.run_steps(2);
    assert_eq!(types(&harness), TypeSettings::default());

    type_options(&mut harness, "Stim channels");
    harness.get_by_label("Reset Stim");
    assert!(harness.query_by_label("Remove DC of Stim").is_none());
    assert!(harness.query_by_label("Scale Each Stim Channel").is_none());
    assert!(!harness.state().channel_types_open());
    harness.get_by_label(ALL_TYPES).click();
    harness.run_steps(3);
    assert!(harness.state().channel_types_open());
    assert!(harness.query_by_label(ALL_TYPES).is_none());
}

/// The Channel Types window lists every type kymo knows, the recording's own and those set
/// before, marked "Built in" or "Custom"; its filter narrows them by name, other spelling
/// or mark; a box acts at once; Forget drops what was set for a custom type, which stays
/// listed while the recording has it and otherwise leaves the list; Esc closes it.
#[test]
fn the_channel_types_window_lists_filters_and_forgets() {
    use egui::accesskit::Role;
    // EEG, AUX and Gaze.
    let file = long_names_xdf();
    let mut harness = fonts_window(file.path(), options());
    ready(&mut harness);
    let joystick = ChannelKind::named("Joystick");
    let mut settings = harness.state().settings().clone();
    settings.types.set_remove_dc(&joystick, true);
    harness.state_mut().set_settings(settings);
    // From a type's popup in the sidebar.
    type_options(&mut harness, "EEG channels in Amp");
    harness.get_by_label(ALL_TYPES).click();
    harness.run_steps(10);
    let mark = |h: &Harness<'_, KymoApp>, name: &str| {
        let row = h
            .get_by_label(&format!("Color of {name}"))
            .rect()
            .center()
            .y;
        ["Built in", "Custom"]
            .into_iter()
            .find(|m| {
                h.query_all_by_role_and_label(Role::Label, m)
                    .any(|n| (n.rect().center().y - row).abs() < 1.0)
            })
            .unwrap_or_else(|| panic!("no mark in {name}'s row"))
    };
    assert_eq!(mark(&harness, "Temperature"), "Built in");
    assert_eq!(mark(&harness, "EEG"), "Built in");
    assert_eq!(mark(&harness, "AUX"), "Custom");
    assert_eq!(mark(&harness, "Gaze"), "Custom");
    assert_eq!(mark(&harness, "Joystick"), "Custom");
    assert!(harness.query_by_label("Reset Temperature").is_none());
    harness.get_by_label("Forget Joystick");
    assert!(harness.query_by_label("Forget AUX").is_none());

    let filter = |h: &mut Harness<'_, KymoApp>, text: &str| {
        let field = "Filter Channel Types";
        h.get_by_label(field).focus();
        h.run_steps(2);
        press(h, command(), Key::A);
        h.get_by_label(field).type_text(text);
        h.run_steps(2);
    };
    // The window's swatches, not the sidebar's ("Color of EEG channels in Amp").
    let listed = |h: &Harness<'_, KymoApp>| -> Vec<String> {
        h.query_all_by_label_contains("Color of ")
            .filter_map(|n| n.accesskit_node().label())
            .filter(|l| !l.contains(" channels"))
            .map(|l| l.trim_start_matches("Color of ").to_owned())
            .collect()
    };
    filter(&mut harness, "temp");
    assert_eq!(listed(&harness), ["Temperature"]);
    filter(&mut harness, "custom");
    assert_eq!(listed(&harness), ["AUX", "Gaze", "Joystick"]);
    filter(&mut harness, "nothing like it");
    assert_eq!(listed(&harness), Vec::<String>::new());
    harness.get_by_label("No channel types match.");

    // A custom type set: at once, and forgettable.
    filter(&mut harness, "aux");
    let aux = ChannelKind::named("AUX");
    harness.get_by_label("Remove DC of AUX").click();
    harness.run_steps(2);
    assert!(harness.state().settings().types.remove_dc(&aux));
    assert!(viewer(&harness).view().type_settings().remove_dc(&aux));
    harness.get_by_label("Forget AUX").click();
    harness.run_steps(2);
    assert!(!harness.state().settings().types.stored(&aux));
    assert_eq!(listed(&harness), ["AUX"]);
    filter(&mut harness, "joy");
    harness.get_by_label("Forget Joystick").click();
    harness.run_steps(2);
    assert_eq!(listed(&harness), Vec::<String>::new());
    assert_eq!(harness.state().settings().types, TypeSettings::default());

    // Esc closes it, the filter's focus first.
    for _ in 0..2 {
        harness.key_press(Key::Escape);
        harness.run_steps(3);
    }
    assert!(!harness.state().channel_types_open());
}

/// While the pointer is over the channel list, a button at the right end of the rule above
/// it folds all of its rows and then unfolds them; Alt-click on a chevron folds or unfolds
/// all of its level.
#[test]
fn all_channel_groups_fold_and_unfold_at_once() {
    let file = typed_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    let list = harness.get_by_label("Show A1").rect().center();
    // Only while the pointer is over the list.
    hover(&mut harness, egui::pos2(900.0, 300.0));
    assert!(harness.query_by_label_contains("All Channels").is_none());
    hover(&mut harness, list);
    let button = harness.get_by_label("Collapse All Channels").rect();
    let sidebar = viewer(&harness).layout().unwrap().names.left() - 8.0;
    assert!(
        button.right() <= sidebar && sidebar - button.right() < 20.0,
        "{button:?}"
    );
    // On the rule, clear of the first row.
    assert!(button.bottom() <= harness.get_by_label("Collapse EEG channels").rect().top());
    hover(&mut harness, button.center());
    assert!(tooltip(&mut harness, "Collapse All"));
    harness.get_by_label("Collapse All Channels").click();
    harness.run_steps(2);
    assert_eq!(folds(&harness), Vec::<String>::new());
    for name in ["A1", "EOG L", "Status"] {
        assert!(
            harness.query_by_label(&format!("Show {name}")).is_none(),
            "{name}"
        );
    }
    // The traces keep their channels.
    assert_eq!(shown(&harness), [0, 1, 2, 3, 4]);
    harness.get_by_label("Expand All Channels").click();
    harness.run_steps(2);
    assert_eq!(folds(&harness).len(), 4);
    harness.get_by_label("Show Status");

    // Alt-click on one type's chevron: all types.
    harness
        .get_by_label("Collapse EOG channels")
        .click_modifiers(Modifiers::ALT);
    harness.run_steps(2);
    assert_eq!(folds(&harness), Vec::<String>::new());
    harness
        .get_by_label("Expand Stim channels")
        .click_modifiers(Modifiers::ALT);
    harness.run_steps(2);
    assert_eq!(folds(&harness).len(), 4);
    // A plain click, just its own.
    harness.get_by_label("Collapse EOG channels").click();
    harness.run_steps(2);
    assert_eq!(folds(&harness).len(), 3);
}

/// Grouped by stream, Alt-click folds a level: the streams, or the types; the button folds
/// both and unfolds both.
#[test]
fn folding_all_reaches_streams_and_their_types() {
    // Amp (EEG and AUX types), Tracker.
    let file = long_names_xdf();
    let mut harness = window(file.path());
    ready(&mut harness);
    harness
        .get_by_label("Collapse AUX channels in Amp")
        .click_modifiers(Modifiers::ALT);
    harness.run_steps(2);
    assert_eq!(folds(&harness), ["Collapse Amp", "Collapse Tracker"]);
    harness
        .get_by_label("Collapse Tracker")
        .click_modifiers(Modifiers::ALT);
    harness.run_steps(2);
    assert_eq!(folds(&harness), Vec::<String>::new());
    harness
        .get_by_label("Expand Amp")
        .click_modifiers(Modifiers::ALT);
    harness.run_steps(2);
    // The streams open, their types as they were.
    assert_eq!(folds(&harness), ["Collapse Amp", "Collapse Tracker"]);
    harness.get_by_label("Expand EEG channels in Amp");

    let list = harness.get_by_label("Show Amp").rect().center();
    hover(&mut harness, list);
    harness.get_by_label("Collapse All Channels").click();
    harness.run_steps(2);
    assert_eq!(folds(&harness), Vec::<String>::new());
    harness.get_by_label("Expand All Channels").click();
    harness.run_steps(2);
    assert_eq!(folds(&harness).len(), 5);
}

/// A grouped legend folds all its streams at once, by the button on its top edge while the
/// pointer is over it, or by Alt-click on a stream's chevron.
#[test]
fn all_legend_streams_fold_and_unfold_at_once() {
    let file = marker_streams_xdf();
    let mut harness = window(file.path());
    ready(&mut harness);
    let open = |h: &Harness<'_, KymoApp>| {
        ["Keyboard", "Task"].map(|s| h.query_by_label(&format!("Show go in {s}")).is_some())
    };
    assert_eq!(open(&harness), [true, true]);
    let legend = harness.get_by_label("Show go in Task").rect().center();
    hover(&mut harness, legend);
    assert!(harness.query_by_label_contains("All Channels").is_none());
    let button = harness.get_by_label("Collapse All Annotations").rect();
    assert!(button.bottom() <= harness.get_by_label("Annotations").rect().top());
    harness.get_by_label("Collapse All Annotations").click();
    harness.run_steps(2);
    assert_eq!(open(&harness), [false, false]);
    harness.get_by_label("Expand All Annotations").click();
    harness.run_steps(2);
    assert_eq!(open(&harness), [true, true]);

    harness
        .get_by_label("Collapse Keyboard")
        .click_modifiers(Modifiers::ALT);
    harness.run_steps(2);
    assert_eq!(open(&harness), [false, false]);
    harness
        .get_by_label("Expand Task")
        .click_modifiers(Modifiers::ALT);
    harness.run_steps(2);
    assert_eq!(open(&harness), [true, true]);
    harness.get_by_label("Collapse Task").click();
    harness.run_steps(2);
    assert_eq!(open(&harness), [true, false]);
}

/// The legend's rows look as the channel list's: "Annotations" a group row with a chevron
/// in line with the types', its descriptions a level in, in line with the channels, rows
/// striped. Folding "Annotations" (its chevron or its name) shrinks the legend to that row,
/// which leaves the channel list more room.
#[test]
fn the_legend_lines_up_with_the_channel_list_and_folds() {
    use egui::accesskit::Role;
    let file = typed_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    let left = |h: &Harness<'_, KymoApp>, label: &str| h.get_by_label(label).rect().left();
    for (legend, list) in [
        ("Collapse Annotations", "Collapse EEG channels"),
        ("Show Annotations", "Show EEG channels"),
        ("Show trigger 0", "Show A1"),
    ] {
        assert_eq!(left(&harness, legend), left(&harness, list), "{legend}");
    }
    // Striped: every other row from the first description on.
    let faint = harness.ctx.global_style().visuals.faint_bg_color;
    let stripes = |h: &Harness<'_, KymoApp>, label: &str| {
        let y = h.get_by_label(label).rect().center().y;
        fn find(shape: &egui::Shape, color: egui::Color32, y: f32) -> bool {
            match shape {
                egui::Shape::Rect(r) => r.fill == color && r.rect.y_range().contains(y),
                egui::Shape::Vec(shapes) => shapes.iter().any(|s| find(s, color, y)),
                _ => false,
            }
        }
        h.output().shapes.iter().any(|c| find(&c.shape, faint, y))
    };
    assert!(stripes(&harness, "Show trigger 0"));
    assert!(!stripes(&harness, "Show trigger 1"));
    assert!(!stripes(&harness, "Show Annotations"));

    let top = |h: &Harness<'_, KymoApp>| h.get_by_label("Show Annotations").rect().top();
    let open = top(&harness);
    harness.get_by_label("Collapse Annotations").click();
    harness.run_steps(3);
    assert!(harness.query_by_label("Show trigger 0").is_none());
    let folded = top(&harness);
    assert!(folded > open + 100.0, "{open} {folded}");
    // Its checkbox still shows and hides them all.
    harness.get_by_label("Show Annotations").click();
    harness.run_steps(2);
    assert_eq!(viewer(&harness).legend().shown_count().0, 0);
    harness.get_by_label("Show Annotations").click();
    harness.run_steps(2);
    // A click on its name unfolds it.
    harness
        .get_by_role_and_label(Role::Label, "Annotations")
        .click();
    harness.run_steps(3);
    harness.get_by_label("Show trigger 0");
    assert_eq!(top(&harness), open);
}

/// The line between the channel list and the legend drags: the legend from one row to all
/// of them, the channel list keeping three; a double click puts it back; folding
/// "Annotations" still shrinks it to that row, and unfolding brings back the dragged height.
#[test]
fn the_line_above_the_legend_drags() {
    // 30 descriptions, 8 rows shown.
    let file = without_triggers("edf+d-mixed");
    let mut harness = window_with_short_frames(file.path());
    ready(&mut harness);
    let top = |h: &Harness<'_, KymoApp>| h.get_by_label("Show Annotations").rect().top();
    let stride = {
        let style = harness.ctx.global_style();
        style.spacing.interact_size.y + style.spacing.item_spacing.y
    };
    // The panel's top, its margin above "Annotations".
    let line = |h: &Harness<'_, KymoApp>| top(h) - 6.0;
    let x = harness.get_by_label("Show Annotations").rect().center().x + 60.0;
    let open = top(&harness);
    // Its cursor on hover.
    let at = egui::pos2(x, line(&harness));
    hover(&mut harness, at);
    harness.step();
    assert_eq!(
        harness.output().platform_output.cursor_icon,
        egui::CursorIcon::ResizeVertical
    );

    // Three rows taller.
    let from = egui::pos2(x, line(&harness));
    drag(&mut harness, from, from - egui::vec2(0.0, 3.0 * stride));
    harness.run_steps(2);
    assert!(
        (top(&harness) - (open - 3.0 * stride)).abs() <= 1.0,
        "{} {open}",
        top(&harness)
    );
    let taller = top(&harness);

    // Folded, one row; unfolded, the dragged height again.
    harness.get_by_label("Collapse Annotations").click();
    harness.run_steps(3);
    assert!(top(&harness) > open);
    harness.get_by_label("Expand Annotations").click();
    harness.run_steps(3);
    assert_eq!(top(&harness), taller);

    // As short as one row.
    let from = egui::pos2(x, line(&harness));
    drag(&mut harness, from, from + egui::vec2(0.0, 2000.0));
    harness.run_steps(2);
    let short = top(&harness);
    assert!(
        (short - (open + 7.0 * stride)).abs() <= 1.0,
        "{short} {open}"
    );

    // As tall as the channel list allows: three of its rows stay.
    let from = egui::pos2(x, line(&harness));
    drag(&mut harness, from, egui::pos2(x, 0.0));
    harness.run_steps(2);
    let list = harness.get_by_label("Collapse EEG channels").rect().top();
    assert!(
        top(&harness) >= list + 3.0 * stride,
        "{} {list}",
        top(&harness)
    );
    assert!(
        top(&harness) < list + 6.0 * stride,
        "{} {list}",
        top(&harness)
    );

    // A double click puts it back.
    let at = egui::pos2(x, line(&harness));
    double_click(&mut harness, at);
    harness.run_steps(2);
    assert_eq!(top(&harness), open);

    // Never taller than all its rows: two streams and four descriptions, all shown already.
    let file = marker_streams_xdf();
    let mut harness = window(file.path());
    ready(&mut harness);
    let open = top(&harness);
    let from = egui::pos2(x, line(&harness));
    drag(&mut harness, from, from - egui::vec2(0.0, 3.0 * stride));
    harness.run_steps(2);
    assert_eq!(top(&harness), open);
}

/// The labels of what collapses rows in the channel list.
fn folds(harness: &Harness<'_, KymoApp>) -> Vec<String> {
    harness
        .query_all_by_label_contains("Collapse ")
        .filter_map(|n| n.accesskit_node().label())
        .filter(|l| !l.starts_with("Collapse All") && l != "Collapse Annotations")
        .collect()
}

/// Nothing of streams shows for a recording without them, nor for one with a single stream
/// of channels (beside its marker stream, which the summary counts): only its types fold.
#[test]
fn recordings_without_several_streams_show_no_groups() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    assert!(harness.query_by_label("Streams").is_none());
    assert_eq!(
        folds(&harness),
        ["Collapse EEG channels", "Collapse Stim channels"]
    );
    // The types' rows have their rates, besides the summary's.
    let rates = |h: &Harness<'_, KymoApp>| h.query_all_by_label_contains(" Hz").count();
    assert!(rates(&harness) > 1);

    let file = TempFile::preset("xdf-float32");
    let mut harness = window(file.path());
    ready(&mut harness);
    harness.get_by_label("2 (1 shown as channels)");
    assert_eq!(folds(&harness), ["Collapse EEG channels"]);
    assert!(harness.query_by_label("Show BrainAmp").is_none());
    assert!(rates(&harness) > 1);
}

/// The view's channel order.
fn order(harness: &Harness<'_, KymoApp>) -> Vec<u32> {
    viewer(harness).view().order().iter().map(|c| c.0).collect()
}

/// The sidebar list's label `name` farthest left: a channel's name rather than a type
/// of the same text.
fn name_rect(harness: &Harness<'_, KymoApp>, name: &str) -> egui::Rect {
    harness
        .query_all_by_role_and_label(egui::accesskit::Role::Label, name)
        .map(|n| n.rect())
        .min_by(|a, b| a.left().total_cmp(&b.left()))
        .unwrap()
}

/// Whether some label text is selected.
fn has_selection(harness: &Harness<'_, KymoApp>) -> bool {
    let selection = harness
        .ctx
        .plugin::<egui::text_selection::LabelSelectionState>();
    selection.lock().has_selection()
}

/// Channels don't move: a drag on a name or a row's gray text moves nothing and selects no
/// text, and a click on a name shows it in the traces if it is off screen. A stream's row starts with a small
/// chevron, left of its checkbox.
#[test]
fn names_click_and_drag_nothing() {
    // A1, A2 (EEG), ECG, EOG L, Status: not in the types' order, so they open as EEG, EOG,
    // ECG, stim.
    let mut spec = kymo_testgen::Spec::preset("bdf+c-small").unwrap();
    spec.channels[2].label = "ECG".into();
    spec.channels[3].label = "EOG L".into();
    let file = TempFile::new(spec.format.extension());
    kymo_testgen::write(&spec, file.path()).unwrap();
    let mut harness = window(file.path());
    ready(&mut harness);
    assert_eq!(order(&harness), [0, 1, 3, 2, 4]);
    let (a, b) = (
        name_rect(&harness, "A2").center(),
        name_rect(&harness, "Status").center(),
    );
    drag(&mut harness, a, b);
    harness.run_steps(2);
    assert_eq!(order(&harness), [0, 1, 3, 2, 4]);
    assert!(!has_selection(&harness));
    let unit = harness
        .query_all_by_role_and_label(egui::accesskit::Role::Label, "\u{b5}V")
        .next()
        .unwrap()
        .rect()
        .center();
    drag(&mut harness, unit, b);
    harness.run_steps(2);
    assert_eq!(order(&harness), [0, 1, 3, 2, 4]);
    assert!(!has_selection(&harness));

    for _ in 0..2 {
        harness.key_press_modifiers(command() | Modifiers::SHIFT, Key::Minus);
        harness.step();
    }
    click_in_list(&mut harness, egui::accesskit::Role::Label, "Status");
    assert_eq!(viewer(&harness).view().top(), 2.0);

    let file = streams_xdf();
    let mut harness = window(file.path());
    ready(&mut harness);
    let chevron = harness.get_by_label("Collapse Amp").rect();
    assert!(chevron.width() <= 10.0, "{chevron:?}");
    assert!(chevron.right() <= harness.get_by_label("Show Amp").rect().left());
}

/// An XDF file of an EEG stream ("Amp": Fz, Cz, Pz at 10 Hz for 30 s) and two marker
/// streams sharing a description: "Keyboard" (id 2: go, stop, go) and "Task" (id 3: go,
/// rest).
fn marker_streams_xdf() -> TempFile {
    use kymo_testgen::XdfFormat;
    use kymo_testgen::xdf::{Chunks, header_xml};
    let amp: Vec<(String, String, String)> = ["Fz", "Cz", "Pz"]
        .iter()
        .map(|l| ((*l).to_owned(), "EEG".to_owned(), "microvolts".to_owned()))
        .collect();
    let mut c = Chunks::new(None);
    c.stream_header(1, &header_xml("Amp", "EEG", 3, 10.0, "float32", Some(&amp)));
    c.stream_header(
        2,
        &header_xml("Keyboard", "Markers", 1, 0.0, "string", None),
    );
    c.stream_header(3, &header_xml("Task", "Markers", 1, 0.0, "string", None));
    let samples: Vec<(Option<f64>, Vec<f64>)> = (0..300)
        .map(|i| (Some(f64::from(i) / 10.0), vec![f64::from(i % 7); 3]))
        .collect();
    c.numeric(1, XdfFormat::F32, &samples);
    let marker = |t: f64, text: &str| (Some(t), vec![text.to_owned()]);
    c.strings(
        2,
        &[marker(1.0, "go"), marker(4.0, "stop"), marker(9.0, "go")],
    );
    c.strings(3, &[marker(2.0, "go"), marker(6.0, "rest")]);
    for id in 1..=3 {
        c.footer(id, "<info/>");
    }
    let file = TempFile::new("xdf");
    std::fs::write(file.path(), &c.bytes).unwrap();
    file
}

/// A recording with several streams of annotations groups its legend by stream: a row
/// per stream shows and hides its descriptions, collapses their rows (by its chevron or
/// name), and counts its annotations; a description of two streams has a row in each.
/// Annotations without a stream (added or loaded) come last, under "Added"; they save and
/// load with their stream.
#[test]
fn several_annotation_streams_group_the_legend() {
    use egui::accesskit::{Role, Toggled};
    let file = marker_streams_xdf();
    let mut harness = window(file.path());
    ready(&mut harness);
    // Streams 1 (Keyboard) and 2 (Task), by index.
    let (keyboard, task) = (Some(1), Some(2));
    let entry = |h: &Harness<'_, KymoApp>, stream, d: &str| {
        let e = viewer(h).legend().entry(stream, d).unwrap().clone();
        (e.count, e.shown)
    };
    assert!(viewer(&harness).legend().by_stream());
    assert_eq!(entry(&harness, keyboard, "go"), (2, true));
    assert_eq!(entry(&harness, task, "go"), (1, true));
    let legend = viewer(&harness).legend();
    assert_eq!(
        (legend.stream_total(keyboard), legend.stream_total(task)),
        (3, 2)
    );
    // Each stream's row, then its descriptions', which line up with the stream's box.
    let top = |h: &Harness<'_, KymoApp>, label: &str| h.get_by_label(label).rect().top();
    let left = |h: &Harness<'_, KymoApp>, label: &str| h.get_by_label(label).rect().left();
    let order = [
        "Show Keyboard",
        "Show go in Keyboard",
        "Show stop in Keyboard",
        "Show Task",
        "Show go in Task",
        "Show rest in Task",
    ];
    for pair in order.windows(2) {
        assert!(top(&harness, pair[0]) < top(&harness, pair[1]), "{pair:?}");
    }
    // A stream a level in from "Annotations", its descriptions a level further.
    let level = left(&harness, "Show Keyboard") - left(&harness, "Show Annotations");
    assert!((level - LEVEL).abs() <= 0.5, "{level}");
    let level = left(&harness, "Show go in Keyboard") - left(&harness, "Show Keyboard");
    assert!((level - LEVEL).abs() <= 0.5, "{level}");
    assert!(left(&harness, "Collapse Keyboard") < left(&harness, "Show Keyboard"));
    let lines = |h: &Harness<'_, KymoApp>| viewer(h).marks().lines.len();
    assert_eq!(lines(&harness), 5);

    // A stream's box hides and shows its descriptions; mixed while some are shown.
    click_in_list(&mut harness, Role::CheckBox, "Show Keyboard");
    assert_eq!(entry(&harness, keyboard, "go"), (2, false));
    assert_eq!(entry(&harness, keyboard, "stop"), (1, false));
    assert_eq!(entry(&harness, task, "go"), (1, true));
    assert_eq!(lines(&harness), 2);
    assert_eq!(
        toggled(&harness, Role::CheckBox, "Show Annotations"),
        Toggled::Mixed
    );
    click_in_list(&mut harness, Role::CheckBox, "Show Keyboard");
    assert_eq!(lines(&harness), 5);
    click_in_list(&mut harness, Role::CheckBox, "Show go in Task");
    assert_eq!(entry(&harness, task, "go"), (1, false));
    assert_eq!(entry(&harness, keyboard, "go"), (2, true));
    assert_eq!(
        toggled(&harness, Role::CheckBox, "Show Task"),
        Toggled::Mixed
    );
    assert_eq!(
        toggled(&harness, Role::CheckBox, "Show Keyboard"),
        Toggled::True
    );
    click_in_list(&mut harness, Role::CheckBox, "Show Task");
    assert_eq!(entry(&harness, task, "go"), (1, true));

    // Its chevron, or its name, hides its descriptions' rows, not their marks; the name
    // has the type on hover.
    // Nothing moves: the legend keeps its height.
    let before = top(&harness, "Show Keyboard");
    harness.get_by_label("Collapse Keyboard").click();
    harness.run_steps(3);
    assert_eq!(top(&harness, "Show Keyboard"), before);
    assert!(harness.query_by_label("Show go in Keyboard").is_none());
    harness.get_by_label("Show go in Task");
    assert_eq!(lines(&harness), 5);
    harness.get_by_label("Expand Keyboard").click();
    harness.run_steps(2);
    harness.get_by_label("Show go in Keyboard");
    click_in_list(&mut harness, Role::Label, "Task");
    assert!(harness.query_by_label("Show rest in Task").is_none());
    let name = harness.get_by_role_and_label(Role::Label, "Task").rect();
    hover(&mut harness, name.center());
    assert!(tooltip(&mut harness, "Task\nMarkers"));
    click_in_list(&mut harness, Role::Label, "Task");
    harness.get_by_label("Show rest in Task");

    // The description menu has each description once.
    press(&mut harness, command(), Key::E);
    harness.get_by_label("Descriptions").click();
    harness.run_steps(2);
    assert_eq!(
        harness
            .query_all_by_role_and_label(Role::Button, "go")
            .count(),
        1
    );
    press(&mut harness, Modifiers::NONE, Key::Escape);
    // A description an annotation is given shows, in its stream: Keyboard's second "go"
    // becomes the hidden "stop" (undone after).
    click_in_list(&mut harness, Role::CheckBox, "Show stop in Keyboard");
    click_at_time(&mut harness, 9.0);
    harness.run_steps(2);
    assert_eq!(selected_data(&harness).unwrap().stream, keyboard);
    harness.get_by_label("Descriptions").click();
    harness.run_steps(2);
    click_in_list(&mut harness, Role::Button, "stop");
    harness.run_steps(2);
    assert_eq!(entry(&harness, keyboard, "stop"), (2, true));
    assert_eq!(selected_data(&harness).unwrap().stream, keyboard);
    press(&mut harness, command(), Key::Z);
    assert_eq!(entry(&harness, keyboard, "go"), (2, true));
    press(&mut harness, Modifiers::NONE, Key::Escape);

    // An annotation added in the viewer has no stream: "Added", last.
    describe(&mut harness, "go");
    let (a, b) = (at_time(&harness, 12.0, 40.0), at_time(&harness, 13.0, 40.0));
    drag(&mut harness, a, b);
    assert_eq!(entry(&harness, None, "go"), (1, true));
    assert!(top(&harness, "Show rest in Task") < top(&harness, "Show Added"));
    harness.get_by_label("Show go in Added");
    assert_eq!(entry(&harness, keyboard, "go"), (2, true));

    // Saved with their streams' ids, and loaded back into them: ids 2 and 3 are Keyboard
    // and Task, stream 9 isn't in the file.
    let mut out = Vec::new();
    viewer(&harness).write_annotations(&mut out, false).unwrap();
    let text = String::from_utf8(out).unwrap();
    let mut lines_of: Vec<(String, String)> = text
        .lines()
        .skip(1)
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f[2].to_owned(), f[3].to_owned())
        })
        .collect();
    lines_of.sort();
    assert_eq!(
        text.lines().next(),
        Some("onset\tduration\ttrial_type\tstream_id")
    );
    let own = |d: &str, s: &str| (d.to_owned(), s.to_owned());
    assert_eq!(
        lines_of,
        [
            own("go", "2"),
            own("go", "2"),
            own("go", "3"),
            own("go", "n/a"),
            own("rest", "3"),
            own("stop", "2"),
        ]
    );
    let table = TempFile::new("tsv");
    std::fs::write(
        table.path(),
        "onset\tduration\ttrial_type\tstream_id\n20\t0\trest\t3\n21\t0\tstop\t9\n",
    )
    .unwrap();
    assert!(harness.state_mut().load_annotations_from(table.path()));
    harness.run_steps(2);
    assert_eq!(entry(&harness, task, "rest"), (2, true));
    assert_eq!(entry(&harness, None, "stop"), (1, true));
    assert_eq!(entry(&harness, keyboard, "stop"), (1, true));

    // Only annotations of a stream merge: a copy of Keyboard's first "go", and one
    // without a stream, at the same time.
    std::fs::write(
        table.path(),
        "onset\tduration\ttrial_type\tstream_id\n1\t0\tgo\t2\n1\t0\tgo\tn/a\n",
    )
    .unwrap();
    assert!(harness.state_mut().load_annotations_from(table.path()));
    harness.run_steps(2);
    assert_eq!(entry(&harness, keyboard, "go"), (3, true));
    choose(&mut harness, "Edit", "Merge Annotations");
    assert_eq!(entry(&harness, keyboard, "go"), (2, true));
    assert_eq!(entry(&harness, None, "go"), (2, true));
}

/// A recording with one stream of annotations, or none, has the legend as before: no
/// stream rows, and an added annotation joins the file's of its description.
#[test]
fn one_annotation_stream_keeps_the_legend_flat() {
    let file = streams_xdf();
    let mut harness = window(file.path());
    ready(&mut harness);
    assert!(!viewer(&harness).legend().by_stream());
    assert!(harness.query_by_label("Collapse Markers").is_none());
    harness.get_by_label("Show go");
    press(&mut harness, command(), Key::E);
    describe(&mut harness, "go");
    let (a, b) = (at_time(&harness, 12.0, 40.0), at_time(&harness, 13.0, 40.0));
    drag(&mut harness, a, b);
    assert_eq!(
        viewer(&harness).legend().entry(None, "go").unwrap().count,
        2
    );
    assert!(harness.query_by_label("Show Added").is_none());
    // An edit merges it with the file's, as the stream doesn't count.
    let (a, b) = (at_time(&harness, 0.5, 40.0), at_time(&harness, 1.5, 40.0));
    drag(&mut harness, a, b);
    assert_eq!(
        viewer(&harness).legend().entry(None, "go").unwrap().count,
        2
    );
}

/// The legend groups by stream where more than one stream holds annotations: streams of
/// annotations, streams with a stim channel, and streams the file's annotations are in.
#[test]
fn legends_group_where_several_streams_hold_annotations() {
    use kymo_testgen::XdfFormat;
    use kymo_testgen::xdf::{Chunks, header_xml};
    let groups = |path: &Path| {
        let (rec, annotations) = kymo_core::Recording::new(kymo_core::open(path).unwrap()).unwrap();
        let store = kymo_core::AnnotationStore::from_data(annotations);
        kymo::annotations::groups_streams(&rec, &store)
    };
    assert!(groups(marker_streams_xdf().path()));
    assert!(!groups(streams_xdf().path()));
    assert!(!groups(TempFile::preset("bdf+c-small").path()));
    // BVRF participants whose markers are their own, beside those of all of them; markers
    // of all participants alone don't group, nor does a file of one participant.
    assert!(groups(TempFile::preset("bvrf-participants").path()));
    assert!(!groups(TempFile::preset("bvrf-int16").path()));
    let mut spec = kymo_testgen::Spec::preset("bvrf-participants").unwrap();
    spec.bvrf.targets.clear();
    assert!(!groups(TempFile::written(&spec).path()));
    // A marker stream and a stream of trigger channels, with or without the markers.
    let file = |markers: bool| {
        let mut c = Chunks::new(None);
        c.stream_header(1, &header_xml("Amp", "EEG", 1, 10.0, "float32", None));
        c.stream_header(
            2,
            &header_xml("Buttons", "Triggers", 1, 10.0, "float32", None),
        );
        let samples: Vec<(Option<f64>, Vec<f64>)> = (0..100)
            .map(|i| (Some(f64::from(i) / 10.0), vec![f64::from(i / 10 % 2)]))
            .collect();
        c.numeric(1, XdfFormat::F32, &samples);
        c.numeric(2, XdfFormat::F32, &samples);
        if markers {
            c.stream_header(3, &header_xml("Task", "Markers", 1, 0.0, "string", None));
            c.strings(3, &[(Some(1.0), vec!["go".into()])]);
        }
        let file = TempFile::new("xdf");
        std::fs::write(file.path(), &c.bytes).unwrap();
        file
    };
    assert!(groups(file(true).path()));
    assert!(!groups(file(false).path()));
}

/// The shortcut overview stays as wide as its rows and scroll bar on every platform, even
/// after it was wider once (here with a larger font for a few frames): a window doesn't
/// shrink by itself, which left a strip right of the scroll bar.
#[test]
fn the_shortcut_overview_shrinks_back_to_its_rows() {
    use kymo::keymap::Platform;
    // How far About's close button is from its window's right edge.
    let about_close = {
        let harness = Harness::builder()
            .with_size(SIZE)
            .build_ui(|ui| kymo::ui::menus::about_window(ui.ctx(), &mut true));
        let window = harness
            .ctx
            .memory(|m| m.area_rect(kymo::ui::Overlay::About.id()))
            .unwrap();
        window.right() - harness.get_by_label("Close window").rect().right()
    };
    for platform in Platform::ALL {
        let mut builder = Harness::builder()
            .with_size(SIZE)
            .with_context_setup(|ctx| {
                system_fonts(ctx);
                kymo::ui::fonts::apply_fonts(ctx, false);
            });
        // The app draws only its own platform's shortcuts; another's symbols (⌫ for the
        // Mac) may be in no font here, as on Windows, and a box is as good for the widths.
        if platform != Platform::current() {
            builder = builder.allow_missing_glyphs();
        }
        let mut harness = builder
            .build_ui(move |ui| kymo::ui::menus::shortcuts_window(ui.ctx(), &mut true, platform));
        let body = |h: &Harness<'_>| h.ctx.global_style().text_styles[&egui::TextStyle::Body].size;
        let normal = body(&harness);
        harness.ctx.all_styles_mut(|s| {
            s.text_styles.get_mut(&egui::TextStyle::Body).unwrap().size = normal * 1.5;
        });
        harness.run_steps(3);
        let wide = harness
            .ctx
            .memory(|m| m.area_rect(kymo::ui::Overlay::Shortcuts.id()))
            .unwrap();
        harness.ctx.all_styles_mut(|s| {
            s.text_styles.get_mut(&egui::TextStyle::Body).unwrap().size = normal;
        });
        harness.run_steps(3);
        let window = harness
            .ctx
            .memory(|m| m.area_rect(kymo::ui::Overlay::Shortcuts.id()))
            .unwrap();
        assert!(window.width() < wide.width(), "{platform:?}");
        let keys = harness.get_by_label("Scroll up or down").rect();
        let scroll = harness.ctx.global_style().spacing.scroll;
        let room = scroll.bar_width + scroll.bar_inner_margin;
        let stroke = harness.ctx.global_style().visuals.window_stroke.width;
        let right = window.right() - stroke;
        assert!(
            (keys.right() + 6.0 + room - right).abs() <= 1.0,
            "{platform:?}: keys end at {}, the window at {right}, room {room}",
            keys.right()
        );
        // The title bar is as other windows' (About's): its close button as far from the
        // corner, not moved in by the scroll bar's room.
        let close = harness.get_by_label("Close window").rect();
        assert!(
            (window.right() - close.right() - about_close).abs() <= 0.5,
            "{platform:?}: close button ends at {}, the window at {}, About's {about_close} in",
            close.right(),
            window.right()
        );
    }
}

/// An XDF file of three streams of channels: "Acc" and "Gyro" (X and Y each, so their
/// labels get the stream's name) for 30 s, and "Pos", without labels (Pos_0, Pos_1), from
/// 5 s to 25 s.
fn qualified_xdf() -> TempFile {
    use kymo_testgen::XdfFormat;
    use kymo_testgen::xdf::{Chunks, header_xml};
    let axes: Vec<(String, String, String)> = ["X", "Y"]
        .iter()
        .map(|l| ((*l).to_owned(), "Misc".to_owned(), String::new()))
        .collect();
    let mut c = Chunks::new(None);
    c.stream_header(
        1,
        &header_xml("Acc", "Motion", 2, 10.0, "float32", Some(&axes)),
    );
    c.stream_header(
        2,
        &header_xml("Gyro", "Motion", 2, 10.0, "float32", Some(&axes)),
    );
    c.stream_header(3, &header_xml("Pos", "Position", 2, 10.0, "float32", None));
    let samples = |from: f64, n: u32| -> Vec<(Option<f64>, Vec<f64>)> {
        (0..n)
            .map(|i| (Some(from + f64::from(i) / 10.0), vec![f64::from(i % 5); 2]))
            .collect()
    };
    c.numeric(1, XdfFormat::F32, &samples(0.0, 300));
    c.numeric(2, XdfFormat::F32, &samples(0.0, 300));
    c.numeric(3, XdfFormat::F32, &samples(5.0, 200));
    for id in 1..=3 {
        c.footer(id, "<info/>");
    }
    let file = TempFile::new("xdf");
    std::fs::write(file.path(), &c.bytes).unwrap();
    file
}

/// Grouped by stream, the channel list names a channel as its stream has it: "X" for
/// "Acc X", "0" for an unlabeled "Pos_0"; the full name on hover, for screen readers and in
/// the traces. A stream's row gives its span on hover. Ungrouped, names stay whole.
#[test]
fn channels_are_named_within_their_streams() {
    use egui::accesskit::Role;
    let file = qualified_xdf();
    let mut harness = window(file.path());
    ready(&mut harness);
    let names: Vec<&str> = viewer(&harness)
        .document()
        .recording
        .channels()
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["Acc X", "Acc Y", "Gyro X", "Gyro Y", "Pos_0", "Pos_1"]
    );
    // The list paints the short names; screen readers get the full ones.
    let list_texts = |h: &Harness<'_, KymoApp>, text: &str| {
        painted(h, text).iter().filter(|t| *t == text).count()
    };
    assert_eq!(list_texts(&harness, "X"), 2);
    assert_eq!(list_texts(&harness, "0"), 1);
    let x = harness.get_by_role_and_label(Role::Label, "Acc X").rect();
    let gyro_x = harness.get_by_role_and_label(Role::Label, "Gyro X").rect();
    let pos_0 = harness.get_by_role_and_label(Role::Label, "Pos_0").rect();
    let sidebar = viewer(&harness).layout().unwrap().names.left() - 8.0;
    for rect in [x, gyro_x, pos_0] {
        assert!(rect.right() < sidebar, "{rect:?}");
    }
    // The names' column is as wide as the short names: "Bad" follows closer
    // than the full names would fit.
    let full_width = {
        let font = egui::TextStyle::Body.resolve(&harness.ctx.global_style());
        harness.ctx.fonts_mut(|f| {
            f.layout_no_wrap("Gyro X".to_owned(), font, egui::Color32::WHITE)
                .size()
                .x
        })
    };
    mark_in_list(&mut harness, "Gyro X", true);
    let bad = harness.get_by_label("Gyro X bad").rect();
    assert!(
        bad.left() - gyro_x.left() < full_width,
        "{bad:?} {gyro_x:?}"
    );
    // The traces keep the full names.
    assert!(painted(&harness, "Acc X").iter().any(|t| t == "Acc X"));
    assert!(painted(&harness, "Pos_1").iter().any(|t| t == "Pos_1"));
    // The full name on hover.
    hover(&mut harness, x.center());
    assert!(tooltip(&mut harness, "Acc X\nMisc"));
    hover(&mut harness, egui::pos2(900.0, 650.0));
    // A stream's span on hover.
    let pos = harness.get_by_role_and_label(Role::Label, "Pos").rect();
    hover(&mut harness, pos.center());
    assert!(tooltip(&mut harness, "from 5.0 s to 25.0 s"));
    hover(&mut harness, egui::pos2(900.0, 650.0));
    let acc = harness.get_by_role_and_label(Role::Label, "Acc").rect();
    hover(&mut harness, acc.center());
    assert!(tooltip(&mut harness, "from 0.0 s to 30.0 s"));

    // One stream of unlabeled channels: not grouped, the names whole.
    use kymo_testgen::XdfFormat;
    use kymo_testgen::xdf::{Chunks, header_xml};
    let mut c = Chunks::new(None);
    c.stream_header(1, &header_xml("Pos", "Position", 2, 10.0, "float32", None));
    let samples: Vec<(Option<f64>, Vec<f64>)> = (0..100)
        .map(|i| (Some(f64::from(i) / 10.0), vec![1.0, 2.0]))
        .collect();
    c.numeric(1, XdfFormat::F32, &samples);
    c.footer(1, "<info/>");
    let single = TempFile::new("xdf");
    std::fs::write(single.path(), &c.bytes).unwrap();
    let mut harness = window(single.path());
    ready(&mut harness);
    assert_eq!(list_texts(&harness, "0"), 0);
    assert!(
        painted(&harness, "Pos_0")
            .iter()
            .filter(|t| *t == "Pos_0")
            .count()
            >= 2
    );
}

/// Two EEG channels, Cz and Pz, at 20 Hz for a minute: 20 s on screen draw every sample.
fn slow_preset() -> TempFile {
    use kymo_testgen::{ChannelSpec, Format, Spec};
    let channels = ["Cz", "Pz"]
        .map(|label| ChannelSpec::eeg(label, 20, Format::Edf))
        .to_vec();
    TempFile::written(&Spec::new(Format::Edf, channels, 1.0, 60))
}

/// A ⇧-drag from `from` to `to`, through the middle.
fn shift_drag(harness: &mut Harness<'_, KymoApp>, from: egui::Pos2, to: egui::Pos2) {
    harness.event(egui::Event::ModifiersChanged(Modifiers::SHIFT));
    drag(harness, from, to);
    harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    harness.step();
}

/// The middle of the first row's height, from the traces' top.
fn first_row(harness: &Harness<'_, KymoApp>) -> f32 {
    let traces = viewer(harness).layout().unwrap().traces;
    0.5 * traces.height() / viewer(harness).view().rows() as f32
}

/// The text of an accessibility node.
fn node_text(node: &egui_kittest::Node<'_>) -> String {
    node.accesskit_node()
        .value()
        .or_else(|| node.accesskit_node().label())
        .unwrap_or_default()
        .to_owned()
}

/// The measurement label's two lines: its interval, and its channel with the amplitude.
fn measurement_label(harness: &Harness<'_, KymoApp>) -> Option<[String; 2]> {
    use egui::accesskit::Role;
    let values = harness.query_by_label_contains(" PTP")?;
    let below = values.rect();
    // The line right above it, from the same left edge.
    let interval = harness.query_all_by_role(Role::Label).find(|n| {
        let r = n.rect();
        (r.bottom() - below.top()).abs() < 1.0 && (r.left() - below.left()).abs() < 1.0
    })?;
    Some([node_text(&interval), node_text(&values)])
}

/// The measurement's label once its amplitude is known, as tiles may still be arriving.
fn settled_measurement(harness: &mut Harness<'_, KymoApp>) -> [String; 2] {
    let start = Instant::now();
    loop {
        if let Some(label) = measurement_label(harness)
            && !label[1].contains(kymo::measure::NO_READING)
        {
            return label;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for the measurement's amplitude: {:?}",
            measurement_label(harness)
        );
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The status line while the pointer is at `pos` (in the band: the measurement's details),
/// once both ends read.
fn settled_details(harness: &mut Harness<'_, KymoApp>, pos: egui::Pos2) -> String {
    hover(harness, pos);
    let start = Instant::now();
    loop {
        let left = viewer(harness).status().left.clone();
        let values = left.rsplit(" \u{b7} ").next().unwrap_or_default();
        if !values.contains(kymo::measure::NO_READING) {
            return left;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for the measurement's readings: {left}"
        );
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The numbers of the readings part of a measurement's details ("1.5 µV → −2 µV, Δ
/// −3.5 µV"), with the unit of the first.
fn measured_numbers(details: &str) -> (Vec<f64>, String) {
    let values = details.rsplit(" \u{b7} ").next().unwrap();
    let mut unit = String::new();
    let numbers = values
        .split([' ', ',', '\u{2192}', '\u{394}', '\u{2026}'])
        .filter(|part| !part.is_empty())
        .filter_map(|part| match part.replace('\u{2212}', "-").parse() {
            Ok(x) => Some(x),
            Err(_) => {
                if unit.is_empty() {
                    unit = part.to_owned();
                }
                None
            }
        })
        .collect();
    (numbers, unit)
}

/// The number in a label's "Cz 86.40 µV PTP".
fn peak_to_peak(line: &str) -> f64 {
    let number = line.split(' ').nth(1).unwrap();
    number.replace('\u{2212}', "-").parse().unwrap()
}

/// The galleys painted in the last frame whose text contains `needle`.
fn painted_galleys(
    harness: &Harness<'_, KymoApp>,
    needle: &str,
) -> Vec<std::sync::Arc<egui::Galley>> {
    fn galleys(shape: &egui::Shape, needle: &str, out: &mut Vec<std::sync::Arc<egui::Galley>>) {
        match shape {
            egui::Shape::Text(t) if t.galley.text().contains(needle) => out.push(t.galley.clone()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| galleys(s, needle, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in &harness.output().shapes {
        galleys(&clipped.shape, needle, &mut out);
    }
    out
}

/// The centers of the small dots painted over the traces in the last frame: where the
/// measurement's ends cross its trace.
fn painted_dots(harness: &Harness<'_, KymoApp>) -> Vec<egui::Pos2> {
    fn dots(shape: &egui::Shape, out: &mut Vec<egui::Pos2>) {
        match shape {
            egui::Shape::Circle(c) if c.radius < 4.0 => out.push(c.center),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| dots(s, out)),
            _ => {}
        }
    }
    let traces = viewer(harness).layout().unwrap().traces;
    let mut out = Vec::new();
    for clipped in &harness.output().shapes {
        dots(&clipped.shape, &mut out);
    }
    out.retain(|p| traces.contains(*p));
    out
}

/// A channel's physical samples `range`, brute force from the file.
fn samples_of(
    harness: &Harness<'_, KymoApp>,
    id: kymo_core::ChannelId,
    range: std::ops::Range<u64>,
) -> Vec<f64> {
    let rec = &viewer(harness).document().recording;
    let mut out = vec![0.0f32; (range.end - range.start) as usize];
    rec.read_physical(rec.channel(id).grid, &[id], range, &mut out)
        .unwrap();
    out.into_iter().map(f64::from).collect()
}

#[test]
fn shift_dragging_measures_time_and_values() {
    let file = slow_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    dc_off(&mut harness);
    let start = viewer(&harness).view().time();
    let dy = first_row(&harness);
    // Off the samples, as a pointer is: 2.05 s and 5.25 s are the nearest, 64 samples
    // apart. Leftward, to show the order doesn't matter.
    let from = at_time(&harness, 5.249, dy);
    let to = at_time(&harness, 2.051, dy);
    shift_drag(&mut harness, from, to);
    let measurement = viewer(&harness).measurement().unwrap();
    let id = kymo_core::ChannelId(0);
    assert_eq!(measurement.channel, id);
    // It doesn't pan.
    assert_eq!(viewer(&harness).view().time(), start);
    let [interval, values] = settled_measurement(&mut harness);
    assert_eq!(interval, "3.20 s 0.312 Hz");
    // Cz's peak-to-peak over samples 41 to 105, in the readout's unit.
    let (unit, factor) = {
        let shown = viewer(&harness);
        kymo::scale_bar::reading_unit(&shown.document().recording, shown.view(), id)
    };
    let samples = samples_of(&harness, id, 41..106);
    let (lo, hi) = samples
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &v| {
            (l.min(v), h.max(v))
        });
    let close = |got: f64, want: f64| (got - want).abs() <= 1e-3 * want.abs().max(1e-9);
    assert!(
        values.starts_with("Cz ") && values.ends_with(" PTP"),
        "{values}"
    );
    assert!(values.ends_with(&format!(" {unit} PTP")), "{values}");
    let pp = peak_to_peak(&values);
    assert!(
        close(pp, (hi - lo) * factor),
        "{values}: {pp} vs {}",
        (hi - lo) * factor
    );

    // The details in the status line while the pointer is over the band: the ends, the
    // samples, Cz's samples 41 and 105 and their difference.
    let at = at_time(&harness, 3.0, dy);
    let details = settled_details(&mut harness, at);
    assert!(
        details.starts_with("Cz 0:02.05 to 0:05.25 \u{b7} 3.20 s (64 samples at 20 Hz) \u{b7} "),
        "{details}"
    );
    let (a, b) = (samples[0] * factor, samples[64] * factor);
    let (numbers, shown_unit) = measured_numbers(&details);
    assert_eq!(shown_unit, unit);
    assert_eq!(numbers.len(), 3, "{details}");
    for (got, want) in numbers.iter().zip([a, b, b - a]) {
        // Four significant digits.
        assert!(close(*got, want), "{details}: {got} vs {want}");
    }
    // Not beside it: the reading under the pointer, as ever.
    let at = at_time(&harness, 8.0, dy);
    hover(&mut harness, at);
    let left = viewer(&harness).status().left.clone();
    assert!(!left.contains("samples at"), "{left}");
    assert!(left.contains("Cz"), "{left}");

    // With DC removal, as drawn: the ends from the window's mean; the difference and the
    // amplitude stay.
    press(&mut harness, command(), Key::D);
    let baseline = viewer(&harness).baseline(id) * factor;
    assert!(baseline.abs() > 0.01, "{baseline}");
    let at = at_time(&harness, 3.0, dy);
    let details = settled_details(&mut harness, at);
    let (numbers, _) = measured_numbers(&details);
    for (got, want) in numbers.iter().zip([a - baseline, b - baseline, b - a]) {
        assert!(close(*got, want), "{details}: {got} vs {want}");
    }
    let [_, values] = settled_measurement(&mut harness);
    assert!(close(peak_to_peak(&values), pp), "{values}");
}

#[test]
fn the_amplitude_is_in_the_readouts_unit() {
    // Pz stored in mV, shown in µV as EEG is.
    use kymo_testgen::{ChannelSpec, Format, Spec};
    let cz = ChannelSpec::eeg("Cz", 20, Format::Edf);
    let mut pz = ChannelSpec::eeg("Pz", 20, Format::Edf);
    pz.dimension = "mV".into();
    pz.physical_min /= 1000.0;
    pz.physical_max /= 1000.0;
    pz.amplitude /= 1000.0;
    let file = TempFile::written(&Spec::new(Format::Edf, vec![cz, pz], 1.0, 60));
    let mut harness = window(file.path());
    ready(&mut harness);
    let pz = kymo_core::ChannelId(1);
    let row = viewer(&harness).view().row_of(pz).unwrap() as f32;
    let dy = first_row(&harness) * (2.0 * row + 1.0);
    let (from, to) = (at_time(&harness, 2.05, dy), at_time(&harness, 5.25, dy));
    shift_drag(&mut harness, from, to);
    assert_eq!(viewer(&harness).measurement().unwrap().channel, pz);
    let [_, values] = settled_measurement(&mut harness);
    assert!(
        values.starts_with("Pz ") && values.ends_with(" PTP"),
        "{values}"
    );
    assert!(values.ends_with(" \u{b5}V PTP"), "{values}");
    let samples = samples_of(&harness, pz, 41..106);
    let (lo, hi) = samples
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &v| {
            (l.min(v), h.max(v))
        });
    let want = (hi - lo) * 1000.0;
    let got = peak_to_peak(&values);
    assert!(
        (got - want).abs() <= 1e-3 * want,
        "{values}: {got} vs {want}"
    );
}

#[test]
fn the_amplitude_asks_for_its_own_tiles() {
    // Zoomed out, the band's edges need finer tiles than the view's; with the cache
    // cleared, only the measurement asks for them again.
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::Num9);
    step_until(&mut harness, "the tiles", |app| {
        app.viewer().unwrap().complete()
    });
    let dy = first_row(&harness);
    let (from, to) = (at_time(&harness, 3.3, dy), at_time(&harness, 11.7, dy));
    shift_drag(&mut harness, from, to);
    let [_, before] = settled_measurement(&mut harness);
    viewer(&harness).cache().clear();
    harness.step();
    let [_, cleared] = measurement_label(&harness).unwrap();
    assert!(
        cleared.ends_with(&format!(" {} PTP", kymo::measure::NO_READING)),
        "{cleared}"
    );
    let [_, after] = settled_measurement(&mut harness);
    assert_eq!(after, before);
}

#[test]
fn the_label_keeps_its_lines_whole_as_it_grows() {
    let file = slow_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    let dy = first_row(&harness);
    let traces = viewer(&harness).layout().unwrap().traces;
    let lines_whole = |harness: &Harness<'_, KymoApp>| {
        let [interval, values] = measurement_label(harness).expect("a label");
        for line in [&interval, &values] {
            let galleys = painted_galleys(harness, line);
            assert!(!galleys.is_empty(), "{line} not painted");
            for g in galleys {
                assert_eq!(g.rows.len(), 1, "{line} wrapped: {:?}", g.text());
            }
        }
        let label = harness.query_by_label_contains(" PTP").unwrap().rect();
        assert!(traces.contains_rect(label), "{label:?} beyond {traces:?}");
    };
    // Short at first, near the right edge, then long: the label laid out for the short
    // text grows rather than wrapping.
    let from = at_time(&harness, 19.0, dy);
    harness.event(egui::Event::ModifiersChanged(Modifiers::SHIFT));
    hover(&mut harness, from);
    harness.drag_at(from);
    harness.step();
    let at = at_time(&harness, 18.8, dy);
    hover(&mut harness, at);
    harness.run_steps(3);
    lines_whole(&harness);
    for t in [12.0, 4.0, 0.5] {
        let at = at_time(&harness, t, dy);
        hover(&mut harness, at);
        harness.run_steps(3);
        lines_whole(&harness);
    }
    harness.drop_at(at_time(&harness, 0.5, dy));
    harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    harness.step();
    settled_measurement(&mut harness);
    harness.run_steps(2);
    lines_whole(&harness);
    // While dragging and after, the details are in the status line only over the band.
    let left = viewer(&harness).status().left.clone();
    assert!(!left.contains("samples at"), "{left}");
}

#[test]
fn dots_mark_where_the_ends_cross_the_trace() {
    let file = slow_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    dc_off(&mut harness);
    let dy = first_row(&harness);
    assert!(painted_dots(&harness).is_empty());
    let (from, to) = (at_time(&harness, 2.05, dy), at_time(&harness, 5.25, dy));
    shift_drag(&mut harness, from, to);
    settled_measurement(&mut harness);
    harness.run_steps(2);
    let mut dots = painted_dots(&harness);
    dots.sort_by(|p, q| p.x.total_cmp(&q.x));
    assert_eq!(dots.len(), 2, "{dots:?}");
    for (dot, end) in dots.iter().zip([from, to]) {
        assert!((dot.x - end.x).abs() <= 1.0, "{dot:?} vs {end:?}");
    }
    // As far apart as Cz's samples 41 and 105 are drawn, larger values higher.
    let id = kymo_core::ChannelId(0);
    let samples = samples_of(&harness, id, 41..106);
    let shown = viewer(&harness);
    let traces = shown.layout().unwrap().traces;
    let row_height = f64::from(traces.height()) / shown.view().rows() as f64;
    let per_unit = row_height / (2.0 * shown.view().channel_amplitude(id));
    let want = (samples[0] - samples[64]) * per_unit;
    let got = f64::from(dots[1].y - dots[0].y);
    assert!((got - want).abs() < 0.5, "{got} vs {want}");
    // Gone with the measurement.
    press(&mut harness, Modifiers::NONE, Key::Escape);
    harness.run_steps(2);
    assert!(painted_dots(&harness).is_empty());

    // Holding ⇧ shows where a measurement would start: on the trace at the sample nearest
    // the pointer (sample 41 at 2.05 s), following it, gone with ⇧.
    let at = at_time(&harness, 2.074, dy);
    hover(&mut harness, at);
    assert!(painted_dots(&harness).is_empty());
    harness.event(egui::Event::ModifiersChanged(Modifiers::SHIFT));
    harness.run_steps(2);
    let start = painted_dots(&harness);
    assert_eq!(start.len(), 1, "{start:?}");
    assert!((start[0].x - from.x).abs() <= 0.3, "{start:?} vs {from:?}");
    assert!(
        (start[0].y - dots[0].y).abs() < 0.5,
        "{start:?} vs {dots:?}"
    );
    let at = at_time(&harness, 5.226, dy);
    hover(&mut harness, at);
    harness.run_steps(2);
    let moved = painted_dots(&harness);
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert!((moved[0].x - to.x).abs() <= 0.3 && (moved[0].y - dots[1].y).abs() < 0.5);
    harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    harness.run_steps(2);
    assert!(painted_dots(&harness).is_empty());
}

#[test]
fn measuring_in_annotation_mode_creates_no_annotation() {
    let file = slow_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::E);
    assert!(viewer(&harness).editing());
    let dy = first_row(&harness);
    let (from, to) = (at_time(&harness, 2.0, dy), at_time(&harness, 6.0, dy));
    shift_drag(&mut harness, from, to);
    assert!(made(&harness).is_empty());
    let m = viewer(&harness).measurement().unwrap();
    assert_eq!((m.press.round(), m.reach.round()), (2.0, 6.0));
    assert!(measurement_label(&harness).is_some());
    // A plain drag still creates one; the measurement stays.
    let (from, to) = (at_time(&harness, 8.0, dy), at_time(&harness, 9.0, dy));
    drag(&mut harness, from, to);
    assert_eq!(made(&harness).len(), 1);
    assert!(viewer(&harness).measurement().is_some());
    // A click removes it, and selects as ever.
    click_at_time(&mut harness, 8.5);
    assert!(viewer(&harness).measurement().is_none());
    assert!(measurement_label(&harness).is_none());
    assert!(viewer(&harness).selected().is_some());
}

#[test]
fn esc_and_clicks_remove_the_measurement_and_drags_keep_it() {
    use egui::accesskit::Role;
    let file = slow_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    let dy = first_row(&harness);
    let measure = |harness: &mut Harness<'_, KymoApp>| {
        let (from, to) = (at_time(harness, 4.0, dy), at_time(harness, 7.0, dy));
        shift_drag(harness, from, to);
        assert!(viewer(harness).measured());
    };
    measure(&mut harness);
    // Too short a ⇧-drag measures nothing, and takes the old one away: out and back to
    // a point from where it started (a press that doesn't move is a click).
    let from = at_time(&harness, 10.0, dy);
    harness.event(egui::Event::ModifiersChanged(Modifiers::SHIFT));
    hover(&mut harness, from);
    harness.drag_at(from);
    harness.step();
    hover(&mut harness, from + egui::vec2(30.0, 0.0));
    assert!(viewer(&harness).measurement().is_some());
    hover(&mut harness, from + egui::vec2(1.0, 0.0));
    assert!(viewer(&harness).measurement().is_none());
    harness.drop_at(from + egui::vec2(1.0, 0.0));
    harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    harness.step();
    assert!(!viewer(&harness).measured());

    // A plain drag pans, the band staying at its times.
    measure(&mut harness);
    let traces = viewer(&harness).layout().unwrap().traces;
    let from = traces.center();
    drag(
        &mut harness,
        from,
        from - egui::vec2(traces.width() / 10.0, 0.0),
    );
    assert!(viewer(&harness).view().time().start > 1.0);
    let m = viewer(&harness).measurement().unwrap();
    assert_eq!((m.press.round(), m.reach.round()), (4.0, 7.0));

    // Esc removes it, alone or before deselecting the channels, then deselects them.
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert!(!viewer(&harness).measured());
    measure(&mut harness);
    click_in_list(&mut harness, Role::Label, "Cz");
    assert_eq!(viewer(&harness).view().selected().len(), 1);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert!(!viewer(&harness).measured());
    assert_eq!(viewer(&harness).view().selected().len(), 1);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert!(viewer(&harness).view().selected().is_empty());

    // A click in the traces removes it too.
    measure(&mut harness);
    click_at_time(&mut harness, 12.0);
    assert!(!viewer(&harness).measured());
}

#[test]
fn zoomed_out_the_amplitude_stays_exact_and_the_ends_read_envelopes() {
    // 512 Hz: the whole recording on screen is envelopes.
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    press(&mut harness, command(), Key::Num9);
    step_until(&mut harness, "the tiles", |app| {
        app.viewer().unwrap().complete()
    });
    dc_off(&mut harness);
    let dy = first_row(&harness);
    let (from, to) = (at_time(&harness, 3.3, dy), at_time(&harness, 11.7, dy));
    shift_drag(&mut harness, from, to);
    let viewer_now = viewer(&harness);
    let measurement = viewer_now.measurement().unwrap();
    let span = measurement.span(&viewer_now.document().recording);
    let samples = span.samples.unwrap();
    assert_eq!(span.dt, samples as f64 / 512.0);
    assert!((span.dt - 8.4).abs() < 0.05, "{span:?}");
    let (unit, factor) = kymo::scale_bar::reading_unit(
        &viewer_now.document().recording,
        viewer_now.view(),
        measurement.channel,
    );
    // The peak-to-peak of every sample in the band, not of the buckets drawn.
    let [interval, values] = settled_measurement(&mut harness);
    assert!(!interval.contains("samples"), "{interval}");
    let all = samples_of(&harness, measurement.channel, span.inside.clone());
    let (lo, hi) = all
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &v| {
            (l.min(v), h.max(v))
        });
    let want = (hi - lo) * factor;
    let got = peak_to_peak(&values);
    assert!(
        (got - want).abs() <= 1e-3 * want.abs(),
        "{values}: {got} vs {want} {unit}"
    );
    // The details count every sample; ranges at both ends, and no difference between them.
    let at = at_time(&harness, 6.0, dy);
    let details = settled_details(&mut harness, at);
    assert!(
        details.contains(&format!(" ({samples} samples at 512 Hz) ")),
        "{details}"
    );
    let values = details.rsplit(" \u{b7} ").next().unwrap();
    assert_eq!(values.matches('\u{2026}').count(), 2, "{details}");
    assert!(!values.contains('\u{394}'), "{details}");
    // No dots on envelopes.
    assert!(painted_dots(&harness).is_empty());
}

#[test]
fn holding_shift_over_the_traces_suggests_measuring() {
    let file = slow_preset();
    let mut harness = window(file.path());
    ready(&mut harness);
    let hint = match Platform::current() {
        Platform::Mac => "\u{21e7}-drag to measure",
        Platform::Windows | Platform::Linux => "Shift-drag to measure",
    };
    let center = viewer(&harness).layout().unwrap().traces.center();
    hover(&mut harness, center);
    harness.event(egui::Event::ModifiersChanged(Modifiers::SHIFT));
    harness.run_steps(2);
    let left = viewer(&harness).status().left.clone();
    assert!(left.starts_with(&format!("{hint} \u{b7} ")), "{left}");
    assert_eq!(painted_dots(&harness).len(), 1, "where a drag would start");
    harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    harness.run_steps(2);
    assert!(!viewer(&harness).status().left.contains(hint));
    assert!(painted_dots(&harness).is_empty());
    // ⇧ with other keys means something else: no hint, no dot, and dragging doesn't measure.
    for modifiers in [
        command() | Modifiers::SHIFT,
        Modifiers::ALT | Modifiers::SHIFT,
        command() | Modifiers::ALT | Modifiers::SHIFT,
        Modifiers::CTRL | Modifiers::SHIFT,
    ] {
        harness.event(egui::Event::ModifiersChanged(modifiers));
        harness.run_steps(2);
        let left = viewer(&harness).status().left.clone();
        assert!(!left.contains(hint), "{modifiers:?}: {left}");
        assert!(painted_dots(&harness).is_empty(), "{modifiers:?}");
    }
    harness.event(egui::Event::ModifiersChanged(command() | Modifiers::SHIFT));
    let width = viewer(&harness).layout().unwrap().traces.width();
    drag(&mut harness, center, center + egui::vec2(width / 4.0, 0.0));
    assert!(viewer(&harness).measurement().is_none());
    harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    harness.run_steps(2);
    // Not over the traces.
    hover(&mut harness, egui::pos2(5.0, 300.0));
    harness.event(egui::Event::ModifiersChanged(Modifiers::SHIFT));
    harness.run_steps(2);
    assert!(!viewer(&harness).status().left.contains(hint));
}

/// The middle of row `row` on screen (0 is the top row), over the traces.
fn row_middle(harness: &Harness<'_, KymoApp>, row: usize) -> egui::Pos2 {
    let traces = viewer(harness).layout().unwrap().traces;
    let height = traces.height() / viewer(harness).view().rows() as f32;
    egui::pos2(
        traces.center().x,
        traces.top() + (row as f32 + 0.5) * height,
    )
}

/// Every channel's amplitude, in its own unit.
fn channel_amplitudes(harness: &Harness<'_, KymoApp>) -> Vec<f64> {
    let viewer = viewer(harness);
    (0..viewer.document().recording.channels().len() as u32)
        .map(|c| viewer.view().channel_amplitude(kymo_core::ChannelId(c)))
        .collect()
}

/// ⌥⇧⌘-scrolling over a trace scales its channel alone (the selection where it is in it),
/// which the status line says while the keys are held; ⌥⇧⌘+ and − scale the selection; the
/// channel list marks a channel scaled on its own, and its menu returns it to its type's
/// scale, as Default Scale returns every channel. ⇧⌘-scrolling changes the rows, ⌥⌘-scrolling
/// scales every channel.
#[test]
fn a_channel_scales_alone() {
    use egui::accesskit::Role;
    use kymo::ui::header::ALONE_LABEL;
    use kymo::ui::types::SCALE_WITH_TYPE;
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let keys = kymo::keymap::Keymap::new(Platform::current()).channel_scale_keys();
    let all = command() | Modifiers::ALT | Modifiers::SHIFT;
    let start = channel_amplitudes(&harness);
    let visible = viewer(&harness).view().visible_channels().to_vec();
    let name = |harness: &Harness<'_, KymoApp>, c: kymo_core::ChannelId| {
        viewer(harness).document().recording.channel(c).name.clone()
    };
    let second = visible[1];

    // Held over the second row, the keys say what a scroll would scale, not measuring.
    let at = row_middle(&harness, 1);
    hover(&mut harness, at);
    harness.event(egui::Event::ModifiersChanged(all));
    harness.run_steps(2);
    let left = viewer(&harness).status().left.clone();
    let hint = format!("{keys}-scroll to scale {} alone", name(&harness, second));
    assert!(left.starts_with(&hint), "{left}");
    assert!(!left.contains("measure"), "{left}");
    assert!(harness.query_by_label(ALONE_LABEL).is_none());

    // A scroll up by 200 points doubles its height, and only its.
    scroll(&mut harness, egui::vec2(0.0, 200.0), all);
    let scaled = channel_amplitudes(&harness);
    for (c, (now, before)) in scaled.iter().zip(&start).enumerate() {
        let want = if c == second.0 as usize {
            before / 2.0
        } else {
            *before
        };
        assert!(
            (now - want).abs() <= 1e-9 * want,
            "channel {c}: {now} != {want}"
        );
    }
    let bars = viewer(&harness).scale_bars();
    assert!(
        bars.iter()
            .any(|(b, _)| b.group == kymo::view::ScaleGroup::Channel(second)),
        "{bars:?}"
    );
    harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    harness.run_steps(2);
    assert!(!viewer(&harness).status().left.contains(&hint));
    // Marked in the channel list, after its whole name (nothing is shortened), before its
    // Bad toggle; its menu returns it.
    assert_eq!(harness.query_all_by_label(ALONE_LABEL).count(), 1);
    let second_name = name(&harness, second);
    assert_eq!(painted(&harness, "\u{2026}"), Vec::<String>::new());
    mark_in_list(&mut harness, &second_name, true);
    let label = harness.get_by_role_and_label(Role::Label, &second_name);
    let mark = harness.get_by_label(ALONE_LABEL).rect();
    let bad = harness.get_by_label(&format!("{second_name} bad")).rect();
    assert!(
        mark.left() >= label.rect().right() - 0.5,
        "{mark:?} {:?}",
        label.rect()
    );
    assert!(mark.right() <= bad.left(), "{mark:?} {bad:?}");
    channel_options(&mut harness, &second_name);
    menu_path(&mut harness, &[SCALE_WITH_TYPE]);
    assert_eq!(channel_amplitudes(&harness), start);
    assert!(harness.query_by_label(ALONE_LABEL).is_none());

    // The first and third selected: over the third, both scale.
    let (first, third) = (visible[0], visible[2]);
    let first_name = name(&harness, first);
    click_in_list(&mut harness, Role::Label, &first_name);
    let third_name = name(&harness, third);
    harness
        .get_by_role_and_label(Role::Label, &third_name)
        .click_modifiers(command());
    harness.run_steps(2);
    assert_eq!(viewer(&harness).view().selected(), [first, third]);
    let at = row_middle(&harness, 2);
    hover(&mut harness, at);
    harness.event(egui::Event::ModifiersChanged(all));
    harness.run_steps(2);
    let left = viewer(&harness).status().left.clone();
    assert!(
        left.starts_with(&format!("{keys}-scroll to scale the 2 selected channels")),
        "{left}"
    );
    scroll(&mut harness, egui::vec2(0.0, -200.0), all);
    harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    harness.run_steps(2);
    let both = channel_amplitudes(&harness);
    for (c, (now, before)) in both.iter().zip(&start).enumerate() {
        let selected = c == first.0 as usize || c == third.0 as usize;
        let want = if selected { before * 2.0 } else { *before };
        assert!(
            (now - want).abs() <= 1e-9 * want,
            "channel {c}: {now} != {want}"
        );
    }
    assert_eq!(harness.query_all_by_label(ALONE_LABEL).count(), 2);
    // A click on a mark returns its channel alone.
    let marks: Vec<egui::Rect> = harness
        .query_all_by_label(ALONE_LABEL)
        .map(|n| n.rect())
        .collect();
    let third_row = harness
        .get_by_role_and_label(Role::Label, &third_name)
        .rect();
    let third_mark = marks
        .iter()
        .find(|r| (r.center().y - third_row.center().y).abs() < 2.0)
        .unwrap();
    click(&mut harness, third_mark.center());
    harness.run_steps(2);
    assert_eq!(harness.query_all_by_label(ALONE_LABEL).count(), 1);
    assert_eq!(
        viewer(&harness).view().channel_amplitude(third),
        start[third.0 as usize]
    );
    assert!(viewer(&harness).view().scaled_alone(first));
    // Scrolled again over the third, both scale: the first is now at 4 times its amplitude.
    hover(&mut harness, at);
    scroll(&mut harness, egui::vec2(0.0, -200.0), all);
    assert!(viewer(&harness).view().scaled_alone(third));
    // ⌥⇧⌘− scales them down, only while something is selected.
    press(&mut harness, all, Key::Minus);
    let step = kymo::view::SCALE_STEP;
    let down = viewer(&harness).view().channel_amplitude(first);
    assert!((down - start[first.0 as usize] * 4.0 * step).abs() <= 1e-9 * down);
    press(&mut harness, Modifiers::NONE, Key::Escape);
    assert!(viewer(&harness).view().selected().is_empty());
    let before = channel_amplitudes(&harness);
    press(&mut harness, all, Key::Minus);
    assert_eq!(channel_amplitudes(&harness), before);

    // ⌥⌘-scrolling scales every channel, the ones on their own too.
    let at = row_middle(&harness, 0);
    hover(&mut harness, at);
    scroll(
        &mut harness,
        egui::vec2(0.0, 200.0),
        command() | Modifiers::ALT,
    );
    let halved = channel_amplitudes(&harness);
    for (c, (now, was)) in halved.iter().zip(&before).enumerate() {
        assert!((now - was / 2.0).abs() <= 1e-9 * was, "channel {c}");
    }
    // ⇧⌘-scrolling shows half as many rows.
    let rows = viewer(&harness).view().rows();
    scroll(
        &mut harness,
        egui::vec2(0.0, 200.0),
        command() | Modifiers::SHIFT,
    );
    assert_eq!(
        viewer(&harness).view().rows(),
        (rows as f64 / 2.0).round() as usize
    );
    // Default Scale returns every channel to its type's default.
    press(&mut harness, command() | Modifiers::ALT, Key::Num0);
    let rec = &viewer(&harness).document().recording;
    let fresh = kymo::view::ViewState::new(rec);
    let defaults: Vec<f64> = (0..rec.channels().len() as u32)
        .map(|c| fresh.channel_amplitude(kymo_core::ChannelId(c)))
        .collect();
    assert_eq!(channel_amplitudes(&harness), defaults);
    assert!(harness.query_by_label(ALONE_LABEL).is_none());
}

/// The mark of a channel scaled on its own widens the names' column where there is room, so
/// the longest name stays whole beside it, as a click on it returns the channel.
#[test]
fn the_mark_widens_the_names_column() {
    use egui::accesskit::Role;
    use kymo::ui::header::ALONE_LABEL;
    use kymo_testgen::{ChannelSpec, Spec, XdfFormat};
    let long = "Interpupillary distance, both eyes";
    let channels = vec![
        ChannelSpec::xdf("Fz", "EEG", "microvolts", 100, XdfFormat::F32),
        ChannelSpec::xdf(long, "EEG", "microvolts", 100, XdfFormat::F32),
    ];
    let file = TempFile::written(&Spec::new(kymo_testgen::Format::Xdf, channels, 1.0, 30));
    let mut harness = window(file.path());
    ready(&mut harness);
    sidebar_to(&mut harness, 600.0);
    // Shortened beside the traces (at most 10 em), whole in the sidebar.
    let shortened = painted(&harness, "\u{2026}");
    assert_eq!(shortened.len(), 1, "{shortened:?}");
    let id = channel_named(&harness, long);
    let row = viewer(&harness).view().row_of(id).unwrap();
    let at = row_middle(&harness, row);
    hover(&mut harness, at);
    let all = command() | Modifiers::ALT | Modifiers::SHIFT;
    scroll(&mut harness, egui::vec2(0.0, 200.0), all);
    harness.run_steps(2);
    assert!(viewer(&harness).view().scaled_alone(id));
    assert_eq!(painted(&harness, "\u{2026}"), shortened);
    mark_in_list(&mut harness, long, true);
    let name = harness.get_by_role_and_label(Role::Label, long).rect();
    let mark = harness.get_by_label(ALONE_LABEL).rect();
    let bad = harness.get_by_label(&format!("{long} bad")).rect();
    assert!(mark.left() >= name.right() - 0.5, "{mark:?} {name:?}");
    assert!(mark.right() <= bad.left(), "{mark:?} {bad:?}");
    // A click on it returns the channel, and the column narrows again.
    click(&mut harness, mark.center());
    harness.run_steps(2);
    assert!(!viewer(&harness).view().scaled_alone(id));
    assert!(harness.query_by_label(ALONE_LABEL).is_none());
    let bad_after = harness.get_by_label(&format!("{long} bad")).rect();
    assert!(bad_after.left() < bad.left() - 5.0, "{bad_after:?} {bad:?}");
}

/// The window opening `path` and started as `run` starts it, its update checks asking
/// `source` (with `state` remembered) and `settings` set.
fn started_window(
    path: &Path,
    source: Source,
    state: UpdateState,
    settings: Settings,
) -> Harness<'static, KymoApp> {
    let path = path.to_owned();
    Harness::builder().with_size(SIZE).build_eframe(move |cc| {
        let mut app = KymoApp::new(
            &cc.egui_ctx,
            Gpu::none("no GPU in this test"),
            Options {
                updates: Some(source),
                update_state: state,
                settings,
                ..options()
            },
            Some(path),
        );
        app.start(&cc.egui_ctx);
        app
    })
}

/// A source whose latest release is `tag`, counting how often it is asked.
fn release_source(tag: &'static str) -> (Source, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = asked.clone();
    let source = Source::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(Release::from_tag(tag))
    });
    (source, asked)
}

#[test]
fn offers_a_newer_release_at_startup_until_it_is_skipped() {
    let file = TempFile::preset("bdf+c-small");
    let (source, asked) = release_source("v999.0.0");
    let mut harness = started_window(
        file.path(),
        source.clone(),
        UpdateState::default(),
        Settings::default(),
    );
    step_until(&mut harness, "the offer", |app| {
        app.update_offer().is_some()
    });
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert!(harness.state().update_state().last_check.is_some());
    harness.get_by_label("Update Available");
    harness.get_by_label("Skip This Version").click();
    harness.run_steps(2);
    assert!(harness.state().update_offer().is_none());
    let state = harness.state().update_state().clone();
    assert_eq!(state.skipped, Some(semver::Version::new(999, 0, 0)));

    // A day later, the skipped version isn't offered at startup, but is from the menu.
    let state = UpdateState {
        last_check: None,
        ..state
    };
    let mut harness = started_window(file.path(), source, state, Settings::default());
    step_until(&mut harness, "the check", |app| !app.checking_for_updates());
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    assert!(harness.state().update_offer().is_none());
    let ctx = harness.ctx.clone();
    harness.state_mut().perform(&ctx, Action::CheckForUpdates);
    step_until(&mut harness, "the offer", |app| {
        app.update_offer().is_some()
    });
    harness.get_by_label("Later").click();
    harness.run_steps(2);
    assert!(harness.state().update_offer().is_none());
    assert!(harness.state().notice().is_none());
}

#[test]
fn checks_at_startup_only_when_due_and_on() {
    let file = TempFile::preset("bdf+c-small");
    let (source, asked) = release_source("v999.0.0");
    let recent = UpdateState {
        last_check: Some(jiff::Timestamp::now()),
        skipped: None,
    };
    let mut harness = started_window(file.path(), source.clone(), recent, Settings::default());
    assert!(!harness.state().checking_for_updates());
    let off = Settings {
        check_updates: false,
        ..Settings::default()
    };
    let mut other = started_window(file.path(), source, UpdateState::default(), off);
    assert!(!other.state().checking_for_updates());
    harness.run_steps(4);
    other.run_steps(4);
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert!(harness.state().update_offer().is_none());
    assert!(other.state().update_offer().is_none());
}

#[test]
fn check_for_updates_reports_every_answer() {
    let file = TempFile::preset("bdf+c-small");
    let off = Settings {
        check_updates: false,
        ..Settings::default()
    };
    let check = |source: Source| {
        let mut harness = started_window(file.path(), source, UpdateState::default(), off.clone());
        let ctx = harness.ctx.clone();
        harness.state_mut().perform(&ctx, Action::CheckForUpdates);
        step_until(&mut harness, "the check", |app| !app.checking_for_updates());
        let notice = harness
            .state()
            .notice()
            .map(|(t, s)| (t.to_owned(), s.to_owned()));
        (notice, harness.state().update_state().last_check.is_some())
    };
    let version = kymo_core::version();
    let (notice, checked) = check(Source::new(move || Ok(Release::from_tag(version))));
    assert_eq!(
        notice,
        Some((
            "Kymo Is Up to Date".to_owned(),
            format!("Kymo {version} is the latest version.")
        ))
    );
    assert!(checked);
    // No release published yet.
    let (notice, _) = check(Source::new(|| Ok(None)));
    assert_eq!(notice.unwrap().0, "Kymo Is Up to Date");
    let (notice, checked) = check(Source::new(|| Err(anyhow::anyhow!("cannot reach GitHub"))));
    assert_eq!(
        notice,
        Some((
            "Could Not Check for Updates".to_owned(),
            "cannot reach GitHub".to_owned()
        ))
    );
    assert!(!checked);
}

/// Each dialog opens in the middle of the main window, each time: not where it was dragged
/// to, nor where it was in a smaller window.
#[test]
fn dialogs_open_in_the_middle_of_the_window() {
    use kymo::ui::Overlay;
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let rect = |h: &Harness<'_, KymoApp>, overlay: Overlay| {
        h.ctx.memory(|m| m.area_rect(overlay.id())).unwrap()
    };
    let centered = |h: &mut Harness<'_, KymoApp>, overlay: Overlay, case: &str| {
        // A window takes a few frames to grow to its content.
        h.run_steps(10);
        let window = rect(h, overlay);
        let middle = h.ctx.content_rect().center();
        // Within egui's rounding to whole pixels.
        assert!(
            (window.center() - middle).length() <= 2.0,
            "{overlay:?} {case}: its middle is at {:?}, the main window's at {middle:?}",
            window.center()
        );
    };
    let is_open = |h: &Harness<'_, KymoApp>, overlay: Overlay| {
        let app = h.state();
        match overlay {
            Overlay::Settings => app.settings_open(),
            Overlay::Shortcuts => app.shortcuts_open(),
            Overlay::About => app.about_open(),
            Overlay::GoToTime => app.go_to_open(),
            Overlay::RecordingInfo => app.info_open(),
            Overlay::ChannelTypes => app.channel_types_open(),
        }
    };
    // Channel Types opens from Settings, over it.
    let open = |h: &mut Harness<'_, KymoApp>, overlay: Overlay| {
        let ctx = h.ctx.clone();
        let action = match overlay {
            Overlay::Settings | Overlay::ChannelTypes => Action::Settings,
            Overlay::Shortcuts => Action::Shortcuts,
            Overlay::About => Action::About,
            Overlay::GoToTime => Action::GoToTime,
            Overlay::RecordingInfo => Action::RecordingInfo,
        };
        h.state_mut().perform(&ctx, action);
        h.run_steps(4);
        if overlay == Overlay::ChannelTypes {
            h.get_by_role_and_label(egui::accesskit::Role::Button, "Channel Types")
                .click();
            h.run_steps(2);
        }
        assert!(is_open(h, overlay), "{overlay:?}");
    };
    // Esc closes the one in front; Settings is behind Channel Types.
    let close = |h: &mut Harness<'_, KymoApp>| {
        while Overlay::ALL.iter().any(|&o| is_open(h, o)) {
            h.key_press(Key::Escape);
            h.run_steps(4);
        }
    };
    // By its title bar, clear of the close button; within the screen, which egui keeps
    // windows in.
    let drag = |h: &mut Harness<'_, KymoApp>, overlay: Overlay| {
        let from = rect(h, overlay).left_top() + egui::vec2(30.0, 12.0);
        let to = from + egui::vec2(-150.0, 40.0);
        h.hover_at(from);
        h.step();
        h.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        h.step();
        for t in [0.5, 1.0] {
            h.hover_at(from + (to - from) * t);
            h.step();
        }
        h.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        h.step();
        let moved = rect(h, overlay).left_top() + egui::vec2(30.0, 12.0);
        assert!(
            (moved - to).length() <= 2.0,
            "{overlay:?} wasn't dragged: {:?} to {to:?}, at {moved:?}",
            from
        );
    };
    for overlay in Overlay::ALL {
        open(&mut harness, overlay);
        centered(&mut harness, overlay, "opened");
        drag(&mut harness, overlay);
        close(&mut harness);
        open(&mut harness, overlay);
        centered(&mut harness, overlay, "opened again after a drag");
        close(&mut harness);
    }
    harness.set_size(SIZE + egui::vec2(400.0, 300.0));
    harness.run_steps(2);
    for overlay in Overlay::ALL {
        open(&mut harness, overlay);
        centered(&mut harness, overlay, "in a larger window");
        close(&mut harness);
    }
}

/// A dialog's title is as large as its text, as the system's are, not a heading: drawn as
/// wide as its text at the body's size.
#[test]
fn dialog_titles_are_as_large_as_body_text() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = screenshot_window(file.path());
    ready(&mut harness);
    let ctx = harness.ctx.clone();
    harness.state_mut().perform(&ctx, Action::Settings);
    harness.run_steps(10);
    let window = harness
        .ctx
        .memory(|m| m.area_rect(kymo::ui::Overlay::Settings.id()))
        .unwrap();
    let close = harness.get_by_label("Close window").rect();
    let image = harness.render().unwrap();
    let luma = |x: f32, y: f32| {
        let [r, g, b, _] = image.get_pixel(x as u32, y as u32).0;
        (u32::from(r) + u32::from(g) + u32::from(b)) / 3
    };
    // Columns of the title bar, left of the close button, with text in them.
    let fill = luma(window.left() + 4.0, close.center().y);
    let (top, bottom) = (close.top() - 4.0, close.bottom() + 4.0);
    let inked: Vec<f32> = (window.left() as u32 + 4..close.left() as u32 - 4)
        .map(|x| x as f32)
        .filter(|&x| (top as u32..bottom as u32).any(|y| luma(x, y as f32).abs_diff(fill) > 60))
        .collect();
    let drawn = inked.last().unwrap() - inked.first().unwrap() + 1.0;
    let width = |style: egui::TextStyle| {
        let font = style.resolve(&ctx.global_style());
        ctx.fonts_mut(|f| {
            f.layout_no_wrap("Settings".into(), font, egui::Color32::WHITE)
                .size()
                .x
        })
    };
    let body = width(egui::TextStyle::Body);
    assert!(
        (drawn - body).abs() <= 3.0,
        "the title is {drawn} wide, as body text {body}, as a heading {}",
        width(egui::TextStyle::Heading)
    );
}

/// Turns the minimap on through the View menu.
fn show_minimap(harness: &mut Harness<'_, KymoApp>) {
    choose(harness, "View", "Show Minimap");
    harness.run_steps(2);
}

#[test]
fn the_minimap_takes_the_time_scrollbars_place() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let before = viewer(&harness).layout().unwrap();
    assert!(before.time_bar.is_some() && before.minimap.is_none());
    assert!(!checked(&mut harness, "View", "Show Minimap"));

    show_minimap(&mut harness);
    assert!(checked(&mut harness, "View", "Show Minimap"));
    assert!(harness.state().settings().minimap);
    let layout = viewer(&harness).layout().unwrap();
    let rect = layout.minimap.unwrap();
    assert_eq!(layout.time_bar, None);
    assert_eq!(rect.height(), minimap::DEFAULT_HEIGHT);
    assert_eq!(rect.x_range(), layout.traces.x_range());
    assert_eq!(rect.bottom(), before.time_bar.unwrap().bottom());
    // The traces give up the room the minimap takes beyond the scrollbar's.
    let taken = minimap::DEFAULT_HEIGHT - scrollbar::THICKNESS;
    assert!((before.traces.bottom() - layout.traces.bottom() - taken).abs() <= 1.0);

    show_minimap(&mut harness);
    assert!(!harness.state().settings().minimap);
    assert_eq!(viewer(&harness).layout().unwrap(), before);
}

#[test]
fn the_minimap_moves_the_window() {
    // 60 s, 20 s at a time.
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    show_minimap(&mut harness);
    let rect = viewer(&harness).layout().unwrap().minimap.unwrap();
    let x_of = |t: f64| rect.left() + (t / 60.0) as f32 * rect.width();
    // In f32, as points are: within 1e-4 s of the viewer's times, in f64.
    let time_of = |x: f32| f64::from((x - rect.left()) / rect.width()) * 60.0;
    let y = rect.center().y;
    let start = |harness: &Harness<'_, KymoApp>| viewer(harness).view().start();

    // A click centers the window there, as far as the recording goes.
    let x = x_of(30.0);
    click(&mut harness, egui::pos2(x, y));
    assert!((start(&harness) - (time_of(x) - 10.0)).abs() < 1e-4);
    click(&mut harness, egui::pos2(rect.right() - 2.0, y));
    assert_eq!(viewer(&harness).view().time(), 40.0..60.0);

    // Dragging the window moves it as grabbed.
    let from = egui::pos2(x_of(45.0), y);
    let to = egui::pos2(x_of(45.0) - 100.0, y);
    drag(&mut harness, from, to);
    let want = 40.0 + time_of(to.x) - time_of(from.x);
    assert!((start(&harness) - want).abs() < 1e-4, "{}", start(&harness));

    // A press beside it centers it there, and dragging on moves it from there.
    let from = egui::pos2(x_of(55.0), y);
    let to = egui::pos2(x_of(25.0), y);
    drag(&mut harness, from, to);
    assert!((start(&harness) - (time_of(to.x) - 10.0)).abs() < 1e-4);
}

#[test]
fn the_minimap_resizes_from_its_top_edge() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window_with_short_frames(file.path());
    ready(&mut harness);
    show_minimap(&mut harness);
    let layout = viewer(&harness).layout().unwrap();
    let rect = layout.minimap.unwrap();
    let edge = egui::pos2(rect.center().x, rect.top());

    drag(&mut harness, edge, edge - egui::vec2(0.0, 30.0));
    let height = minimap::DEFAULT_HEIGHT + 30.0;
    let resized = viewer(&harness).layout().unwrap();
    assert_eq!(resized.minimap.unwrap().height(), height);
    assert_eq!(harness.state().settings().minimap_height, height);
    assert!((layout.traces.bottom() - resized.traces.bottom() - 30.0).abs() <= 1.0);

    // No higher than a quarter of the trace region.
    drag(
        &mut harness,
        edge - egui::vec2(0.0, 30.0),
        egui::pos2(edge.x, 0.0),
    );
    let tallest = viewer(&harness).layout().unwrap().minimap.unwrap();
    let region = tallest.bottom() - layout.traces.top();
    assert!((tallest.height() - region * minimap::MAX_FRACTION).abs() <= 1.0);
    // The strip is on whole pixels.
    assert!((harness.state().settings().minimap_height - tallest.height()).abs() <= 1.0);
    // The window didn't move.
    assert_eq!(viewer(&harness).view().time(), 0.0..20.0);

    // A double click on the edge puts it back.
    double_click(&mut harness, egui::pos2(edge.x, tallest.top()));
    harness.step();
    let rect = viewer(&harness).layout().unwrap().minimap.unwrap();
    assert_eq!(rect.height(), minimap::DEFAULT_HEIGHT);
    assert_eq!(
        harness.state().settings().minimap_height,
        minimap::DEFAULT_HEIGHT
    );
}

/// A secondary click at `pos`.
fn secondary_click(harness: &mut Harness<'_, KymoApp>, pos: egui::Pos2) {
    hover(harness, pos);
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.run_steps(2);
}

#[test]
fn the_minimaps_menu_resets_its_height_and_hides_it() {
    let file = TempFile::preset("bdf+c-small");
    let options = Options {
        settings: Settings {
            minimap: true,
            minimap_height: 70.0,
            ..options().settings
        },
        ..options()
    };
    let mut harness = fonts_window(file.path(), options);
    ready(&mut harness);
    // As set when the recording opens.
    let rect = viewer(&harness).layout().unwrap().minimap.unwrap();
    assert_eq!(rect.height(), 70.0);

    secondary_click(&mut harness, rect.center());
    // A right-click doesn't move the window.
    assert_eq!(viewer(&harness).view().time(), 0.0..20.0);
    harness.get_by_label(minimap::RESET_HEIGHT).click();
    harness.run_steps(2);
    let rect = viewer(&harness).layout().unwrap().minimap.unwrap();
    assert_eq!(rect.height(), minimap::DEFAULT_HEIGHT);
    assert_eq!(
        harness.state().settings().minimap_height,
        minimap::DEFAULT_HEIGHT
    );

    // Show Signals, on by default, turns them off and on.
    assert!(harness.state().settings().minimap_signals);
    for on in [false, true] {
        secondary_click(&mut harness, rect.center());
        harness.get_by_label(minimap::SHOW_SIGNALS).click();
        harness.run_steps(2);
        assert_eq!(harness.state().settings().minimap_signals, on);
    }

    secondary_click(&mut harness, rect.center());
    harness.get_by_label(minimap::HIDE).click();
    harness.run_steps(2);
    assert!(!harness.state().settings().minimap);
    let layout = viewer(&harness).layout().unwrap();
    assert!(layout.minimap.is_none() && layout.time_bar.is_some());
}

#[test]
fn hovering_the_minimap_names_its_time_and_annotations() {
    let file = TempFile::preset("edf+c-annotations");
    for signals in [false, true] {
        let settings = Settings {
            minimap: true,
            minimap_signals: signals,
            minimap_height: 80.0,
            ..options().settings
        };
        let mut harness = window_with(file.path(), settings);
        ready(&mut harness);
        let rect = viewer(&harness).layout().unwrap().minimap.unwrap();
        let ppp = harness.ctx.pixels_per_point();
        let width_px = (rect.width() * ppp).round() as u32;
        for (fx, fy) in [(0.1, 0.2), (0.37, 0.5), (0.8, 0.9)] {
            let p = egui::pos2(
                rect.left() + rect.width() * fx,
                rect.top() + rect.height() * fy,
            );
            hover(&mut harness, p);
            harness.step();
            let viewer = viewer(&harness);
            let duration = viewer.view().duration();
            let time = f64::from((p.x - rect.left()) / rect.width()) * duration;
            // With signals, the channel of the row under the pointer, named without a
            // reading.
            let rows = viewer.view().row_channels();
            let row = ((p.y - rect.top()) / rect.height() * rows.len() as f32) as usize;
            let channel = signals.then(|| rows[row]);
            let under = annotations::overview_under(
                &viewer.document().annotations,
                viewer.legend(),
                duration,
                width_px,
                f64::from((p.x - rect.left()) * ppp),
                (2.0 * ppp).round() as u32,
                channel,
            );
            assert!(!under.is_empty());
            let rec = &viewer.document().recording;
            let want = format!(
                "{} · {}",
                status::pointer_text(
                    time,
                    status::time_decimals(rec.sfreq()),
                    Clock::for_end(duration),
                    channel.map(|c| (rec.channel(c).name.as_str(), None)),
                ),
                status::annotations_text(under.iter().map(|a| a.data.description.as_str())),
            );
            assert_eq!(viewer.status().left, want, "signals {signals}");
        }
    }
}

/// A window on `path` with the minimap shown `height` points tall, signals or not, its
/// spreads read and drawn; screenshots render on the shared GPU.
fn minimap_window(path: &Path, height: f32, signals: bool) -> Harness<'static, KymoApp> {
    let mut harness = screenshot_window(path);
    ready(&mut harness);
    let settings = Settings {
        minimap: true,
        minimap_height: height,
        minimap_signals: signals,
        ..harness.state().settings().clone()
    };
    harness.state_mut().set_settings(settings);
    let grids = viewer(&harness).document().recording.grids().len();
    step_until(&mut harness, "the spreads", |app| {
        !signals || app.viewer().unwrap().minimap().spreads_read() == grids
    });
    harness.run_steps(2);
    harness
}

/// The rendered pixel at `p` (in points).
fn pixel_at(harness: &mut Harness<'_, KymoApp>, p: egui::Pos2) -> [u8; 3] {
    let ppp = harness.ctx.pixels_per_point();
    let image = harness.render().unwrap();
    let [r, g, b, _] = image.get_pixel((p.x * ppp) as u32, (p.y * ppp) as u32).0;
    [r, g, b]
}

#[test]
fn the_minimaps_rows_show_the_signals_and_leave_gaps_empty() {
    // 10 min of channels with 3 gaps, without the trigger channel's events.
    let file = without_triggers("edf+d-mixed");
    let mut harness = minimap_window(file.path(), 100.0, true);
    let rect = viewer(&harness).layout().unwrap().minimap.unwrap();
    let (gap, present) = {
        let viewer = viewer(&harness);
        let rec = &viewer.document().recording;
        let duration = rec.duration();
        let grid = &rec.grids()[0];
        let segments = grid.segments();
        assert!(segments.len() > 1, "{segments:?}");
        let gap = (grid.time_of(segments[0].end) + grid.time_of(segments[1].start)) / 2.0;
        // Where samples are and no annotation is near, so only the signals show.
        let near = 3.0 * duration / f64::from(rect.width());
        let present = (0..100)
            .map(|k| {
                let (a, b) = (
                    grid.time_of(segments[1].start),
                    grid.time_of(segments[1].end),
                );
                a + (b - a) * (f64::from(k) + 0.5) / 100.0
            })
            .find(|&t| {
                !viewer
                    .document()
                    .annotations
                    .iter()
                    .any(|a| a.data.onset - near <= t && t <= a.data.end() + near)
            })
            .expect("a stretch without annotations");
        (gap / duration, present / duration)
    };
    let background = harness.ctx.global_style().visuals.extreme_bg_color;
    let x = |f: f64| rect.left() + rect.width() * f as f32;
    for fy in [0.1, 0.5, 0.9] {
        let y = rect.top() + rect.height() * fy;
        let [r, g, b] = pixel_at(&mut harness, egui::pos2(x(gap), y));
        let near = |a: u8, b: u8| a.abs_diff(b) <= 1;
        assert!(
            near(r, background.r()) && near(g, background.g()) && near(b, background.b()),
            "gap at {fy}: {:?}",
            [r, g, b]
        );
        // Gray, brighter than the background.
        let [r, g, b] = pixel_at(&mut harness, egui::pos2(x(present), y));
        assert!(
            r > background.r() + 3 && r.max(g).max(b) - r.min(g).min(b) <= 2,
            "signal at {fy}: {:?}",
            [r, g, b]
        );
    }
}

#[test]
fn the_minimap_draws_channel_annotations_in_their_rows() {
    // 12 channels, annotations on some; without the stim events, which come every half
    // second.
    let file = without_triggers("fif-float32-meg");
    let mut harness = minimap_window(file.path(), 160.0, true);
    let rect = viewer(&harness).layout().unwrap().minimap.unwrap();
    let saturation = |[r, g, b]: [u8; 3]| r.max(g).max(b) - r.min(g).min(b);
    let (x, inside, outside) = {
        let viewer = viewer(&harness);
        let view = viewer.view();
        let rows = view.row_count() as f32;
        let duration = view.duration();
        let store = &viewer.document().annotations;
        let at =
            |t: f64, a: &kymo_core::Annotation| a.data.onset - 0.5 <= t && t <= a.data.end() + 0.5;
        let all = |t: f64| {
            store
                .iter()
                .any(|a| annotations::spans_all(&a.data) && at(t, a))
        };
        let a = store
            .iter()
            .find(|a| {
                let t = a.data.onset + a.data.duration / 2.0;
                !annotations::spans_all(&a.data) && !all(t)
            })
            .expect("a channel annotation clear of the others");
        let t = a.data.onset + a.data.duration / 2.0;
        let row = view.row_of(a.data.channels[0]).unwrap();
        // A row no annotation of channels covers there.
        let other = (0..view.row_count())
            .find(|&r| {
                let c = view.row_channels()[r];
                !store
                    .iter()
                    .any(|a| a.data.channels.contains(&c) && at(t, a))
            })
            .unwrap();
        let y = |r: usize| rect.top() + (r as f32 + 0.5) / rows * rect.height();
        let x = rect.left() + (t / duration) as f32 * rect.width();
        let name = viewer
            .document()
            .recording
            .channel(a.data.channels[0])
            .name
            .clone();
        (egui::pos2(x, y(row)), egui::pos2(x, y(other)), name)
    };
    let (inside, outside, name) = (x, inside, outside);
    let color = pixel_at(&mut harness, inside);
    assert!(saturation(color) > 15, "{color:?}");
    let color = pixel_at(&mut harness, outside);
    assert!(saturation(color) <= 2, "{color:?}");

    // Hidden, the channel takes its annotation's marks along: none lies where its row was.
    click_in_list(
        &mut harness,
        egui::accesskit::Role::CheckBox,
        &format!("Show {name}"),
    );
    harness.run_steps(2);
    let color = pixel_at(&mut harness, inside);
    assert!(saturation(color) <= 2, "{color:?}");
}

#[test]
fn settings_show_or_hide_the_minimaps_signals() {
    let file = TempFile::preset("bdf+c-small");
    let mut harness = window(file.path());
    ready(&mut harness);
    let ctx = harness.ctx.clone();
    harness.state_mut().perform(&ctx, Action::Settings);
    harness.run_steps(3);
    assert!(harness.state().settings().minimap_signals);
    harness
        .get_by_role_and_label(egui::accesskit::Role::CheckBox, minimap::SHOW_SIGNALS)
        .click();
    harness.run_steps(2);
    assert!(!harness.state().settings().minimap_signals);
    // Restore Defaults shows them again, as it is a setting of its own.
    harness.get_by_label("Restore Defaults").click();
    harness.run_steps(2);
    assert!(harness.state().settings().minimap_signals);
}

#[test]
fn the_minimap_moves_the_rows_with_the_window() {
    // 32 channels.
    let file = TempFile::preset("edf+c-annotations");
    let settings = Settings {
        minimap: true,
        minimap_height: 128.0,
        ..options().settings
    };
    let mut harness = window_with(file.path(), settings);
    ready(&mut harness);
    let rect = viewer(&harness).layout().unwrap().minimap.unwrap();
    let view = |h: &Harness<'_, KymoApp>| viewer(h).view().clone();
    let (count, rows) = {
        let v = view(&harness);
        (v.row_count() as f64, v.rows() as f64)
    };
    assert!(rows < count, "{rows} of {count} rows on screen");
    let row_of = |y: f32| f64::from((y - rect.top()) / rect.height()) * count;
    let x = rect.center().x;

    // A click centers the window on its rows too, as far as they go.
    let y = rect.top() + rect.height() * 0.6;
    click(&mut harness, egui::pos2(x, y));
    let top = view(&harness).top();
    assert!((top - (row_of(y) - rows / 2.0)).abs() < 1e-3, "{top}");
    click(&mut harness, egui::pos2(x, rect.bottom() - 1.0));
    assert_eq!(view(&harness).top(), count - rows);

    // Dragging the window moves its rows as grabbed.
    let from = egui::pos2(
        x,
        rect.top() + rect.height() * ((count - rows / 2.0) / count) as f32,
    );
    let to = from - egui::vec2(0.0, 40.0);
    drag(&mut harness, from, to);
    let want = count - rows + row_of(to.y) - row_of(from.y);
    let top = view(&harness).top();
    assert!((top - want).abs() < 1e-3, "{top}, want {want}");

    // Without signals, a click moves only the time.
    secondary_click(&mut harness, rect.center());
    harness.get_by_label(minimap::SHOW_SIGNALS).click();
    harness.run_steps(2);
    assert!(!harness.state().settings().minimap_signals);
    let before = view(&harness).top();
    click(&mut harness, egui::pos2(x, rect.top() + 4.0));
    assert_eq!(view(&harness).top(), before);
}

#[test]
fn hiding_a_channel_drops_its_minimap_row() {
    let file = TempFile::preset("edf+c-kinds");
    let settings = Settings {
        minimap: true,
        minimap_height: 120.0,
        ..options().settings
    };
    let mut harness = window_with(file.path(), settings);
    ready(&mut harness);
    let rect = viewer(&harness).layout().unwrap().minimap.unwrap();
    let named = |harness: &mut Harness<'_, KymoApp>, row: usize| {
        let count = viewer(harness).view().row_count() as f32;
        let y = rect.top() + (row as f32 + 0.5) / count * rect.height();
        hover(harness, egui::pos2(rect.center().x, y));
        harness.step();
        let rows = viewer(harness).view().row_channels().to_vec();
        let rec = &viewer(harness).document().recording;
        let name = rec.channel(rows[row]).name.clone();
        assert!(
            viewer(harness)
                .status()
                .left
                .contains(&format!(" · {name}")),
            "{} names {name}",
            viewer(harness).status().left
        );
        name
    };
    let first = named(&mut harness, 0);
    let second = named(&mut harness, 1);
    click_in_list(
        &mut harness,
        egui::accesskit::Role::CheckBox,
        &format!("Show {first}"),
    );
    assert_eq!(named(&mut harness, 0), second);
}

#[test]
fn the_minimap_follows_picked_colors() {
    // 30 s without annotations, the window over the first 20 s; an event at 10 s.
    let file = without_triggers("fif-mixed");
    let dir = TempDir::new();
    let events = dir.path().join("events.tsv");
    std::fs::write(&events, "onset\tduration\ttrial_type\n10\t0\tblink\n").unwrap();
    let mut harness = screenshot_window(file.path());
    ready(&mut harness);
    assert!(harness.state_mut().load_annotations_from(&events));
    harness.run_steps(2);
    assert_eq!(viewer(&harness).document().annotations.len(), 1);
    show_minimap(&mut harness);
    // The event's line, inside the window, where nothing lies over it.
    let line = |harness: &mut Harness<'_, KymoApp>| {
        let rect = viewer(harness).layout().unwrap().minimap.unwrap();
        let ppp = harness.ctx.pixels_per_point();
        let column = (rect.width() * ppp).round() as u32 / 3;
        let image = harness.render().unwrap();
        let x = (rect.left() * ppp) as u32 + column;
        let y = (rect.center().y * ppp) as u32;
        let [r, g, b, _] = image.get_pixel(x, y).0;
        [r, g, b]
    };
    let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(&a, b)| a.abs_diff(b) <= 2);
    let entry = viewer(&harness)
        .legend()
        .entry(None, "blink")
        .unwrap()
        .color;
    let palette = annotation_color(entry, &harness.ctx.global_style().visuals);
    let before = line(&mut harness);
    assert!(
        near(before, [palette.r(), palette.g(), palette.b()]),
        "{before:?}"
    );

    pick(&mut harness, "Color of blink", [10, 200, 30]);
    close_picker(&mut harness);
    let after = line(&mut harness);
    assert!(near(after, [10, 200, 30]), "{after:?}");
}
