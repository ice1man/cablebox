//! Display outputs (connectors) from /sys/class/drm, and which one to render on.
//!
//! The KMS backend renders on one output, chosen once at startup through
//! `SLINT_DRM_OUTPUT` (e.g. "HDMI-A-1"). Left alone it takes the first connected output,
//! which on laptops is the built-in panel, so "Auto" prefers an external screen instead.
//!
//! Started from a desktop (Wayland/X11), CableBox opens in a window instead and none of
//! this applies.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// GPU device it belongs to: "card0".
    pub card: String,
    /// Connector name as the KMS backend expects it: "HDMI-A-1", "eDP-1", "DP-2".
    pub name: String,
    pub connected: bool,
    /// Modes in the kernel's order ("1920x1080", ...); `SLINT_DRM_MODE` indexes this list.
    pub modes: Vec<String>,
}

/// Connectors of all GPUs, sorted by card then name (writeback connectors skipped).
pub fn outputs() -> Vec<Output> {
    outputs_in(Path::new("/sys/class/drm"))
}

fn outputs_in(dir: &Path) -> Vec<Output> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.join("status").exists()).collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| {
            let file = p.file_name()?.to_str()?;
            // "card0-HDMI-A-1" → "card0", "HDMI-A-1"
            let (card, name) = file.split_once('-')?;
            let (card, name) = (card.to_string(), name.to_string());
            if name.starts_with("Writeback") {
                return None;
            }
            let read = |f: &str| std::fs::read_to_string(p.join(f)).unwrap_or_default();
            Some(Output {
                connected: read("status").trim() == "connected",
                modes: read("modes").lines().map(str::to_string).collect(),
                card,
                name,
            })
        })
        .collect()
}

/// Laptop/tablet panels rather than a TV or monitor.
pub fn is_internal(name: &str) -> bool {
    ["eDP", "LVDS", "DSI"].iter().any(|p| name.starts_with(p))
}

/// Friendly name: "Built-in screen", "HDMI 1", "DisplayPort 2", "VGA 1".
pub fn label(name: &str) -> String {
    if is_internal(name) {
        return "Built-in screen".into();
    }
    let (kind, num) = name.rsplit_once('-').unwrap_or((name, ""));
    let kind = match kind {
        "HDMI-A" | "HDMI-B" => "HDMI",
        "DP" => "DisplayPort",
        "DVI-I" | "DVI-D" | "DVI-A" => "DVI",
        "Composite" | "SVIDEO" | "Component" => "TV out",
        other => other,
    };
    if num.is_empty() { kind.to_string() } else { format!("{kind} {num}") }
}

/// The output to use: the saved one if it's connected, otherwise Auto (first connected
/// external screen, else the built-in one). `None` when nothing is connected (or no DRM),
/// leaving the choice to the backend.
pub fn pick(outputs: &[Output], saved: &str) -> Option<String> {
    let connected = || outputs.iter().filter(|o| o.connected);
    if !saved.is_empty() && connected().any(|o| o.name == saved) {
        return Some(saved.to_string());
    }
    connected()
        .find(|o| !is_internal(&o.name))
        .or_else(|| connected().next())
        .map(|o| o.name.clone())
}

/// The UI is laid out for this many logical pixels of height.
pub const DESIGN_HEIGHT: f32 = 720.0;

/// Height of a mode string such as "1920x1080" (or "1920x1080i").
pub fn mode_height(mode: &str) -> Option<u32> {
    let (_, h) = mode.split_once('x')?;
    h.trim_end_matches(|c: char| !c.is_ascii_digit()).parse().ok()
}

/// Scale that fits the 1280x720 design to a screen this many pixels tall, unless a
/// manual Interface size is set. Clamped to a sane range.
pub fn ui_scale(screen_height: u32, manual: Option<f32>) -> f32 {
    manual.unwrap_or(screen_height as f32 / DESIGN_HEIGHT).clamp(0.5, 4.0)
}

/// Full screen on KMS (a box, started by the service or from a console) or a desktop window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    Tv,
    Window,
}

