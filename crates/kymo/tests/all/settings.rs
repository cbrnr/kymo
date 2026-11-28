//! The settings: kept in and read back from eframe's storage, with defaults and ranges.

use std::collections::HashMap;

use eframe::Storage;
use eframe::egui::Color32;
use kymo::annotations::AnnotationColors;
use kymo::settings::{
    DEFAULT_WINDOW_S, DISK_CACHE_MB, GPU_MEMORY_MB, OFF, SESSION, Settings, TILE_CACHE_MB, Theme,
    UNLIMITED, nearest,
};
use kymo::settings::{load_annotation_colors, save_annotation_colors};
use kymo::types::TypeSettings;
use kymo::ui::settings::{disk_cache_off_warning, disk_cache_text, megabytes_text};
use kymo::update::UpdateState;
use kymo_core::ChannelKind;
use proptest::prelude::*;

/// Storage in memory.
#[derive(Default)]
struct Memory(HashMap<String, String>);

impl Storage for Memory {
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

#[test]
fn defaults_without_storage_or_values() {
    let defaults = Settings::default();
    assert_eq!(defaults.tile_cache_mb, 256);
    assert_eq!(defaults.gpu_memory_mb, 256);
    assert_eq!(defaults.disk_cache_mb, 5120);
    assert_eq!(defaults.default_window_s, 20.0);
    assert!(!defaults.antialias);
    assert!(defaults.auto_scale_on_open);
    assert!(defaults.minimap && defaults.minimap_signals);
    assert_eq!(defaults.theme, Theme::System);
    assert!(defaults.animations);
    // On where Kymo has a system font to use.
    assert_eq!(
        defaults.system_font,
        kymo::ui::fonts::SYSTEM_FONT_NAME.is_some()
    );
    assert_eq!(Settings::load(None), defaults);
    assert_eq!(Settings::load(Some(&Memory::default())), defaults);
}

/// The default per-type settings with DC removal off for `no_dc`, scaling each channel on
/// its own flipped for `flipped` and the colors `picked`.
fn types(
    no_dc: &[ChannelKind],
    flipped: &[ChannelKind],
    picked: &[(ChannelKind, Color32)],
) -> TypeSettings {
    let mut types = TypeSettings::default();
    for kind in no_dc {
        types.set_remove_dc(kind, false);
    }
    for kind in flipped {
        let own = types.own_scale(kind);
        types.set_own_scale(kind, !own);
    }
    for (kind, color) in picked {
        types.set_color(kind, Some(*color));
    }
    types
}

#[test]
fn saved_settings_read_back() {
    let settings = Settings {
        tile_cache_mb: 1024,
        gpu_memory_mb: 512,
        disk_cache_mb: 0,
        default_window_s: 7.5,
        antialias: true,
        scale_bars: false,
        minimap: true,
        minimap_height: 80.5,
        minimap_signals: false,
        remove_dc: false,
        types: types(
            &[
                ChannelKind::EEG,
                ChannelKind::GRAD,
                ChannelKind::named("Gaze"),
            ],
            &[
                ChannelKind::MISC,
                ChannelKind::ECG,
                ChannelKind::named("tab\there"),
            ],
            &[
                (ChannelKind::EOG, Color32::from_rgb(10, 200, 30)),
                (ChannelKind::MISC, Color32::from_rgb(0, 0, 0)),
                (ChannelKind::REF_MEG, Color32::from_rgb(1, 2, 3)),
                (
                    ChannelKind::named("line\nbreak \\ Ü"),
                    Color32::from_rgb(4, 5, 6),
                ),
            ],
        ),
        auto_scale_on_open: true,
        theme: Theme::Dark,
        system_font: true,
        animations: false,
        check_updates: false,
    };
    let mut storage = Memory::default();
    settings.save(&mut storage);
    assert_eq!(Settings::load(Some(&storage)), settings);
    // The types by name, a type kymo doesn't know as first given.
    let loaded = Settings::load(Some(&storage)).types;
    assert!(!loaded.remove_dc(&ChannelKind::named("GAZE")));
    let names: Vec<&str> = loaded.entries().map(|t| t.name.as_str()).collect();
    assert!(
        names.contains(&"Gaze") && names.contains(&"tab\there"),
        "{names:?}"
    );
    assert!(storage.0.contains_key("kymo.channel_types"));
    for theme in Theme::ALL {
        Settings {
            theme,
            ..settings.clone()
        }
        .save(&mut storage);
        assert_eq!(Settings::load(Some(&storage)).theme, theme);
    }
    assert_eq!(settings.tile_cache_bytes(), 1024 << 20);
    assert_eq!(settings.gpu_memory_bytes(), 512 << 20);
    assert!(settings.disk_cache_off());
    // While Kymo runs and once it has quit.
    let limits = |disk_cache_mb| {
        let settings = Settings {
            disk_cache_mb,
            ..Settings::default()
        };
        (
            settings.disk_cache_bytes(),
            settings.disk_cache_bytes_at_exit(),
        )
    };
    assert_eq!(limits(OFF), (0, 0));
    assert_eq!(limits(SESSION), (u64::MAX, 0));
    assert_eq!(limits(10240), (10 << 30, 10 << 30));
    assert_eq!(limits(UNLIMITED), (u64::MAX, u64::MAX));
    assert!(!Settings::default().disk_cache_off());
}

#[test]
fn unreadable_values_are_defaults_and_others_kept() {
    let saved = Settings {
        tile_cache_mb: 512,
        gpu_memory_mb: 128,
        disk_cache_mb: 1024,
        default_window_s: 5.0,
        antialias: true,
        scale_bars: false,
        minimap: true,
        minimap_height: 80.5,
        minimap_signals: false,
        remove_dc: false,
        types: types(
            &[ChannelKind::EEG],
            &[ChannelKind::MISC],
            &[(ChannelKind::GRAD, Color32::from_rgb(255, 0, 128))],
        ),
        auto_scale_on_open: true,
        theme: Theme::Light,
        system_font: true,
        animations: false,
        check_updates: false,
    };
    let defaults = Settings::default();
    let cases = [
        (
            "kymo.tile_cache_mb",
            Settings {
                tile_cache_mb: defaults.tile_cache_mb,
                ..saved.clone()
            },
        ),
        (
            "kymo.gpu_memory_mb",
            Settings {
                gpu_memory_mb: defaults.gpu_memory_mb,
                ..saved.clone()
            },
        ),
        (
            "kymo.disk_cache_mb",
            Settings {
                disk_cache_mb: defaults.disk_cache_mb,
                ..saved.clone()
            },
        ),
        (
            "kymo.default_window_s",
            Settings {
                default_window_s: defaults.default_window_s,
                ..saved.clone()
            },
        ),
        (
            "kymo.antialias",
            Settings {
                antialias: defaults.antialias,
                ..saved.clone()
            },
        ),
        (
            "kymo.scale_bars",
            Settings {
                scale_bars: defaults.scale_bars,
                ..saved.clone()
            },
        ),
        (
            "kymo.minimap",
            Settings {
                minimap: defaults.minimap,
                ..saved.clone()
            },
        ),
        (
            "kymo.minimap_height",
            Settings {
                minimap_height: defaults.minimap_height,
                ..saved.clone()
            },
        ),
        (
            "kymo.minimap_signals",
            Settings {
                minimap_signals: defaults.minimap_signals,
                ..saved.clone()
            },
        ),
        (
            "kymo.remove_dc",
            Settings {
                remove_dc: defaults.remove_dc,
                ..saved.clone()
            },
        ),
        (
            "kymo.auto_scale_on_open",
            Settings {
                auto_scale_on_open: defaults.auto_scale_on_open,
                ..saved.clone()
            },
        ),
        (
            "kymo.channel_types",
            Settings {
                types: TypeSettings::default(),
                ..saved.clone()
            },
        ),
        (
            "kymo.theme",
            Settings {
                theme: defaults.theme,
                ..saved.clone()
            },
        ),
        (
            "kymo.system_font",
            Settings {
                system_font: defaults.system_font,
                ..saved.clone()
            },
        ),
        (
            "kymo.animations",
            Settings {
                animations: defaults.animations,
                ..saved.clone()
            },
        ),
        (
            "kymo.check_updates",
            Settings {
                check_updates: defaults.check_updates,
                ..saved.clone()
            },
        ),
    ];
    for (key, want) in cases {
        let colors = ["#12345", "#12345g", "ff0080", "#+1+2+3", "#aé€"];
        for value in ["lots", "", "-1.5e", "NaN"].into_iter().chain(colors) {
            let mut storage = Memory::default();
            saved.save(&mut storage);
            assert!(storage.0.contains_key(key), "{key} not saved");
            storage.set_string(key, value.into());
            assert_eq!(Settings::load(Some(&storage)), want, "{key} = {value:?}");
        }
    }
}

#[test]
fn values_out_of_range_are_clamped() {
    let mut storage = Memory::default();
    Settings {
        tile_cache_mb: 1,
        gpu_memory_mb: u32::MAX,
        disk_cache_mb: u32::MAX,
        default_window_s: 1e9,
        antialias: false,
        scale_bars: true,
        minimap: false,
        minimap_height: 1.0,
        minimap_signals: true,
        remove_dc: true,
        types: TypeSettings::default(),
        auto_scale_on_open: false,
        theme: Theme::System,
        system_font: false,
        animations: false,
        check_updates: true,
    }
    .save(&mut storage);
    let loaded = Settings::load(Some(&storage));
    assert_eq!(loaded.tile_cache_mb, TILE_CACHE_MB[0]);
    assert_eq!(loaded.gpu_memory_mb, *GPU_MEMORY_MB.last().unwrap());
    assert_eq!(loaded.disk_cache_mb, UNLIMITED);
    assert_eq!(loaded.default_window_s, *DEFAULT_WINDOW_S.end());
    assert_eq!(loaded.minimap_height, kymo::ui::minimap::MIN_HEIGHT);
    storage.set_string("kymo.default_window_s", "-3".into());
    assert_eq!(
        Settings::load(Some(&storage)).default_window_s,
        *DEFAULT_WINDOW_S.start()
    );
}

/// Budgets saved by earlier versions, which took any number of MB, read back as the nearest
/// offered one; the defaults are offered.
#[test]
fn budgets_snap_to_the_nearest_offered() {
    let defaults = Settings::default();
    assert!(TILE_CACHE_MB.contains(&defaults.tile_cache_mb));
    assert!(GPU_MEMORY_MB.contains(&defaults.gpu_memory_mb));
    for (saved, tile, gpu) in [
        ("0", 128, 128),
        ("64", 128, 128),
        ("100", 128, 128),
        ("181", 128, 128),
        ("182", 256, 256),
        ("300", 256, 256),
        ("700", 512, 512),
        ("1000", 1024, 1024),
        ("2500", 2048, 2048),
        ("3000", 4096, 2048),
        ("16384", 4096, 2048),
        ("65536", 4096, 2048),
    ] {
        let mut storage = Memory::default();
        storage.set_string("kymo.tile_cache_mb", saved.into());
        storage.set_string("kymo.gpu_memory_mb", saved.into());
        let loaded = Settings::load(Some(&storage));
        assert_eq!(
            (loaded.tile_cache_mb, loaded.gpu_memory_mb),
            (tile, gpu),
            "{saved}"
        );
    }
    // As near either way: the smaller.
    assert_eq!(nearest(4, &[2, 8]), 2);
}

/// The disk cache's limit: Off, Session and Unlimited stay so, however near a size is to
/// them, and any other size is the nearest one offered; the default is offered.
#[test]
fn the_disk_cache_limit_snaps_but_off_session_and_unlimited_stay() {
    assert!(DISK_CACHE_MB.contains(&Settings::default().disk_cache_mb));
    for (saved, limit) in [
        ("0", OFF),
        ("1", 1024),
        ("3000", 2048),
        ("4000", 5120),
        ("100000", 10240),
        (&*(SESSION - 1).to_string(), 10240),
        (&*SESSION.to_string(), SESSION),
        (&*UNLIMITED.to_string(), UNLIMITED),
    ] {
        let mut storage = Memory::default();
        storage.set_string("kymo.disk_cache_mb", saved.into());
        assert_eq!(
            Settings::load(Some(&storage)).disk_cache_mb,
            limit,
            "{saved}"
        );
    }
    assert_eq!(disk_cache_text(OFF), "Off");
    assert_eq!(disk_cache_text(SESSION), "Session");
    assert_eq!(disk_cache_text(5120), "5 GB");
    assert_eq!(disk_cache_text(UNLIMITED), "Unlimited");
}

/// With the disk cache off, the warning gives the open recording's overviews in memory, or
/// about 10% of each recording without one.
#[test]
fn the_disk_cache_off_warning_gives_the_memory_overviews_take() {
    let warning = disk_cache_off_warning(Some(5_090_922_512));
    assert!(warning.contains("(4.7 GB for this recording)"), "{warning}");
    let warning = disk_cache_off_warning(Some(13_020_880));
    assert!(warning.contains("(12 MB for this recording)"), "{warning}");
    let warning = disk_cache_off_warning(None);
    assert!(warning.contains("(about 10% of"), "{warning}");
}

#[test]
fn budgets_read_in_mb_or_gb() {
    assert_eq!(megabytes_text(128), "128 MB");
    assert_eq!(megabytes_text(512), "512 MB");
    assert_eq!(megabytes_text(1024), "1 GB");
    assert_eq!(megabytes_text(4096), "4 GB");
    assert_eq!(megabytes_text(1536), "1.5 GB");
}

proptest! {
    /// Any descriptions (line breaks, tabs, backslashes, `#`, non-ASCII) and colors read
    /// back exactly.
    #[test]
    fn annotation_colors_read_back(
        colors in proptest::collection::btree_map(any::<String>(), any::<[u8; 3]>(), 0..12),
    ) {
        let mut storage = Memory::default();
        save_annotation_colors(&colors, &mut storage);
        prop_assert_eq!(load_annotation_colors(Some(&storage)), colors);
    }
}

#[test]
fn annotation_colors_skip_unreadable_lines() {
    assert_eq!(load_annotation_colors(None), AnnotationColors::new());
    let mut storage = Memory::default();
    storage.set_string(
        "kymo.annotation_colors",
        "garbage\n#00ff00\tgood\n#zzzzzz\tbad color\n#00ff00\tbad \\x escape\n\
         #0000ff\tline\\nbreak \\\\ and\ttab\n#00ff0\tshort\n"
            .into(),
    );
    let want = AnnotationColors::from([
        ("good".to_owned(), [0, 255, 0]),
        ("line\nbreak \\ and\ttab".to_owned(), [0, 0, 255]),
    ]);
    assert_eq!(load_annotation_colors(Some(&storage)), want);
}

#[test]
fn a_reset_type_color_is_not_read_back() {
    let mut picked = Settings::default();
    picked
        .types
        .set_color(&ChannelKind::EEG, Some(Color32::from_rgb(1, 2, 3)));
    let mut storage = Memory::default();
    picked.save(&mut storage);
    Settings::default().save(&mut storage);
    assert_eq!(Settings::load(Some(&storage)), Settings::default());
}

#[test]
fn awkward_descriptions_read_back() {
    let colors: AnnotationColors = [
        "",
        "#",
        "\r",
        "a\r",
        "\r\n",
        "\n",
        "x\\",
        "\\n",
        "tab\there",
        "\t",
        "ü€😀",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, d)| (d.to_owned(), [i as u8, 0, 255]))
    .collect();
    let mut storage = Memory::default();
    save_annotation_colors(&colors, &mut storage);
    assert_eq!(load_annotation_colors(Some(&storage)), colors);
}

