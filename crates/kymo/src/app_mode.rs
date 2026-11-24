//! The system's app mode, which the System theme follows where winit can't tell it, and its
//! accent color, which the selection takes (see [`crate::ui::theme`]).
//!
//! The app mode: on Windows read from the registry (Settings → Personalization → Colors →
//! `AppsUseLightTheme`), on Linux from the desktop portal's `color-scheme` setting; both
//! watched for changes. On macOS winit tells it, and the mode stays unknown.
//!
//! Windows: winit and muda ask uxtheme instead (`ShouldAppsUseDarkMode`), which is
//! undocumented and, while Kymo forces a theme on its menus (`native_menu::set_app_mode`),
//! reports the forced one; so egui could keep a forced theme under System. Linux: winit 0.30
//! doesn't read the system's theme at all (X11 says nothing, Wayland only what the app set),
//! so System was always dark. With the mode read here, System becomes Light or Dark before
//! anything is themed, and every part gets the same.
//!
//! The accent color: on Linux the portal's `accent-color`, followed with the color scheme;
//! on macOS `NSColor.controlAccentColor` and on Windows the registry's `AccentColorMenu`
//! (Settings → Personalization → Colors → Accent color), both read again whenever the
//! window gains focus ([`AppMode::refresh_accent`]), as changing them takes the system's
//! settings in front. Unknown where the system has none (GNOME before 47, for one).

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use eframe::egui::Color32;

const UNKNOWN: u8 = 0;
const LIGHT: u8 = 1;
const DARK: u8 = 2;

/// No accent color, in [`AppMode`]'s; a known one has the high byte set.
const NO_ACCENT: u32 = 0;

/// The app mode and the accent color, shared with the thread that watches them.
#[derive(Clone, Debug, Default)]
pub struct AppMode {
    mode: Arc<AtomicU8>,
    accent: Arc<AtomicU32>,
}

impl AppMode {
    /// Not known; [`set`](Self::set) tells it.
    pub fn new() -> AppMode {
        AppMode::default()
    }

    /// Whether apps should be dark; `None` if unknown.
    pub fn dark(&self) -> Option<bool> {
        match self.mode.load(Ordering::Relaxed) {
            LIGHT => Some(false),
            DARK => Some(true),
            _ => None,
        }
    }

    pub fn set(&self, dark: Option<bool>) {
        let value = match dark {
            Some(false) => LIGHT,
            Some(true) => DARK,
            None => UNKNOWN,
        };
        self.mode.store(value, Ordering::Relaxed);
    }

    /// The system's accent color; `None` if unknown.
    pub fn accent(&self) -> Option<Color32> {
        match self.accent.load(Ordering::Relaxed) {
            NO_ACCENT => None,
            packed => {
                let [_, r, g, b] = packed.to_be_bytes();
                Some(Color32::from_rgb(r, g, b))
            }
        }
    }

    pub fn set_accent(&self, accent: Option<Color32>) {
        let packed = accent.map_or(NO_ACCENT, |c| u32::from_be_bytes([1, c.r(), c.g(), c.b()]));
        self.accent.store(packed, Ordering::Relaxed);
    }

    /// Reads the accent color again where no thread follows it: macOS and Windows. Kymo
    /// calls it at the start and whenever the window gains focus. Both reads are quick and
    /// stay in the process (the registry's user hive is in memory).
    pub fn refresh_accent(&self) {
        #[cfg(target_os = "macos")]
        self.set_accent(appkit_accent());
        #[cfg(windows)]
        self.set_accent(registry::accent());
    }

    /// The app mode as the registry has it, kept up to date by a thread that waits for the
    /// registry to change and then wakes `ctx`. Unknown where the value can't be read.
    ///
    /// Each read follows asking for the next change's notice, so no change slips between.
    #[cfg(windows)]
    pub fn watch(ctx: &eframe::egui::Context) -> AppMode {
        let mode = AppMode::new();
        let Some(key) = registry::open() else {
            return mode;
        };
        let changes = registry::Changes::new().filter(|changes| changes.ask(&key));
        mode.set(registry::apps_dark(&key));
        // Without notices, or the thread, the mode is read once and not followed.
        let Some(changes) = changes else {
            return mode;
        };
        let (shared, ctx) = (mode.clone(), ctx.clone());
        let _ = std::thread::Builder::new()
            .name("kymo app mode".into())
            .spawn(move || {
                // Blocked until a value of the key changes (others too, as transparency);
                // a frame only if the mode did.
                while changes.wait() {
                    let asked = changes.ask(&key);
                    let dark = registry::apps_dark(&key);
                    if dark != shared.dark() {
                        shared.set(dark);
                        ctx.request_repaint();
                    }
                    if !asked {
                        break;
                    }
                }
            });
        mode
    }

