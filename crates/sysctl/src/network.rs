//! Wi-Fi status, scanning and connecting through NetworkManager.

use std::collections::HashMap;
use std::time::Duration;

use zbus::{ObjectPath, OwnedObjectPath, OwnedVariant, Variant};
use zbus::{proxy, Connection};

use crate::{Error, Result};

#[proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    fn get_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    fn add_and_activate_connection(
        &self,
        connection: HashMap<&str, HashMap<&str, Variant<'_>>>,
        device: &ObjectPath<'_>,
        specific_object: &ObjectPath<'_>,
    ) -> zbus::Result<(OwnedObjectPath, OwnedObjectPath)>;
    #[zbus(property)]
    fn primary_connection(&self) -> zbus::Result<OwnedObjectPath>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
}

#[proxy(interface = "org.freedesktop.NetworkManager.Device", default_service = "org.freedesktop.NetworkManager")]
trait Device {
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Wireless {
    fn request_scan(&self, options: HashMap<&str, Variant<'_>>) -> zbus::Result<()>;
    fn get_all_access_points(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(interface = "org.freedesktop.NetworkManager.AccessPoint", default_service = "org.freedesktop.NetworkManager")]
trait AccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;
    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;
    #[zbus(property)]
    fn wpa_flags(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn rsn_flags(&self) -> zbus::Result<u32>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Connection.Active",
    default_service = "org.freedesktop.NetworkManager"
)]
trait ActiveConnection {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;
    #[zbus(property, name = "Type")]
    fn kind(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn ip4_config(&self) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(interface = "org.freedesktop.NetworkManager.IP4Config", default_service = "org.freedesktop.NetworkManager")]
trait Ip4Config {
    #[zbus(property)]
    fn address_data(&self) -> zbus::Result<Vec<HashMap<String, OwnedVariant>>>;
}

const DEVICE_TYPE_WIFI: u32 = 2;
const STATE_CONNECTED_GLOBAL: u32 = 70;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub connected: bool,
    /// Connection name, e.g. "Wired connection 1" or the SSID.
    pub name: String,
    pub address: String,
    pub wifi: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPointInfo {
    pub ssid: String,
    pub strength: u8,
    pub secured: bool,
    pub active: bool,
}

pub async fn status() -> Result<Status> {
    let conn = Connection::system().await?;
    let nm = NetworkManagerProxy::new(&conn).await?;
    let mut st = Status { connected: nm.state().await? >= STATE_CONNECTED_GLOBAL, ..Default::default() };
    let primary = nm.primary_connection().await?;
    if primary.as_str() == "/" {
        return Ok(st);
    }
    let active = ActiveConnectionProxy::builder(&conn).path(primary).build().await?;
    st.name = active.id().await.unwrap_or_default();
    st.wifi = active.kind().await.unwrap_or_default() == "802-11-wireless";
    if let Ok(ip4) = active.ip4_config().await {
        let cfg = Ip4ConfigProxy::builder(&conn).path(ip4).build().await?;
        if let Some(addr) = cfg.address_data().await.ok().and_then(|a| a.into_iter().next()) {
            if let Some(v) = addr.get("address") {
                st.address = String::try_from(v.try_clone()?).unwrap_or_default();
            }
        }
    }
    Ok(st)
}

async fn wifi_device(conn: &Connection) -> Result<OwnedObjectPath> {
    let nm = NetworkManagerProxy::new(conn).await?;
    for path in nm.get_devices().await? {
        let dev = DeviceProxy::builder(conn).path(path.clone()).build().await?;
        if dev.device_type().await? == DEVICE_TYPE_WIFI {
            return Ok(path);
        }
    }
    Err(Error::Failed("No Wi-Fi adapter found".into()))
}

/// Scans and returns networks sorted by signal strength, one entry per SSID.
pub async fn scan() -> Result<Vec<AccessPointInfo>> {
    let conn = Connection::system().await?;
    let dev = wifi_device(&conn).await?;
    let wifi = WirelessProxy::builder(&conn).path(dev).build().await?;
    if wifi.request_scan(HashMap::new()).await.is_ok() {
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    let active = wifi.active_access_point().await.ok();
    let mut out: Vec<AccessPointInfo> = Vec::new();
    for path in wifi.get_all_access_points().await? {
        let is_active = active.as_ref() == Some(&path);
        let ap = AccessPointProxy::builder(&conn).path(path).build().await?;
        let ssid = String::from_utf8_lossy(&ap.ssid().await?).into_owned();
        if ssid.is_empty() {
            continue;
        }
        let info = AccessPointInfo {
            strength: ap.strength().await.unwrap_or(0),
            secured: ap.wpa_flags().await.unwrap_or(0) != 0 || ap.rsn_flags().await.unwrap_or(0) != 0,
            active: is_active,
            ssid,
        };
        match out.iter_mut().find(|o| o.ssid == info.ssid) {
            Some(o) if o.strength < info.strength => *o = AccessPointInfo { active: o.active || info.active, ..info },
            Some(o) => o.active |= info.active,
            None => out.push(info),
        }
    }
    out.sort_by(|a, b| b.active.cmp(&a.active).then(b.strength.cmp(&a.strength)));
    Ok(out)
}

/// Creates a connection profile and activates it. `password` is ignored for open networks.
pub async fn connect(ssid: &str, password: Option<&str>) -> Result<()> {
    let conn = Connection::system().await?;
    let dev = wifi_device(&conn).await?;
    let nm = NetworkManagerProxy::new(&conn).await?;

    let mut settings: HashMap<&str, HashMap<&str, Variant<'_>>> = HashMap::new();
    settings.insert("connection", HashMap::from([("id", Variant::from(ssid)), ("type", Variant::from("802-11-wireless"))]));
    settings.insert("802-11-wireless", HashMap::from([("ssid", Variant::from(ssid.as_bytes().to_vec()))]));
    if let Some(pw) = password.filter(|p| !p.is_empty()) {
        settings.insert(
            "802-11-wireless-security",
            HashMap::from([("key-mgmt", Variant::from("wpa-psk")), ("psk", Variant::from(pw))]),
        );
    }
    let root = ObjectPath::try_from("/")?;
    nm.add_and_activate_connection(settings, &dev.as_ref(), &root).await?;
    Ok(())
}