#[test]
fn recent_recordings_are_newest_first_each_once_and_capped() {
    use kymo::recent::Recent;
    use std::path::PathBuf;
    // "/data" is absolute only on Unix; Windows would give it the current drive.
    let path = |k: usize| std::env::temp_dir().join("data").join(format!("r{k}.edf"));
    let mut recent = Recent::default();
    for k in 0..12 {
        recent.add(&path(k));
    }
    assert_eq!(recent.paths().len(), Recent::MAX);
    assert_eq!(recent.get(0), Some(path(11).as_path()));
    assert_eq!(recent.get(9), Some(path(2).as_path()));
    // Opened again, it moves to the top rather than showing twice.
    recent.add(&path(5));
    assert_eq!(recent.get(0), Some(path(5).as_path()));
    assert_eq!(recent.paths().len(), Recent::MAX);
    recent.remove(&path(5));
    assert!(!recent.paths().contains(&path(5)));
    // Relative paths are kept as absolute ones.
    recent.add(&PathBuf::from("relative.edf"));
    assert!(recent.get(0).unwrap().is_absolute());
    recent.clear();
    assert!(recent.paths().is_empty());
}

#[test]
fn recent_labels_name_the_folder_only_for_names_shared() {
    use kymo::recent::Recent;
    use std::path::Path;
    let mut recent = Recent::default();
    recent.add(Path::new("/data/day1/sleep.edf"));
    recent.add(Path::new("/data/other.bdf"));
    recent.add(Path::new("/data/day2/sleep.edf"));
    assert_eq!(
        recent.labels(),
        ["sleep.edf — day2", "other.bdf", "sleep.edf — day1"]
    );
}