    /// The color scheme and accent color the desktop portal reports, kept up to date by a
    /// thread that follows their changes and then wakes `ctx`. Unknown without a session bus or portal.
    ///
    /// Waits briefly for the first answer, so that the first frame has the right theme.
    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn watch(ctx: &eframe::egui::Context) -> AppMode {
        match zbus::blocking::Connection::session() {
            Ok(connection) => AppMode::watch_connection(connection, ctx),
            Err(_) => AppMode::new(),
        }
    }

    /// winit tells the system's appearance on macOS: unknown here.
    #[cfg(target_os = "macos")]
    pub fn watch(_: &eframe::egui::Context) -> AppMode {
        AppMode::new()
    }

    /// As [`watch`](Self::watch), on a given connection (a test's, to a fake portal).
    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn watch_connection(
        connection: zbus::blocking::Connection,
        ctx: &eframe::egui::Context,
    ) -> AppMode {
        let mode = AppMode::new();
        let (shared, ctx) = (mode.clone(), ctx.clone());
        let (first_tx, first_rx) = std::sync::mpsc::channel();
        // Without the thread, the mode stays unknown.
        let _ = std::thread::Builder::new()
            .name("kymo app mode".into())
            .spawn(move || {
                portal::follow(&connection, |setting| {
                    let changed = match setting {
                        portal::Setting::Dark(dark) => {
                            let changed = dark != shared.dark();
                            shared.set(dark);
                            // The receiver is gone after the first answer, or the wait.
                            let _ = first_tx.send(());
                            changed
                        }
                        portal::Setting::Accent(accent) => {
                            let changed = accent != shared.accent();
                            shared.set_accent(accent);
                            changed
                        }
                    };
                    if changed {
                        ctx.request_repaint();
                    }
                });
            });
        // Ends at once if the thread ends without an answer.
        let _ = first_rx.recv_timeout(std::time::Duration::from_millis(500));
        mode
    }
}

