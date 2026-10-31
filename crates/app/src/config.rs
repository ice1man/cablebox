//! Persistent settings in `~/.config/cablebox/config.toml` (mode 0600: holds the server password).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

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
    /// Receiver firmware already offered ("IR1 0.2.0"), so the prompt appears once per version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware_prompted: Option<String>,
    /// Remote buttons taught in Settings, as `"scan:800f0422" = "ok"`. Keep this last: TOML
    /// writes tables after plain values.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub buttons: BTreeMap<String, String>,
}

impl Default for Remote {
    fn default() -> Self {
        Self { cec: true, cec_name: "CableBox".into(), ssh: None, firmware_prompted: None, buttons: BTreeMap::new() }
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

/// Why the settings file couldn't be used.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigProblem {
    pub path: PathBuf,
    pub message: String,
}

impl ConfigProblem {
    /// Comments out the bad lines, after copying the file to a backup. On any failure the file
    /// is left exactly as it was, and the error says why, in a form to show on screen.
    pub fn repair(&self) -> Result<Repaired, String> {
        let text = fs::read_to_string(&self.path).map_err(|e| format!("Couldn't read the file: {e}"))?;
        let (repaired, removed) = repair_text(&text)?;
        let backup = backup_path(&self.path);
        write_atomic(&backup, text.as_bytes()).map_err(|e| format!("Couldn't save a backup: {e:#}"))?;
        write_atomic(&self.path, repaired.as_bytes()).map_err(|e| format!("Couldn't write the file: {e:#}"))?;
        Ok(Repaired { removed, backup })
    }

    /// Moves the file out of the way, never over an earlier backup. Returns where it went.
    pub fn set_aside(&self) -> std::io::Result<PathBuf> {
        let backup = backup_path(&self.path);
        fs::rename(&self.path, &backup)?;
        Ok(backup)
    }
}

/// Most bad lines a repair will comment out before giving up.
const MAX_REPAIRS: usize = 10;

/// Comments out the lines stopping `text` from loading. Returns the new text and each removed
/// line's number and content. Lines keep their numbers, so errors stay easy to find.
fn repair_text(text: &str) -> Result<(String, Vec<(usize, String)>), String> {
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let mut removed = Vec::new();
    loop {
        let current = lines.join("\n");
        let Err(e) = toml::from_str::<Config>(&current) else { return Ok((current, removed)) };
        if removed.len() == MAX_REPAIRS {
            return Err(format!("More than {MAX_REPAIRS} lines have errors."));
        }
        let Some(span) = e.span() else { return Err(describe_parse_error(&current, &e)) };
        let index = current.as_bytes()[..span.start.min(current.len())].iter().filter(|b| **b == b'\n').count();
        let line = lines[index].clone();
        let trimmed = line.trim();
        // Without its header, a section's settings would land in the section above and be
        // silently ignored.
        if trimmed.starts_with('[') {
            return Err(format!("Line {} is a section header, which has to be fixed by hand.", index + 1));
        }
        // Nothing to take out: the error is something this can't fix a line at a time.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return Err(describe_parse_error(&current, &e));
        }
        lines[index] = format!("# Commented out by CableBox: {line}");
        removed.push((index + 1, line));
    }
}

/// A bad line, safe to show on a TV: a value that may be secret is hidden.
pub fn describe_removed(line: &str) -> String {
    let line = line.trim();
    let key = line.split('=').next().unwrap_or(line).trim();
    let lower = key.to_ascii_lowercase();
    if ["pass", "pin", "secret", "token", "key"].iter().any(|s| lower.contains(s)) {
        return format!("{key} = ••••");
    }
    let mut chars = line.chars();
    let short: String = chars.by_ref().take(50).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

/// Writes through a temporary file, with owner-only permissions: this holds passwords.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    let tmp = PathBuf::from(name);
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

/// What a repair did.
#[derive(Debug, Clone, PartialEq)]
pub struct Repaired {
    /// Each commented-out line's number and original content.
    pub removed: Vec<(usize, String)>,
    pub backup: PathBuf,
}

/// The first unused of `config.toml.bad`, `config.toml.bad.1`, ...
fn backup_path(path: &Path) -> PathBuf {
    let name = |suffix: String| {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name)
    };
    std::iter::once(name(".bad".into()))
        .chain((1..).map(|n| name(format!(".bad.{n}"))))
        .find(|p| !p.exists())
        .expect("an unused name")
}