#[test]
fn recent_recordings_are_kept_in_storage() {
    use kymo::recent::Recent;
    use std::path::Path;
    let mut recent = Recent::default();
    recent.add(Path::new("/data/a.edf"));
    recent.add(Path::new("/data/with\nbreak and \\ backslash.bdf"));
    let mut storage = Memory::default();
    recent.save(&mut storage);
    assert_eq!(Recent::load(Some(&storage)), recent);
    assert_eq!(Recent::load(None), Recent::default());
    storage.set_string("kymo.recent", "\n/x.edf\n/x.edf\n".into());
    assert_eq!(Recent::load(Some(&storage)).paths().len(), 1);
}

/// DC removal centers brain signals, EOG, ECG, EMG and respiration, not misc, other
/// biological signals, MNE's other types and types kymo doesn't know (and never stim);
/// misc, other biological signals, respiration and those others are scaled each channel on
/// its own.
#[test]
fn per_type_defaults() {
    use crate::kinds::*;
    let types = Settings::default().types;
    let named = ChannelKind::named;
    let shared = [
        Eeg,
        Mag,
        Grad,
        RefMeg,
        Eog,
        named("HEOG"),
        Ecg,
        Emg,
        Seeg,
        Ecog,
        Dbs,
    ];
    for kind in &shared {
        assert!(types.remove_dc(kind), "{kind:?}");
        assert!(!types.own_scale(kind), "{kind:?}");
    }
    assert!(types.remove_dc(&Resp) && types.own_scale(&Resp));
    for kind in [
        Misc,
        Bio,
        named("SaO2"),
        named("Temperature"),
        named("Gaze"),
    ] {
        assert!(!types.remove_dc(&kind), "{kind:?}");
        assert!(types.own_scale(&kind), "{kind:?}");
    }
    assert!(!types.remove_dc(&Stim) && !types.own_scale(&Stim));
    // Ticked, stim stays as it is.
    let mut ticked = TypeSettings::default();
    ticked.set_remove_dc(&Stim, true);
    ticked.set_own_scale(&Stim, true);
    assert!(!ticked.remove_dc(&Stim) && !ticked.own_scale(&Stim));
}