static SESSION: OnceLock<Session> = OnceLock::new();

pub fn session() -> Session {
    *SESSION.get().unwrap_or(&Session::Tv)
}

/// Decides between TV and window mode. Must run before the Slint backend starts: without
/// a desktop it selects the KMS backend explicitly, so the scale set by
/// `configure_backend` applies.
pub fn init_session() -> Session {
    let backend = std::env::var("SLINT_BACKEND").ok();
    let desktop = ["WAYLAND_DISPLAY", "DISPLAY"].iter().any(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()));
    let (session, set_kms) = choose_session(backend.as_deref(), desktop, cfg!(feature = "winit"), cfg!(feature = "kms"));
    if set_kms {
        std::env::set_var("SLINT_BACKEND", "linuxkms");
    }
    tracing::info!("session: {}", if session == Session::Tv { "TV (full screen)" } else { "window" });
    *SESSION.get_or_init(|| session)
}

/// Returns the session and whether to select the KMS backend.
fn choose_session(backend: Option<&str>, desktop: bool, has_winit: bool, has_kms: bool) -> (Session, bool) {
    match backend {
        Some(b) if b.starts_with("linuxkms") => (Session::Tv, false),
        Some(_) => (Session::Window, false),
        None if desktop && has_winit => (Session::Window, false),
        None if has_kms => (Session::Tv, true),
        None => (Session::Window, false),
    }
}

/// OpenGL's renderer name (`GL_RENDERER`), once the window's GL context exists.
static GPU: OnceLock<String> = OnceLock::new();

pub fn gpu() -> Option<&'static str> {
    GPU.get().map(String::as_str)
}

pub fn set_gpu(name: String) {
    tracing::info!("gpu: {name}{}", if is_software_renderer(&name) { " (software rendering, no GPU acceleration)" } else { "" });
    let _ = GPU.set(name);
}

/// Mesa's CPU rasterizers, used when there's no working GPU driver.
pub fn is_software_renderer(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["llvmpipe", "softpipe", "swrast", "software rasterizer"].iter().any(|s| n.contains(s))
}

/// Short form for Settings: "AMD Radeon 760M · radeonsi", "llvmpipe (software)".
pub fn gpu_label(name: &str) -> String {
    let (model, rest) = name.split_once(" (").unwrap_or((name, ""));
    if is_software_renderer(name) {
        return format!("{model} (software)");
    }
    match rest.split([',', ')']).next().map(str::trim).filter(|d| !d.is_empty()) {
        Some(driver) => format!("{model} · {driver}"),
        None => model.to_string(),
    }
}

/// A real GPU is rendering (unknown counts as no until the GL context is up).
pub fn hw_available() -> bool {
    gpu().is_some_and(|g| !is_software_renderer(g))
}

/// What startup decided to render on (for the Display page).
static IN_USE: OnceLock<Option<String>> = OnceLock::new();

/// Refresh rate of the screen in use in millihertz, 0 until known (Smooth motion needs it).
/// A window can learn it late or move to another monitor, so it can change.
static REFRESH_MHZ: AtomicU32 = AtomicU32::new(0);
/// The rate came from the screen itself rather than from mpv's measurement.
static REFRESH_REPORTED: AtomicBool = AtomicBool::new(false);

pub fn refresh_hz() -> Option<f64> {
    let mhz = REFRESH_MHZ.load(Ordering::Relaxed);
    (mhz != 0).then(|| mhz as f64 / 1000.0)
}

pub fn refresh_reported() -> bool {
    REFRESH_REPORTED.load(Ordering::Relaxed)
}

/// Records the refresh rate the screen reports, if plausible; true when it changed.
pub fn set_refresh_hz(hz: f64) -> bool {
    if !(20.0..=500.0).contains(&hz) {
        return false;
    }
    REFRESH_REPORTED.store(true, Ordering::Relaxed);
    let mhz = (hz * 1000.0).round() as u32;
    REFRESH_MHZ.swap(mhz, Ordering::Relaxed) != mhz
}

