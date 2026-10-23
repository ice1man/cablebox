//! Display outputs (connectors) from /sys/class/drm, and which one to render on.
//!
//! The KMS backend renders on one output, chosen once at startup through
//! `SLINT_DRM_OUTPUT` (e.g. "HDMI-A-1"). Left alone it takes the first connected output,
//! which on laptops is the built-in panel, so "Auto" prefers an external screen instead.

use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
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
            // "card0-HDMI-A-1" → "HDMI-A-1"
            let name = file.split_once('-')?.1.to_string();
            if name.starts_with("Writeback") {
                return None;
            }
            let read = |f: &str| std::fs::read_to_string(p.join(f)).unwrap_or_default();
            Some(Output {
                connected: read("status").trim() == "connected",
                modes: read("modes").lines().map(str::to_string).collect(),
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

/// What startup decided to render on (for the Display page).
static IN_USE: OnceLock<Option<String>> = OnceLock::new();

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
        match saved_mode.parse::<usize>() {
            Ok(i) if mode_ok && i < mode_list.len() && std::env::var_os("SLINT_DRM_MODE").is_none() => {
                std::env::set_var("SLINT_DRM_MODE", saved_mode);
                mode = mode_list.get(i).cloned();
            }
            Ok(i) if i >= mode_list.len() => tracing::warn!("saved resolution #{i} doesn't exist on {name}; using its preferred mode"),
            _ => {}
        }

        // The KMS backend only applies a scale given before it starts; setting it here makes
        // the very first layout fill the screen. (Windows keep scaling at runtime.)
        let kms = std::env::var("SLINT_BACKEND").is_ok_and(|b| b.starts_with("linuxkms"));
        let height = mode.as_deref().and_then(mode_height);
        if let Some(h) = height {
            let scale = ui_scale(h, manual_scale);
            if kms && std::env::var_os("SLINT_SCALE_FACTOR").is_none() {
                std::env::set_var("SLINT_SCALE_FACTOR", format!("{scale:.4}"));
            }
            tracing::info!("screen: {}, {}, scale {scale:.2}", label(name), mode.as_deref().unwrap_or("?"));
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
        Output { name: name.into(), connected, modes: vec!["1920x1080".into(), "1280x720".into()] }
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
        assert!(outs.iter().all(|o| o.connected));
    }
}
