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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Remote {
    pub cec: bool,
    /// Device name shown on the TV's CEC menu.
    pub cec_name: String,
}

impl Default for Remote {
    fn default() -> Self {
        Self { cec: true, cec_name: "CableBox".into() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    /// `WIDTHxHEIGHT@HZ` or empty for the connector's preferred mode. Applied on session restart.
    pub mode: String,
    /// Overrides the automatic 720p-based UI scale.
    pub scale: Option<f32>,
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
pub struct Config {
    pub server: Server,
    pub ui: Ui,
    pub remote: Remote,
    pub display: Display,
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
    fn empty_user_means_no_auth() {
        let s = Server { url: "x".into(), ..Default::default() };
        assert_eq!(s.tvh_auth(), tvh::Auth::None);
    }
}