/// Records a measured rate unless the screen reported one; true when it changed.
pub fn set_measured_refresh_hz(hz: f64) -> bool {
    let mhz = (hz * 1000.0).round() as u32;
    !refresh_reported() && REFRESH_MHZ.swap(mhz, Ordering::Relaxed) != mhz
}

/// Turns noisy measurements into a rate: the same snapped value `STREAK` times in a row.
#[derive(Debug, Default)]
pub struct RefreshProbe {
    candidate: Option<f64>,
    streak: u8,
}

impl RefreshProbe {
    const STREAK: u8 = 3;

    pub fn feed(&mut self, measured: Option<f64>) -> Option<f64> {
        let snapped = measured.and_then(snap_refresh);
        if snapped.is_some() && snapped == self.candidate {
            self.streak = self.streak.saturating_add(1);
        } else {
            self.candidate = snapped;
            self.streak = u8::from(snapped.is_some());
        }
        self.candidate.filter(|_| self.streak >= Self::STREAK)
    }
}

/// Settings → Display → Refresh target: draw on every Nth refresh (1 = every one).
static FRAME_DIVISOR: AtomicU32 = AtomicU32::new(1);

pub fn set_frame_divisor(n: u32) {
    FRAME_DIVISOR.store(n.clamp(1, 4), Ordering::Relaxed);
}

pub fn frame_divisor() -> u32 {
    FRAME_DIVISOR.load(Ordering::Relaxed)
}

/// The Refresh target: the refresh rate divided by the chosen fraction. An unknown refresh
/// rate (a Wayland window) counts as 60 Hz, the usual TV rate.
pub fn target_hz() -> f64 {
    refresh_hz().unwrap_or(60.0) / frame_divisor() as f64
}

/// Frame rate to thin video down to for the Refresh target, when it's below the video's own
/// (dropping frames; never duplicating them). `None`: show every frame.
pub fn frame_cap(video_fps: Option<f64>) -> Option<f64> {
    if frame_divisor() <= 1 {
        return None;
    }
    let target = target_hz();
    (target < video_fps? * 0.98).then_some(target)
}

/// "Full · 119.95 Hz", "Half · 59.98 Hz", or just "Half" while the refresh rate is unknown.
pub fn target_label(refresh: Option<f64>, divisor: u32) -> String {
    let name = match divisor {
        2 => "Half",
        3 => "Third",
        4 => "Quarter",
        _ => "Full",
    };
    match refresh {
        Some(hz) => format!("{name} · {:.2} Hz", hz / divisor.clamp(1, 4) as f64),
        None => name.to_string(),
    }
}

/// Nearest common refresh rate to a measured one (measurements read a little low when
/// frames are missed), or `None` if it isn't within 5% of any.
pub fn snap_refresh(measured: f64) -> Option<f64> {
    const RATES: [f64; 17] =
        [24.0, 25.0, 30.0, 48.0, 50.0, 59.94, 60.0, 72.0, 75.0, 90.0, 100.0, 119.88, 120.0, 144.0, 165.0, 180.0, 240.0];
    RATES
        .into_iter()
        .min_by(|a, b| (a - measured).abs().total_cmp(&(b - measured).abs()))
        .filter(|r| ((r - measured) / r).abs() <= 0.05)
}

/// Exact refresh rate from a mode's timings (59.94 rather than the rounded 60).
fn timings_hz(clock_khz: u32, htotal: u16, vtotal: u16, interlaced: bool, doublescan: bool) -> Option<f64> {
    if htotal == 0 || vtotal == 0 || clock_khz == 0 {
        return None;
    }
    let mut hz = clock_khz as f64 * 1000.0 / (htotal as f64 * vtotal as f64);
    if interlaced {
        hz *= 2.0;
    }
    if doublescan {
        hz /= 2.0;
    }
    Some(hz)
}

/// Opened only to read connector modes (never kept, so it can't hold DRM master).
struct Card(std::fs::File);

