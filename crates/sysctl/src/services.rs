//! Starting/stopping a few system services through systemd (PID 1) over D-Bus.
//! Allowed for the `cablebox` user by the polkit rule, for specific units only.

use zbus::{proxy, Connection, OwnedObjectPath};

use crate::Result;

#[proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait Manager {
    fn load_unit(&self, name: &str) -> zbus::Result<OwnedObjectPath>;
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(interface = "org.freedesktop.systemd1.Service", default_service = "org.freedesktop.systemd1")]
trait Service {
    #[zbus(property, name = "MainPID")]
    fn main_pid(&self) -> zbus::Result<u32>;
}

#[proxy(interface = "org.freedesktop.systemd1.Unit", default_service = "org.freedesktop.systemd1")]
trait Unit {
    #[zbus(property)]
    fn load_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn active_state(&self) -> zbus::Result<String>;
}

/// The OpenSSH server unit on Debian and Raspberry Pi OS.
pub const SSH: &str = "ssh.service";
/// Starting this login prompt stops CableBox (its unit `Conflicts=` with it).
pub const CONSOLE: &str = "getty@tty1.service";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    NotInstalled,
    Running,
    Stopped,
}

pub async fn state(unit: &str) -> Result<State> {
    let conn = Connection::system().await?;
    let path = ManagerProxy::new(&conn).await?.load_unit(unit).await?;
    let unit = UnitProxy::builder(&conn).path(path).build().await?;
    if unit.load_state().await? == "not-found" {
        return Ok(State::NotInstalled);
    }
    Ok(match unit.active_state().await?.as_str() {
        "active" | "activating" | "reloading" => State::Running,
        _ => State::Stopped,
    })
}

pub async fn start(unit: &str) -> Result<()> {
    let conn = Connection::system().await?;
    ManagerProxy::new(&conn).await?.start_unit(unit, "replace").await?;
    Ok(())
}

pub async fn stop(unit: &str) -> Result<()> {
    let conn = Connection::system().await?;
    ManagerProxy::new(&conn).await?.stop_unit(unit, "replace").await?;
    Ok(())
}

/// Main process ID systemd records for a service (0 when it isn't running).
pub async fn main_pid(unit: &str) -> Result<u32> {
    let conn = Connection::system().await?;
    let path = ManagerProxy::new(&conn).await?.load_unit(unit).await?;
    let service = ServiceProxy::builder(&conn).path(path).build().await?;
    Ok(service.main_pid().await?)
}

/// True when this process is the main process of the given system service. Unlike a
/// cgroup check this survives `PAMName=` moving the process into a logind session scope.
pub async fn is_main_process_of(unit: &str) -> bool {
    main_pid(unit).await.is_ok_and(|pid| pid != 0 && pid == std::process::id())
}