/// Settings per type kept before types were names, a key per setting and type by MNE's
/// name, are read once and then kept the new way.
#[test]
fn old_per_type_keys_are_read_once() {
    let mut storage = Memory::default();
    for (key, value) in [
        ("kymo.color.eog", "#0a0b0c"),
        ("kymo.remove_dc.eeg", "false"),
        ("kymo.own_scale.ref_meg", "true"),
        ("kymo.own_scale.misc", "true"),
        ("kymo.remove_dc.bio", "nonsense"),
    ] {
        storage.set_string(key, value.into());
    }
    let loaded = Settings::load(Some(&storage));
    let types = &loaded.types;
    assert_eq!(
        types.color(&ChannelKind::EOG),
        Some(Color32::from_rgb(10, 11, 12))
    );
    assert!(!types.remove_dc(&ChannelKind::EEG));
    assert!(types.own_scale(&ChannelKind::REF_MEG));
    // As they start: nothing set.
    assert!(types.own_scale(&ChannelKind::MISC));
    assert!(!types.remove_dc(&ChannelKind::BIO));
    assert_eq!(types.entries().count(), 3);
    loaded.save(&mut storage);
    assert!(storage.0.keys().all(|k| !k.starts_with("kymo.color.")));
    assert!(storage.0.contains_key("kymo.channel_types"));
    assert_eq!(Settings::load(Some(&storage)), loaded);
    // Once kept the new way, old keys don't count.
    storage.set_string("kymo.remove_dc.eog", "false".into());
    assert!(
        Settings::load(Some(&storage))
            .types
            .remove_dc(&ChannelKind::EOG)
    );
}

