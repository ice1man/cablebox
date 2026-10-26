//! Persistent settings in `~/.config/cablebox/config.toml` (mode 0600: holds the server password).

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    #[default]
    None,
    Basic,
    Digest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Server {
    pub url: String,
    pub auth: AuthMode,
    pub user: String,
    pub pass: String,
    /// Tvheadend streaming profile (`pass` keeps the original stream; `matroska`/`htsp` also work).
    pub profile: String,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            url: String::new(),
            auth: AuthMode::Digest,
            user: String::new(),
            pass: String::new(),
            profile: "pass".into(),
        }
    }
}

impl Server {
    /// Basic auth over plain HTTP sends the password readable to anyone on the network.
    pub fn sends_cleartext_password(&self) -> bool {
        self.auth == AuthMode::Basic && !self.user.is_empty() && self.url.trim().to_ascii_lowercase().starts_with("http://")
    }

    pub fn tvh_auth(&self) -> tvh::Auth {
        let (user, pass) = (self.user.clone(), self.pass.clone());
        match self.auth {
            _ if self.user.is_empty() => tvh::Auth::None,
            AuthMode::None => tvh::Auth::None,
            AuthMode::Basic => tvh::Auth::Basic { user, pass },
            AuthMode::Digest => tvh::Auth::Digest { user, pass },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub last_channel: Option<String>,
    pub favorites: Vec<String>,
    /// `all`, `favorites` or `tag:<uuid>`.
    pub filter: String,
    pub volume: Option<i64>,
    /// 4-digit PIN protecting Settings, updates and deleting recordings. Remove this line to reset it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    /// Settings → System → Debug info overlay.
    pub debug_overlay: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Remote {
    pub cec: bool,
    /// Device name shown on the TV's CEC menu.
    pub cec_name: String,
    /// SSH server state chosen in Settings, re-applied at startup. `None` leaves the
    /// system's own setting alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh: Option<bool>,
}

impl Default for Remote {
    fn default() -> Self {
        Self { cec: true, cec_name: "CableBox".into(), ssh: None }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    /// Screen to render on ("HDMI-A-1"); empty = Auto (an external screen if connected,
    /// otherwise the built-in one). Applied on restart.
    pub output: String,
    /// Index into the chosen screen's mode list, or empty for its preferred mode.
    /// Applied on restart.
    pub mode: String,
    /// Overrides the automatic 720p-based UI scale.
    pub scale: Option<f32>,
    /// Blend frames to the screen's refresh rate (mpv interpolation).
    pub smooth_motion: bool,
    pub quality: Quality,
    /// Use the GPU to decode video, scale it and generate frames.
    pub hw_accel: bool,
    /// Refresh target: draw on every Nth screen refresh (1–4) to save GPU and power.
    pub refresh_divisor: u32,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            output: String::new(),
            mode: String::new(),
            scale: None,
            smooth_motion: false,
            quality: Quality::Standard,
            hw_accel: true,
            refresh_divisor: 1,
        }
    }
}

/// Settings → Sound.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sound {
    /// mpv audio device name ("alsa/hdmi:CARD=…"); empty = Auto.
    pub device: String,
    pub format: AudioFormat,
    /// Compress loud and quiet passages closer together.
    pub night_mode: bool,
    /// Allow volume up to 130%.
    pub volume_boost: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    /// Downmixed to two channels: right for TV speakers, works everywhere.
    #[default]
    Stereo,
    /// Decoded multichannel PCM (5.1) for a receiver or soundbar.
    Surround,
    /// Dolby Digital sent untouched over HDMI/S/PDIF for the receiver to decode.
    Passthrough,
}

/// What the player actually does, given the settings and whether there's a real GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoMode {
    pub hwdec: bool,
    pub smooth_motion: bool,
    pub quality: Quality,
}

impl Display {
    /// Without hardware acceleration (switched off, or only a software renderer) video is
    /// decoded on the CPU and Smooth motion / High quality stay off. Smooth motion draws on
    /// every refresh, so it's also off below a Full refresh target. The saved choices are kept
    /// for when they apply again.
    pub fn effective(&self, gpu_ok: bool) -> VideoMode {
        let hw = self.hw_accel && gpu_ok;
        VideoMode {
            hwdec: hw,
            smooth_motion: hw && self.smooth_motion && self.refresh_divisor <= 1,
            quality: if hw { self.quality } else { Quality::Standard },
        }
    }
}

