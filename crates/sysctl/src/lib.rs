//! System integration over D-Bus: OS updates (PackageKit), power (logind) and Wi-Fi
//! (NetworkManager). Privileges come from the polkit rule shipped in `packaging/`.

pub mod network;
pub mod power;
pub mod updates;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("D-Bus: {0}")]
    Dbus(#[from] zbus::Error),
    #[error("{0}")]
    Failed(String),
}

impl From<zbus::fdo::Error> for Error {
    fn from(e: zbus::fdo::Error) -> Self {
        Error::Dbus(e.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