#[test]
fn channel_types_skip_unreadable_lines_and_values() {
    let mut storage = Memory::default();
    storage.set_string(
        "kymo.channel_types",
        "garbage\n#0000ff\ttrue\t\tGaze\n\tfalse\tmaybe\tECG\n#00ff00\t\t\t\n\
         #zzzzzz\tfalse\ttrue\tPupil\n\t\t\tbad \\x escape\n"
            .into(),
    );
    let types = Settings::load(Some(&storage)).types;
    let gaze = ChannelKind::named("gaze");
    assert_eq!(types.color(&gaze), Some(Color32::from_rgb(0, 0, 255)));
    assert!(types.remove_dc(&gaze));
    assert!(types.own_scale(&gaze));
    assert!(!types.remove_dc(&ChannelKind::ECG));
    assert!(!types.own_scale(&ChannelKind::ECG));
    let pupil = ChannelKind::named("Pupil");
    assert_eq!(types.color(&pupil), None);
    assert!(!types.remove_dc(&pupil) && types.own_scale(&pupil));
    let names: Vec<&str> = types.entries().map(|t| t.name.as_str()).collect();
    assert_eq!(names.len(), 3, "{names:?}");
}

proptest! {
    /// Any names (line breaks, tabs, backslashes, non-ASCII) and settings read back.
    #[test]
    fn channel_types_read_back(
        set in proptest::collection::vec(
            ("(?s)\\S.{0,12}", proptest::option::of(any::<[u8; 3]>()), any::<bool>(), any::<bool>()),
            0..8,
        ),
    ) {
        let mut types = TypeSettings::default();
        for (name, color, dc, own) in &set {
            let kind = ChannelKind::named(name);
            types.set_color(&kind, color.map(|[r, g, b]| Color32::from_rgb(r, g, b)));
            types.set_remove_dc(&kind, *dc);
            types.set_own_scale(&kind, *own);
        }
        let settings = Settings { types, ..Settings::default() };
        let mut storage = Memory::default();
        settings.save(&mut storage);
        prop_assert_eq!(Settings::load(Some(&storage)), settings);
    }
}

#[test]
fn update_checks_are_remembered() {
    let mut storage = Memory::default();
    assert_eq!(UpdateState::load(Some(&storage)), UpdateState::default());
    let state = UpdateState {
        last_check: Some("2026-11-23T12:34:56Z".parse().unwrap()),
        skipped: Some(semver::Version::parse("0.2.0-rc.1").unwrap()),
    };
    state.save(&mut storage);
    assert_eq!(UpdateState::load(Some(&storage)), state);
    storage.set_string("kymo.update_checked", "soon".into());
    storage.set_string("kymo.update_skipped", "v0.2".into());
    assert_eq!(UpdateState::load(Some(&storage)), UpdateState::default());
    UpdateState::default().save(&mut storage);
    assert!(!storage.0.contains_key("kymo.update_checked"));
    assert!(!storage.0.contains_key("kymo.update_skipped"));
}