/// One line saying where the error is and what it is.
fn describe_parse_error(text: &str, e: &toml::de::Error) -> String {
    let what = e.message().lines().next().unwrap_or("not valid");
    match e.span() {
        Some(span) => {
            let before = &text.as_bytes()[..span.start.min(text.len())];
            let line = before.iter().filter(|b| **b == b'\n').count() + 1;
            let column = before.iter().rev().take_while(|b| **b != b'\n').count() + 1;
            format!("Line {line}, column {column}: {what}")
        }
        None => what.to_string(),
    }
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

    pub fn load() -> Result<Self, ConfigProblem> {
        Self::load_from(&Self::path())
    }

    /// A missing file is a normal first run. Anything else that stops it loading is a problem,
    /// and the file must then be left alone: it may hold the only copy of the settings.
    fn load_from(path: &Path) -> Result<Self, ConfigProblem> {
        let problem = |message| ConfigProblem { path: path.to_path_buf(), message };
        match fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|e| problem(describe_parse_error(&s, &e))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(problem(format!("Couldn't read it: {e}"))),
        }
    }

    /// Saves, unless the file on disk couldn't be loaded. Returns whether it was written.
    pub fn save_unless(&self, problem: Option<&ConfigProblem>) -> Result<bool> {
        self.save_to_unless(&Self::path(), problem)
    }

    fn save_to_unless(&self, path: &Path, problem: Option<&ConfigProblem>) -> Result<bool> {
        if problem.is_some() {
            tracing::warn!("not saving settings: {} has an error", path.display());
            return Ok(false);
        }
        self.save_to(path)?;
        Ok(true)
    }

    /// Writes atomically with owner-only permissions.
    fn save_to(&self, path: &Path) -> Result<()> {
        write_atomic(path, toml::to_string_pretty(self)?.as_bytes())
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

    /// A new empty folder for one test.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cablebox-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_file_is_a_first_run_and_not_a_problem() {
        let path = temp_dir("missing").join("config.toml");
        assert_eq!(Config::load_from(&path), Ok(Config::default()));
    }

    #[test]
    fn a_broken_file_is_a_problem_naming_where() {
        let path = temp_dir("broken").join("config.toml");
        fs::write(&path, "[server]\nurl = \"http://tv:9981\"\nuser = oops\n").unwrap();
        let problem = Config::load_from(&path).unwrap_err();
        assert_eq!(problem.path, path);
        assert!(problem.message.starts_with("Line 3, column 8: "), "{}", problem.message);
        // A valid file with a wrong type counts too.
        fs::write(&path, "[remote]\ncec = \"yes\"\n").unwrap();
        assert!(Config::load_from(&path).unwrap_err().message.starts_with("Line 2, "));
    }

    #[test]
    fn an_unreadable_file_is_a_problem() {
        // Not text, so it can't be read as settings.
        let path = temp_dir("unreadable").join("config.toml");
        fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();
        assert!(Config::load_from(&path).unwrap_err().message.starts_with("Couldn't read it"));
    }

    #[test]
    fn saving_leaves_a_broken_file_exactly_as_it_was() {
        let path = temp_dir("guard").join("config.toml");
        let broken = "[server]\nurl = \"http://tv:9981\"\npass = \"secret\"\nuser = oops\n";
        fs::write(&path, broken).unwrap();
        let problem = Config::load_from(&path).unwrap_err();
        assert!(!Config::default().save_to_unless(&path, Some(&problem)).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), broken);
        // Once it's dealt with, saving works again.
        assert!(Config::default().save_to_unless(&path, None).unwrap());
        assert_eq!(Config::load_from(&path), Ok(Config::default()));
    }

    #[test]
    fn setting_aside_never_overwrites_a_backup() {
        let path = temp_dir("backup").join("config.toml");
        let problem = ConfigProblem { path: path.clone(), message: String::new() };
        let mut name = path.clone().into_os_string();
        name.push(".bad");
        let first = PathBuf::from(name.clone());
        name.push(".1");
        let second = PathBuf::from(name);

        fs::write(&path, "first").unwrap();
        assert_eq!(problem.set_aside().unwrap(), first);
        assert!(!path.exists());
        fs::write(&path, "second").unwrap();
        assert_eq!(problem.set_aside().unwrap(), second);
        assert_eq!(fs::read_to_string(&first).unwrap(), "first");
        assert_eq!(fs::read_to_string(&second).unwrap(), "second");
    }

    const TYPO: &str = "[server]\nurl = \"http://tv:9981\"\nauth = none\n[remote]\ncec = false\n[remote.buttons]\n\"ir:nec:01:0b\" = \"up\"\n";

    #[test]
    fn repair_comments_out_only_the_bad_line_and_keeps_the_rest() {
        let (text, removed) = repair_text(TYPO).unwrap();
        assert_eq!(removed, [(3, "auth = none".to_string())]);
        assert!(text.contains("# Commented out by CableBox: auth = none\n"));
        let c: Config = toml::from_str(&text).unwrap();
        assert_eq!(c.server.url, "http://tv:9981");
        assert_eq!(c.server.auth, Server::default().auth); // only this one went back to default
        assert!(!c.remote.cec);
        assert_eq!(c.remote.buttons.get("ir:nec:01:0b").unwrap(), "up");
    }

    #[test]
    fn repair_handles_several_bad_lines_and_wrong_types() {
        let (text, removed) = repair_text("[server]\nauth = none\nuser = oops\n[remote]\ncec = \"yes\"\ncec_name = \"Box\"\n").unwrap();
        assert_eq!(removed.iter().map(|r| r.0).collect::<Vec<_>>(), [2, 3, 5]);
        assert_eq!(toml::from_str::<Config>(&text).unwrap().remote.cec_name, "Box");
    }

    #[test]
    fn repair_refuses_a_bad_section_header() {
        for text in ["[server\nurl = \"http://tv:9981\"\n", "[remote]\ncec = true\n[remote]\ncec = false\n"] {
            let e = repair_text(text).unwrap_err();
            assert!(e.contains("section header"), "{e}");
        }
    }

    #[test]
    fn repair_gives_up_after_too_many_bad_lines() {
        let text = format!("[server]\n{}", (0..=MAX_REPAIRS).map(|i| format!("k{i} = oops\n")).collect::<String>());
        assert!(repair_text(&text).unwrap_err().contains("More than"));
    }

    #[test]
    fn repair_makes_a_private_backup_of_the_original() {
        let path = temp_dir("repair").join("config.toml");
        fs::write(&path, TYPO).unwrap();
        let problem = Config::load_from(&path).unwrap_err();
        let done = problem.repair().unwrap();
        assert_eq!(fs::read_to_string(&done.backup).unwrap(), TYPO);
        assert_eq!(fs::metadata(&done.backup).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(Config::load_from(&path).unwrap().server.url, "http://tv:9981");
    }

    #[test]
    fn a_refused_repair_leaves_the_file_alone_with_no_backup() {
        let dir = temp_dir("refused");
        let path = dir.join("config.toml");
        fs::write(&path, "[server\nurl = 1\n").unwrap();
        let problem = Config::load_from(&path).unwrap_err();
        assert!(problem.repair().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "[server\nurl = 1\n");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn removed_lines_shown_on_screen_hide_secrets() {
        assert_eq!(describe_removed("  auth = none"), "auth = none");
        assert_eq!(describe_removed("pass = hunter2"), "pass = ••••");
        assert_eq!(describe_removed("pin = 12x4"), "pin = ••••");
        assert!(describe_removed(&"x".repeat(80)).ends_with('…'));
    }

    #[test]
    fn learned_buttons_round_trip() {
        let old: Config = toml::from_str("[remote]\ncec = false\n").unwrap();
        assert!(old.remote.buttons.is_empty()); // configs from before learning
        assert!(!toml::to_string_pretty(&old).unwrap().contains("buttons"));
        let mut c = Config::default();
        c.remote.ssh = Some(true);
        c.remote.buttons.insert("scan:800f0422".into(), "ok".into());
        c.remote.buttons.insert("key:KEY_OK".into(), "guide".into());
        let text = toml::to_string_pretty(&c).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), c, "{text}");
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
