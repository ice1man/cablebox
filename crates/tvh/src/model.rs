use std::fmt;

use serde::{Deserialize, Deserializer};

/// Generic `{ "entries": [...], "total": n }` envelope returned by `*/grid` endpoints.
#[derive(Debug, Deserialize)]
pub struct Grid<T> {
    #[serde(default = "Vec::new")]
    pub entries: Vec<T>,
    #[serde(default)]
    pub total: Option<u64>,
}

/// Cable-style channel number: major plus optional minor (`5` or `5.1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ChannelNumber {
    pub major: u32,
    pub minor: u32,
}

impl ChannelNumber {
    pub fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// Parses `"12"`, `"12.3"` (also accepts `-` as the minor separator).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (maj, min) = match s.split_once(['.', '-']) {
            Some((a, b)) => (a, Some(b)),
            None => (s, None),
        };
        let major = maj.parse().ok()?;
        let minor = match min {
            Some(m) if !m.is_empty() => m.parse().ok()?,
            _ => 0,
        };
        Some(Self { major, minor })
    }
}

impl fmt::Display for ChannelNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.minor == 0 {
            write!(f, "{}", self.major)
        } else {
            write!(f, "{}.{}", self.major, self.minor)
        }
    }
}

/// Tvheadend reports channel numbers as an integer, a float, or a string depending on version.
fn de_channel_number<'de, D: Deserializer<'de>>(d: D) -> Result<ChannelNumber, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Int(u64),
        Float(f64),
        Str(String),
    }
    Ok(match Option::<Raw>::deserialize(d)? {
        None => ChannelNumber::default(),
        Some(Raw::Int(n)) => ChannelNumber::new(n as u32, 0),
        Some(Raw::Float(f)) => ChannelNumber::parse(&f.to_string()).unwrap_or_default(),
        Some(Raw::Str(s)) => ChannelNumber::parse(&s).unwrap_or_default(),
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    pub uuid: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub name: String,
    #[serde(default, deserialize_with = "de_channel_number")]
    pub number: ChannelNumber,
    /// Relative path such as `imagecache/12`, resolved against the server URL.
    #[serde(default)]
    pub icon_public_url: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelTag {
    pub uuid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub internal: bool,
    #[serde(default)]
    pub index: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpgEvent {
    pub event_id: u64,
    pub channel_uuid: String,
    pub start: i64,
    pub stop: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub episode_onscreen: Option<String>,
    #[serde(default)]
    pub serieslink_uri: Option<String>,
    /// Set when a DVR entry exists for this event.
    #[serde(default)]
    pub dvr_uuid: Option<String>,
    #[serde(default)]
    pub dvr_state: Option<String>,
}

impl EpgEvent {
    pub fn is_airing(&self, now: i64) -> bool {
        self.start <= now && now < self.stop
    }

    /// Best available long-form text.
    pub fn synopsis(&self) -> &str {
        self.description
            .as_deref()
            .or(self.summary.as_deref())
            .or(self.subtitle.as_deref())
            .unwrap_or("")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DvrEntry {
    pub uuid: String,
    #[serde(default)]
    pub disp_title: String,
    #[serde(default)]
    pub disp_subtitle: Option<String>,
    #[serde(default)]
    pub disp_description: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub channelname: Option<String>,
    pub start: i64,
    pub stop: i64,
    #[serde(default)]
    pub sched_status: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub filesize: Option<u64>,
    #[serde(default)]
    pub broadcast: Option<u64>,
}

impl DvrEntry {
    pub fn is_recording(&self) -> bool {
        self.sched_status.as_deref() == Some("recording")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AutorecRule {
    pub uuid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub channel: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerInfo {
    #[serde(default)]
    pub sw_version: String,
    #[serde(default)]
    pub api_version: u32,
    #[serde(default)]
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_channel_numbers() {
        assert_eq!(ChannelNumber::parse("12"), Some(ChannelNumber::new(12, 0)));
        assert_eq!(ChannelNumber::parse("5.1"), Some(ChannelNumber::new(5, 1)));
        assert_eq!(ChannelNumber::parse("7-2"), Some(ChannelNumber::new(7, 2)));
        assert_eq!(ChannelNumber::parse("7."), Some(ChannelNumber::new(7, 0)));
        assert_eq!(ChannelNumber::parse("x"), None);
        assert_eq!(ChannelNumber::new(5, 1).to_string(), "5.1");
        assert_eq!(ChannelNumber::new(5, 0).to_string(), "5");
    }

    #[test]
    fn channel_number_ordering() {
        assert!(ChannelNumber::new(5, 0) < ChannelNumber::new(5, 1));
        assert!(ChannelNumber::new(5, 9) < ChannelNumber::new(6, 0));
    }
}
