//! Time zone, network time (NTP) and the system clock through systemd-timedated.
//! Allowed for the `cablebox` user by the polkit rule.

use zbus::{proxy, Connection};

use crate::Result;

#[proxy(
    interface = "org.freedesktop.timedate1",
    default_service = "org.freedesktop.timedate1",
    default_path = "/org/freedesktop/timedate1"
)]
trait Timedate1 {
    fn list_timezones(&self) -> zbus::Result<Vec<String>>;
    fn set_timezone(&self, timezone: &str, interactive: bool) -> zbus::Result<()>;
    #[zbus(name = "SetNTP")]
    fn set_ntp(&self, use_ntp: bool, interactive: bool) -> zbus::Result<()>;
    fn set_time(&self, usec_utc: i64, relative: bool, interactive: bool) -> zbus::Result<()>;

    #[zbus(property)]
    fn timezone(&self) -> zbus::Result<String>;
    #[zbus(property, name = "NTP")]
    fn ntp(&self) -> zbus::Result<bool>;
    #[zbus(property, name = "CanNTP")]
    fn can_ntp(&self) -> zbus::Result<bool>;
    #[zbus(property, name = "NTPSynchronized")]
    fn ntp_synchronized(&self) -> zbus::Result<bool>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub timezone: String,
    pub ntp: bool,
    pub can_ntp: bool,
    pub synced: bool,
}

async fn proxy() -> Result<Timedate1Proxy<'static>> {
    let conn = Connection::system().await?;
    Ok(Timedate1Proxy::new(&conn).await?)
}

pub async fn status() -> Result<Status> {
    let p = proxy().await?;
    Ok(Status {
        timezone: p.timezone().await?,
        ntp: p.ntp().await?,
        can_ntp: p.can_ntp().await.unwrap_or(false),
        synced: p.ntp_synchronized().await.unwrap_or(false),
    })
}

/// All known zones ("America/New_York", ...), sorted.
pub async fn list_timezones() -> Result<Vec<String>> {
    let mut zones = match proxy().await?.list_timezones().await {
        Ok(z) if !z.is_empty() => z,
        _ => zones_from_tzdata(),
    };
    zones.sort();
    zones.dedup();
    Ok(zones)
}

/// Fallback for systemd without `ListTimezones`: zone names from tzdata's compact source.
fn zones_from_tzdata() -> Vec<String> {
    std::fs::read_to_string("/usr/share/zoneinfo/tzdata.zi")
        .map(|s| {
            s.lines()
                .filter_map(|l| {
                    let mut f = l.split_whitespace();
                    match (f.next(), f.next(), f.next()) {
                        (Some("Z"), Some(name), _) => Some(name.to_string()),
                        (Some("L"), _, Some(alias)) => Some(alias.to_string()),
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

pub async fn set_timezone(tz: &str) -> Result<()> {
    Ok(proxy().await?.set_timezone(tz, false).await?)
}

pub async fn set_ntp(on: bool) -> Result<()> {
    Ok(proxy().await?.set_ntp(on, false).await?)
}

/// Sets the system clock to an absolute time (microseconds since the Unix epoch, UTC).
/// Fails while NTP is on.
pub async fn set_time(unix_usec: i64) -> Result<()> {
    Ok(proxy().await?.set_time(unix_usec, false, false).await?)
}
