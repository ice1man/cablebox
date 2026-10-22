//! OS and app updates through PackageKit (apt backend on Debian).

use std::time::Duration;

use futures_util::StreamExt;
use zbus::OwnedObjectPath;
use zbus::{proxy, Connection};

use crate::{Error, Result};

#[proxy(
    interface = "org.freedesktop.PackageKit",
    default_service = "org.freedesktop.PackageKit",
    default_path = "/org/freedesktop/PackageKit"
)]
trait PackageKit {
    fn create_transaction(&self) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(interface = "org.freedesktop.PackageKit.Transaction", default_service = "org.freedesktop.PackageKit")]
trait Transaction {
    fn set_hints(&self, hints: &[&str]) -> zbus::Result<()>;
    fn refresh_cache(&self, force: bool) -> zbus::Result<()>;
    fn get_updates(&self, filter: u64) -> zbus::Result<()>;
    fn update_packages(&self, transaction_flags: u64, package_ids: &[&str]) -> zbus::Result<()>;

    #[zbus(signal)]
    fn package(&self, info: u32, package_id: String, summary: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn finished(&self, exit: u32, runtime: u32) -> zbus::Result<()>;
    #[zbus(signal)]
    fn error_code(&self, code: u32, details: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn require_restart(&self, kind: u32, package_id: String) -> zbus::Result<()>;

    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<u32>;
}

const EXIT_SUCCESS: u32 = 1;
const INFO_IMPORTANT: u32 = 7;
const INFO_SECURITY: u32 = 8;
const FLAG_ONLY_TRUSTED: u64 = 1 << 1;
const RESTART_SESSION: u32 = 3;
const RESTART_SYSTEM: u32 = 4;
const RESTART_SECURITY_SESSION: u32 = 5;
const RESTART_SECURITY_SYSTEM: u32 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// PackageKit id: `name;version;arch;repo`.
    pub id: String,
    pub name: String,
    pub version: String,
    pub summary: String,
    pub important: bool,
}

impl Update {
    fn from_id(info: u32, id: String, summary: String) -> Self {
        let mut parts = id.split(';');
        let name = parts.next().unwrap_or_default().to_string();
        let version = parts.next().unwrap_or_default().to_string();
        Self { id, name, version, summary, important: matches!(info, INFO_IMPORTANT | INFO_SECURITY) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Restart {
    #[default]
    None,
    /// The app (session) must restart, e.g. cablebox itself was upgraded.
    Session,
    System,
}

struct Outcome {
    packages: Vec<Update>,
    restart: Restart,
}

async fn transaction(conn: &Connection) -> Result<TransactionProxy<'static>> {
    let path = PackageKitProxy::new(conn).await?.create_transaction().await?;
    let tx = TransactionProxy::builder(conn).path(path).build().await?;
    // Non-interactive: never block on a debconf prompt the TV can't show.
    tx.set_hints(&["interactive=false", "background=false"]).await?;
    Ok(tx)
}

/// Waits for `Finished`, collecting packages, restart requirements and progress.
async fn drive<Fut>(tx: &TransactionProxy<'_>, start: Fut, progress: &(dyn Fn(u32) + Sync)) -> Result<Outcome>
where
    Fut: std::future::Future<Output = zbus::Result<()>>,
{
    // Subscribe before starting so no signal is missed.
    let mut packages = tx.receive_package().await?;
    let mut finished = tx.receive_finished().await?;
    let mut errors = tx.receive_error_code().await?;
    let mut restarts = tx.receive_require_restart().await?;
    start.await?;

    let mut out = Outcome { packages: Vec::new(), restart: Restart::None };
    let mut error: Option<String> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            Some(sig) = packages.next() => {
                let a = sig.args()?;
                out.packages.push(Update::from_id(a.info, a.package_id, a.summary));
            }
            Some(sig) = restarts.next() => {
                let a = sig.args()?;
                let r = match a.kind {
                    RESTART_SYSTEM | RESTART_SECURITY_SYSTEM => Restart::System,
                    RESTART_SESSION | RESTART_SECURITY_SESSION => Restart::Session,
                    _ => Restart::None,
                };
                out.restart = out.restart.max(r);
            }
            Some(sig) = errors.next() => {
                let a = sig.args()?;
                error = Some(a.details);
            }
            Some(sig) = finished.next() => {
                let a = sig.args()?;
                if a.exit == EXIT_SUCCESS {
                    return Ok(out);
                }
                return Err(Error::Failed(error.unwrap_or_else(|| format!("update failed (exit {})", a.exit))));
            }
            _ = tick.tick() => {
                if let Ok(p) = tx.percentage().await {
                    if p <= 100 {
                        progress(p);
                    }
                }
            }
        }
    }
}

/// Refreshes package lists (optional) and returns available updates.
pub async fn check(refresh: bool) -> Result<Vec<Update>> {
    let conn = Connection::system().await?;
    if refresh {
        let tx = transaction(&conn).await?;
        drive(&tx, tx.refresh_cache(false), &|_| {}).await?;
    }
    let tx = transaction(&conn).await?;
    let out = drive(&tx, tx.get_updates(0), &|_| {}).await?;
    Ok(out.packages)
}

/// Installs the given updates. `progress` receives 0–100.
pub async fn install(updates: &[Update], progress: &(dyn Fn(u32) + Sync)) -> Result<Restart> {
    let conn = Connection::system().await?;
    let tx = transaction(&conn).await?;
    let ids: Vec<&str> = updates.iter().map(|u| u.id.as_str()).collect();
    let out = drive(&tx, tx.update_packages(FLAG_ONLY_TRUSTED, &ids), progress).await?;
    let mut restart = out.restart;
    if crate::power::reboot_required() {
        restart = Restart::System;
    }
    if updates.iter().any(|u| u.name == "cablebox") {
        restart = restart.max(Restart::Session);
    }
    Ok(restart)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_package_ids() {
        let u = Update::from_id(INFO_SECURITY, "openssl;3.0.15-1;amd64;debian-security".into(), "TLS".into());
        assert_eq!((u.name.as_str(), u.version.as_str(), u.important), ("openssl", "3.0.15-1", true));
    }

    #[test]
    fn restart_ordering() {
        assert!(Restart::System > Restart::Session && Restart::Session > Restart::None);
    }
}