/// Video scaling quality.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    /// mpv's defaults: fine everywhere, including a Raspberry Pi.
    #[default]
    Standard,
    /// Sharper scaling and debanding for strong GPUs.
    High,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Updates {
    pub auto_check: bool,
    pub check_interval_hours: u64,
}

impl Default for Updates {
    fn default() -> Self {
        Self { auto_check: true, check_interval_hours: 24 }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Clock {
    /// 24-hour times ("20:05") instead of "8:05 PM".
    pub hour24: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: Server,
    pub ui: Ui,
    pub clock: Clock,
    pub remote: Remote,
    pub display: Display,
    pub sound: Sound,
    pub updates: Updates,
}

impl Config {
    pub fn path() -> PathBuf {
        if let Ok(p) = std::env::var("CABLEBOX_CONFIG") {
            return p.into();
        }
        directories::ProjectDirs::from("", "", "cablebox")
            .map(|d| d.config_dir().join("config.toml"))
            .unwrap_or_else(|| PathBuf::from("cablebox.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        match fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Writes atomically with owner-only permissions.
    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(toml::to_string_pretty(self)?.as_bytes())?;
        f.sync_all()?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
    }

    pub fn is_configured(&self) -> bool {
        !self.server.url.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_uses_defaults() {
        let c: Config = toml::from_str("[server]\nurl = \"http://tv:9981\"\n").unwrap();
        assert_eq!(c.server.url, "http://tv:9981");
        assert_eq!(c.server.profile, "pass");
        assert!(c.remote.cec);
        assert!(c.updates.auto_check);
        assert!(!c.display.smooth_motion);
        assert_eq!(c.display.quality, Quality::Standard);
    }

    #[test]
    fn hardware_acceleration_gates_gpu_features() {
        let mut d = Display { smooth_motion: true, quality: Quality::High, ..Default::default() };
        assert!(d.hw_accel); // on by default
        let on = VideoMode { hwdec: true, smooth_motion: true, quality: Quality::High };
        let off = VideoMode { hwdec: false, smooth_motion: false, quality: Quality::Standard };
        assert_eq!(d.effective(true), on);
        assert_eq!(d.effective(false), off); // software renderer
        d.hw_accel = false;
        assert_eq!(d.effective(true), off);
        assert_eq!((d.smooth_motion, d.quality), (true, Quality::High)); // choices kept
        d.hw_accel = true;
        d.refresh_divisor = 2; // below Full: no Smooth motion, quality unaffected
        assert_eq!(d.effective(true), VideoMode { hwdec: true, smooth_motion: false, quality: Quality::High });
        let c: Config = toml::from_str("[display]\nsmooth_motion = true\n").unwrap();
        assert!(c.display.hw_accel); // configs from before the switch
    }

    #[test]
    fn refresh_target_defaults_to_full() {
        assert_eq!(Display::default().refresh_divisor, 1);
        let c: Config = toml::from_str("[display]\nrefresh_divisor = 2\n").unwrap();
        assert_eq!(c.display.refresh_divisor, 2);
    }

    #[test]
    fn sound_settings_load() {
        let c: Config = toml::from_str("[server]\nurl = \"http://tv:9981\"\n").unwrap();
        assert_eq!(c.sound, Sound::default());
        assert_eq!(c.sound.format, AudioFormat::Stereo);
        let c: Config = toml::from_str("[sound]\nformat = \"passthrough\"\nnight_mode = true\n").unwrap();
        assert_eq!((c.sound.format, c.sound.night_mode, c.sound.volume_boost), (AudioFormat::Passthrough, true, false));
    }

    #[test]
    fn video_settings_load() {
        let c: Config = toml::from_str("[display]\nsmooth_motion = true\nquality = \"high\"\n").unwrap();
        assert!(c.display.smooth_motion);
        assert_eq!(c.display.quality, Quality::High);
    }

    #[test]
    fn roundtrip() {
        let mut c = Config::default();
        c.ui.favorites = vec!["a".into()];
        c.server.auth = AuthMode::Basic;
        let back: Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn cleartext_password_detection() {
        let mut s = Server { url: "http://tv:9981".into(), auth: AuthMode::Basic, user: "u".into(), ..Default::default() };
        assert!(s.sends_cleartext_password());
        s.auth = AuthMode::Digest;
        assert!(!s.sends_cleartext_password());
        s.auth = AuthMode::Basic;
        s.url = "https://tv.example".into();
        assert!(!s.sends_cleartext_password());
    }

    #[test]
    fn empty_user_means_no_auth() {
        let s = Server { url: "x".into(), ..Default::default() };
        assert_eq!(s.tvh_auth(), tvh::Auth::None);
    }
}