/// Whether a `color-scheme` value of the desktop portal (`org.freedesktop.appearance`) asks
/// for dark apps: 1 does; 2 asks for light ones and 0, no preference, is light too (GNOME
/// reports its default as 0). Other values are unknown.
pub fn color_scheme_dark(value: u32) -> Option<bool> {
    match value {
        0 | 2 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// The color an `accent-color` value of the desktop portal (`org.freedesktop.appearance`)
/// gives, as red, green and blue from 0 to 1; any outside that range means none is set.
pub fn portal_accent(rgb: [f64; 3]) -> Option<Color32> {
    if !rgb.iter().all(|c| (0.0..=1.0).contains(c)) {
        return None;
    }
    let [r, g, b] = rgb.map(|c| (c * 255.0).round() as u8);
    Some(Color32::from_rgb(r, g, b))
}

/// The accent color AppKit reports, in sRGB; `None` if it has none in that space.
#[cfg(target_os = "macos")]
fn appkit_accent() -> Option<Color32> {
    use objc2_app_kit::{NSColor, NSColorSpace};
    let color =
        NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    let channel = |c: f64| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(Color32::from_rgb(
        channel(color.redComponent()),
        channel(color.greenComponent()),
        channel(color.blueComponent()),
    ))
}

#[cfg(all(unix, not(target_os = "macos")))]
mod portal {
    use super::{color_scheme_dark, portal_accent};
    use eframe::egui::Color32;
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedValue, Value};

    const DESTINATION: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const INTERFACE: &str = "org.freedesktop.portal.Settings";
    const NAMESPACE: &str = "org.freedesktop.appearance";
    const KEY: &str = "color-scheme";
    const ACCENT: &str = "accent-color";

    /// A setting the portal reported: the app mode, or the accent color.
    pub enum Setting {
        Dark(Option<bool>),
        Accent(Option<Color32>),
    }

    /// The number in a value, which `Read` wraps in a second variant (as the portal's
    /// documentation says it does since version 1, though old ones sent it bare).
    fn number(value: &Value<'_>) -> Option<u32> {
        match value {
            Value::Value(inner) => number(inner),
            other => u32::try_from(other).ok(),
        }
    }

    /// The color in an `accent-color` value, three doubles, wrapped like [`number`]'s.
    fn color(value: &Value<'_>) -> Option<Color32> {
        match value {
            Value::Value(inner) => color(inner),
            Value::Structure(fields) => match fields.fields() {
                [r, g, b] => portal_accent([
                    f64::try_from(r).ok()?,
                    f64::try_from(g).ok()?,
                    f64::try_from(b).ok()?,
                ]),
                _ => None,
            },
            _ => None,
        }
    }

    /// Calls `publish` with the current accent color and mode, then again at each change,
    /// until the connection ends. Subscribed before the first reads, so no change slips
    /// between. The mode comes second: its first answer ends the wait at the start.
    pub fn follow(connection: &Connection, publish: impl Fn(Setting)) {
        let Ok(proxy) = Proxy::new(connection, DESTINATION, PATH, INTERFACE) else {
            return;
        };
        let Ok(changes) = proxy.receive_signal("SettingChanged") else {
            return;
        };
        let read = |key: &str| {
            proxy
                .call::<_, _, OwnedValue>("Read", &(NAMESPACE, key))
                .ok()
        };
        publish(Setting::Accent(
            read(ACCENT).and_then(|value| color(&value)),
        ));
        let dark = read(KEY)
            .and_then(|value| number(&value))
            .and_then(color_scheme_dark);
        publish(Setting::Dark(dark));
        for message in changes {
            let body = message.body();
            let Ok((namespace, key, value)) = body.deserialize::<(String, String, OwnedValue)>()
            else {
                continue;
            };
            if namespace != NAMESPACE {
                continue;
            }
            match key.as_str() {
                KEY => publish(Setting::Dark(number(&value).and_then(color_scheme_dark))),
                ACCENT => publish(Setting::Accent(color(&value))),
                _ => {}
            }
        }
    }
}

#[cfg(windows)]
mod registry {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_QUERY_VALUE, REG_NOTIFY_CHANGE_LAST_SET,
        REG_NOTIFY_THREAD_AGNOSTIC, RRF_RT_REG_DWORD, RegCloseKey, RegGetValueW,
        RegNotifyChangeKeyValue, RegOpenKeyExW,
    };
    use windows_sys::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};

    /// The key holding the Colors settings, open for reading and change notices.
    pub struct Key(HKEY);

    // SAFETY: a registry key handle may be used from any thread.
    #[allow(unsafe_code)]
    unsafe impl Send for Key {}

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the handle came from `RegOpenKeyExW` and is closed once.
            #[allow(unsafe_code)]
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    pub fn open() -> Option<Key> {
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: a static path and a pointer to a local the call fills.
        #[allow(unsafe_code)]
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                windows_sys::w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                0,
                KEY_NOTIFY | KEY_QUERY_VALUE,
                &mut key,
            )
        };
        (status == ERROR_SUCCESS).then_some(Key(key))
    }

    /// Whether the app mode is dark: `AppsUseLightTheme` 0. `None` without the value
    /// (Windows before 10 1607, where apps are light).
    pub fn apps_dark(key: &Key) -> Option<bool> {
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: `key` is open; the call writes at most `size` bytes into `value`.
        #[allow(unsafe_code)]
        let status = unsafe {
            RegGetValueW(
                key.0,
                std::ptr::null(),
                windows_sys::w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut value).cast(),
                &mut size,
            )
        };
        (status == ERROR_SUCCESS).then_some(value == 0)
    }

    /// The accent color: `AccentColorMenu` under Explorer's `Accent` key, as 0xAABBGGRR.
    /// `None` without the value.
    pub fn accent() -> Option<eframe::egui::Color32> {
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: static strings; the call writes at most `size` bytes into `value`.
        #[allow(unsafe_code)]
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                windows_sys::w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent"),
                windows_sys::w!("AccentColorMenu"),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut value).cast(),
                &mut size,
            )
        };
        let [r, g, b, _] = value.to_le_bytes();
        (status == ERROR_SUCCESS).then(|| eframe::egui::Color32::from_rgb(r, g, b))
    }

    /// Notices of changes to a key's values, signaled on an event: asked for before the
    /// key is read, so that a change while it is read is noticed too.
    pub struct Changes(HANDLE);

    // SAFETY: an event handle may be used from any thread, and the notice is asked for
    // thread-agnostic, so it outlives the thread that asked.
    #[allow(unsafe_code)]
    unsafe impl Send for Changes {}

    impl Drop for Changes {
        fn drop(&mut self) {
            // SAFETY: the handle came from `CreateEventW` and is closed once.
            #[allow(unsafe_code)]
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    impl Changes {
        pub fn new() -> Option<Changes> {
            // SAFETY: an unnamed auto-reset event, not signaled, with default security.
            #[allow(unsafe_code)]
            let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
            (!event.is_null()).then_some(Changes(event))
        }

        /// Asks for the event to be signaled at the next change to a value of `key`;
        /// `false` if it can't be.
        pub fn ask(&self, key: &Key) -> bool {
            // SAFETY: `key` is open and the event alive; asynchronous, so the call returns
            // at once and signals the event later.
            #[allow(unsafe_code)]
            let status = unsafe {
                RegNotifyChangeKeyValue(
                    key.0,
                    0,
                    REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_THREAD_AGNOSTIC,
                    self.0,
                    1,
                )
            };
            status == ERROR_SUCCESS
        }

        /// Waits until the change asked about; `false` if it can't.
        pub fn wait(&self) -> bool {
            // SAFETY: the event is alive.
            #[allow(unsafe_code)]
            let status = unsafe { WaitForSingleObject(self.0, INFINITE) };
            status == WAIT_OBJECT_0
        }
    }
}