impl std::os::fd::AsFd for Card {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl drm::Device for Card {}
impl drm::control::Device for Card {}

/// Refresh rate of the mode the KMS backend will set on `output`: `mode_index`, or the
/// same default it picks (the largest preferred mode).
fn kms_refresh(card: &str, output: &str, mode_index: Option<usize>) -> Option<f64> {
    use drm::control::{Device, ModeFlags, ModeTypeFlags};
    let file = std::fs::OpenOptions::new().read(true).write(true).open(Path::new("/dev/dri").join(card)).ok()?;
    let card = Card(file);
    let res = card.resource_handles().ok()?;
    let conn = res
        .connectors()
        .iter()
        .filter_map(|h| card.get_connector(*h, false).ok())
        .find(|c| format!("{}-{}", c.interface().as_str(), c.interface_id()) == output)?;
    let modes = conn.modes();
    let mode = match mode_index {
        Some(i) => modes.get(i)?,
        None => modes.iter().max_by_key(|m| {
            let (w, h) = m.size();
            (m.mode_type().contains(ModeTypeFlags::PREFERRED), w as u32 * h as u32)
        })?,
    };
    let flags = mode.flags();
    timings_hz(
        mode.clock(),
        mode.hsync().2,
        mode.vsync().2,
        flags.contains(ModeFlags::INTERLACE),
        flags.contains(ModeFlags::DBLSCAN),
    )
    .or(Some(mode.vrefresh() as f64))
}

pub fn in_use() -> Option<&'static str> {
    IN_USE.get().and_then(|o| o.as_deref())
}

