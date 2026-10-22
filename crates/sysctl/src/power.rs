use zbus::{proxy, Connection};

use crate::Result;

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1 {
    fn reboot(&self, interactive: bool) -> zbus::Result<()>;
    fn power_off(&self, interactive: bool) -> zbus::Result<()>;
}

pub async fn reboot() -> Result<()> {
    let conn = Connection::system().await?;
    Login1Proxy::new(&conn).await?.reboot(false).await?;
    Ok(())
}

pub async fn power_off() -> Result<()> {
    let conn = Connection::system().await?;
    Login1Proxy::new(&conn).await?.power_off(false).await?;
    Ok(())
}

/// Debian's marker that a reboot is needed (kernel, libc, ...).
pub fn reboot_required() -> bool {
    std::path::Path::new("/run/reboot-required").exists()
}