/// Chooses the output and mode before the Slint backend starts, via `SLINT_DRM_OUTPUT`
/// and `SLINT_DRM_MODE`. Never sets a value the backend would reject (an unplugged output
/// or a mode index past the end), since that would stop CableBox from starting at all.
pub fn configure_backend(saved_output: &str, saved_mode: &str, manual_scale: Option<f32>) {
    let all = outputs();
    let chosen = pick(&all, saved_output);
    if !saved_output.is_empty() && chosen.as_deref() != Some(saved_output) {
        tracing::warn!("screen {saved_output} is not connected; using {}", chosen.as_deref().unwrap_or("the default"));
    }
    if let Some(name) = &chosen {
        if std::env::var_os("SLINT_DRM_OUTPUT").is_none() {
            std::env::set_var("SLINT_DRM_OUTPUT", name);
        }
        // A saved mode only applies to the screen it was chosen for.
        let mode_list = all.iter().find(|o| &o.name == name).map(|o| o.modes.clone()).unwrap_or_default();
        let mode_ok = chosen.as_deref() == Some(saved_output) || saved_output.is_empty();
        // The mode the backend will use: the saved one if valid, else its preferred (first) one.
        let mut mode = mode_list.first().cloned();
        let mut mode_index = None;
        match saved_mode.parse::<usize>() {
            Ok(i) if mode_ok && i < mode_list.len() && std::env::var_os("SLINT_DRM_MODE").is_none() => {
                std::env::set_var("SLINT_DRM_MODE", saved_mode);
                mode = mode_list.get(i).cloned();
                mode_index = Some(i);
            }
            Ok(i) if i >= mode_list.len() => tracing::warn!("saved resolution #{i} doesn't exist on {name}; using its preferred mode"),
            _ => {}
        }

        // The KMS backend only applies a scale given before it starts; setting it here makes
        // the very first layout fill the screen. (Windows keep scaling at runtime.)
        let kms = std::env::var("SLINT_BACKEND").is_ok_and(|b| b.starts_with("linuxkms"));
        let card = all.iter().find(|o| &o.name == name).map(|o| o.card.as_str()).unwrap_or("card0");
        if let Some(hz) = kms_refresh(card, name, mode_index) {
            set_refresh_hz(hz);
        }
        let height = mode.as_deref().and_then(mode_height);
        if let Some(h) = height {
            let scale = ui_scale(h, manual_scale);
            if kms && std::env::var_os("SLINT_SCALE_FACTOR").is_none() {
                std::env::set_var("SLINT_SCALE_FACTOR", format!("{scale:.4}"));
            }
            let hz = refresh_hz().map(|hz| format!(" @ {hz:.2} Hz")).unwrap_or_default();
            tracing::info!("screen: {}, {}{hz}, scale {scale:.2}", label(name), mode.as_deref().unwrap_or("?"));
        } else {
            tracing::info!("screen: {}", label(name));
        }
    } else {
        tracing::info!("screen: backend default");
    }
    let _ = IN_USE.set(chosen);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(name: &str, connected: bool) -> Output {
        Output { card: "card0".into(), name: name.into(), connected, modes: vec!["1920x1080".into(), "1280x720".into()] }
    }

    #[test]
    fn auto_prefers_external_screens() {
        let laptop = [out("eDP-1", true), out("HDMI-A-1", true), out("DP-1", false)];
        assert_eq!(pick(&laptop, "").as_deref(), Some("HDMI-A-1"));
        let lid_only = [out("eDP-1", true), out("HDMI-A-1", false)];
        assert_eq!(pick(&lid_only, "").as_deref(), Some("eDP-1"));
        assert_eq!(pick(&[out("eDP-1", false)], ""), None);
        assert_eq!(pick(&[], ""), None);
    }

    #[test]
    fn saved_screen_used_only_while_connected() {
        let both = [out("eDP-1", true), out("HDMI-A-1", true)];
        assert_eq!(pick(&both, "eDP-1").as_deref(), Some("eDP-1"));
        let unplugged = [out("eDP-1", true), out("HDMI-A-1", false)];
        assert_eq!(pick(&unplugged, "HDMI-A-1").as_deref(), Some("eDP-1"));
        assert_eq!(pick(&both, "HDMI-A-9").as_deref(), Some("HDMI-A-1"));
    }

    #[test]
    fn scale_from_screen_height() {
        assert_eq!(ui_scale(1080, None), 1.5);
        assert_eq!(ui_scale(720, None), 1.0);
        assert_eq!(ui_scale(2160, None), 3.0);
        assert!((ui_scale(1504, None) - 2.0889).abs() < 0.001);
        assert_eq!(ui_scale(1080, Some(1.25)), 1.25);
        assert_eq!(ui_scale(4320, None), 4.0); // clamped
        assert_eq!(ui_scale(240, None), 0.5);
        assert_eq!(mode_height("1920x1080"), Some(1080));
        assert_eq!(mode_height("1920x1080i"), Some(1080));
        assert_eq!(mode_height("garbage"), None);
    }

    #[test]
    fn session_choice() {
        // The service sets SLINT_BACKEND=linuxkms, even if a desktop is also running.
        assert_eq!(choose_session(Some("linuxkms"), true, true, true), (Session::Tv, false));
        assert_eq!(choose_session(Some("linuxkms-femtovg"), false, true, true), (Session::Tv, false));
        assert_eq!(choose_session(Some("winit"), false, true, true), (Session::Window, false));
        // Launched from a desktop.
        assert_eq!(choose_session(None, true, true, true), (Session::Window, false));
        // From a console: select KMS so the startup scale applies.
        assert_eq!(choose_session(None, false, true, true), (Session::Tv, true));
        // Dev build without KMS.
        assert_eq!(choose_session(None, false, true, false), (Session::Window, false));
        assert_eq!(choose_session(None, true, false, true), (Session::Tv, true));
    }

    #[test]
    fn refresh_from_timings() {
        // CEA 1080p60 and 1080p59.94.
        assert!((timings_hz(148500, 2200, 1125, false, false).unwrap() - 60.0).abs() < 0.001);
        assert!((timings_hz(148352, 2200, 1125, false, false).unwrap() - 59.94).abs() < 0.01);
        // 1080i: fields per second.
        assert!((timings_hz(74250, 2200, 1125, true, false).unwrap() - 60.0).abs() < 0.001);
        assert_eq!(timings_hz(0, 2200, 1125, false, false), None);
        assert_eq!(timings_hz(148500, 0, 1125, false, false), None);
    }

    #[test]
    fn snaps_measured_refresh() {
        assert_eq!(snap_refresh(117.8), Some(119.88));
        assert_eq!(snap_refresh(59.2), Some(59.94));
        assert_eq!(snap_refresh(143.0), Some(144.0));
        assert_eq!(snap_refresh(49.7), Some(50.0));
        assert_eq!(snap_refresh(200.0), None);
        assert_eq!(snap_refresh(10.0), None);
    }

    #[test]
    fn probe_needs_a_stable_measurement() {
        let mut p = RefreshProbe::default();
        assert_eq!(p.feed(Some(91.0)), None); // early, noisy
        assert_eq!(p.feed(Some(117.5)), None);
        assert_eq!(p.feed(Some(118.2)), None);
        assert_eq!(p.feed(Some(119.0)), Some(119.88));
        assert_eq!(p.feed(Some(119.4)), Some(119.88));
        assert_eq!(p.feed(None), None);
        assert_eq!(p.feed(Some(59.9)), None);
    }

    #[test]
    fn software_renderers() {
        assert!(is_software_renderer("llvmpipe (LLVM 19.1.7, 256 bits)"));
        assert!(is_software_renderer("softpipe"));
        assert!(is_software_renderer("Software Rasterizer"));
        assert!(!is_software_renderer("AMD Radeon Graphics (radeonsi, phoenix, LLVM 19.1.7, DRM 3.64, 6.17.0)"));
        assert!(!is_software_renderer("V3D 7.1"));
        assert!(!is_software_renderer("Mesa Intel(R) UHD Graphics 620 (KBL GT2)"));
        assert_eq!(gpu_label("AMD Radeon 760M (radeonsi, phoenix, LLVM 19.1.7, DRM 3.64)"), "AMD Radeon 760M · radeonsi");
        assert_eq!(gpu_label("llvmpipe (LLVM 19.1.7, 256 bits)"), "llvmpipe (software)");
        assert_eq!(gpu_label("V3D 7.1"), "V3D 7.1");
    }

    #[test]
    fn refresh_target_labels() {
        assert_eq!(target_label(Some(60.0), 1), "Full · 60.00 Hz");
        assert_eq!(target_label(Some(60.0), 2), "Half · 30.00 Hz");
        assert_eq!(target_label(Some(119.95), 2), "Half · 59.98 Hz");
        assert_eq!(target_label(Some(144.0), 3), "Third · 48.00 Hz");
        assert_eq!(target_label(Some(144.0), 4), "Quarter · 36.00 Hz");
        assert_eq!(target_label(None, 2), "Half");
    }

    #[test]
    fn friendly_labels() {
        assert_eq!(label("eDP-1"), "Built-in screen");
        assert_eq!(label("LVDS-1"), "Built-in screen");
        assert_eq!(label("HDMI-A-2"), "HDMI 2");
        assert_eq!(label("DP-3"), "DisplayPort 3");
        assert_eq!(label("DVI-I-1"), "DVI 1");
        assert_eq!(label("VGA-1"), "VGA 1");
    }

    #[test]
    fn reads_sysfs_layout() {
        let dir = std::env::temp_dir().join(format!("cablebox-drm-test-{}", std::process::id()));
        let mk = |n: &str, status: &str, modes: &str| {
            let p = dir.join(n);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("status"), status).unwrap();
            std::fs::write(p.join("modes"), modes).unwrap();
        };
        mk("card0-eDP-1", "connected\n", "2256x1504\n");
        mk("card0-HDMI-A-1", "connected\n", "1920x1080\n1280x720\n");
        mk("card0-Writeback-1", "unknown\n", "");
        std::fs::create_dir_all(dir.join("card0")).unwrap(); // the card itself has no status
        let outs = outputs_in(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        let names: Vec<_> = outs.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["HDMI-A-1", "eDP-1"]);
        assert_eq!(outs[0].modes, ["1920x1080", "1280x720"]);
        assert_eq!(outs[0].card, "card0");
        assert!(outs.iter().all(|o| o.connected));
    }
}
