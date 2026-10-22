//! IR receivers and HDMI-CEC via the kernel input layer.
//!
//! Both show up as evdev devices driven by rc-core (IR keymaps, and `rc-cec` for CEC
//! adapters such as the Raspberry Pi's HDMI port or a Pulse-Eight USB adapter). We grab
//! remote-like devices exclusively so their digit/arrow keys aren't also seen as keyboard
//! input by libinput.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use evdev::{Device, EventSummary, KeyCode};

use crate::RemoteKey::{self, *};

const RESCAN: Duration = Duration::from_secs(5);

/// Keys only remotes have; a device with any of these is treated as a remote.
const REMOTE_ONLY: [KeyCode; 6] = [
    KeyCode::KEY_OK,
    KeyCode::KEY_CHANNELUP,
    KeyCode::KEY_NUMERIC_0,
    KeyCode::KEY_RED,
    KeyCode::KEY_EPG,
    KeyCode::KEY_ROOT_MENU,
];

pub fn map_key(code: KeyCode) -> Option<RemoteKey> {
    let c = code.code();
    let digit = |base: KeyCode| c.checked_sub(base.code()).filter(|d| *d < 10).map(|d| Digit(d as u8));
    if let Some(d) = digit(KeyCode::KEY_NUMERIC_0) {
        return Some(d);
    }
    Some(match code {
        // KEY_1..KEY_9 then KEY_0 on the top row.
        KeyCode::KEY_0 => Digit(0),
        k if (KeyCode::KEY_1.code()..=KeyCode::KEY_9.code()).contains(&k.code()) => {
            Digit((k.code() - KeyCode::KEY_1.code() + 1) as u8)
        }
        KeyCode::KEY_KP0 => Digit(0),
        KeyCode::KEY_KP1 => Digit(1),
        KeyCode::KEY_KP2 => Digit(2),
        KeyCode::KEY_KP3 => Digit(3),
        KeyCode::KEY_KP4 => Digit(4),
        KeyCode::KEY_KP5 => Digit(5),
        KeyCode::KEY_KP6 => Digit(6),
        KeyCode::KEY_KP7 => Digit(7),
        KeyCode::KEY_KP8 => Digit(8),
        KeyCode::KEY_KP9 => Digit(9),
        KeyCode::KEY_DOT | KeyCode::KEY_KPDOT | KeyCode::KEY_NUMERIC_STAR => Dot,
        KeyCode::KEY_UP => Up,
        KeyCode::KEY_DOWN => Down,
        KeyCode::KEY_LEFT => Left,
        KeyCode::KEY_RIGHT => Right,
        KeyCode::KEY_OK | KeyCode::KEY_ENTER | KeyCode::KEY_KPENTER | KeyCode::KEY_SELECT => Ok,
        KeyCode::KEY_BACK | KeyCode::KEY_ESC | KeyCode::KEY_BACKSPACE => Back,
        KeyCode::KEY_EXIT => Exit,
        KeyCode::KEY_EPG | KeyCode::KEY_PROGRAM => Guide,
        KeyCode::KEY_INFO | KeyCode::KEY_TEXT => Info,
        KeyCode::KEY_MENU | KeyCode::KEY_ROOT_MENU | KeyCode::KEY_SETUP | KeyCode::KEY_CONTEXT_MENU => Menu,
        KeyCode::KEY_CHANNELUP => ChannelUp,
        KeyCode::KEY_CHANNELDOWN => ChannelDown,
        KeyCode::KEY_PREVIOUS | KeyCode::KEY_LAST | KeyCode::KEY_AGAIN => Last,
        KeyCode::KEY_RECORD => Record,
        KeyCode::KEY_PLAY | KeyCode::KEY_PAUSE | KeyCode::KEY_PLAYPAUSE => PlayPause,
        KeyCode::KEY_STOP | KeyCode::KEY_STOPCD => Stop,
        KeyCode::KEY_FASTFORWARD | KeyCode::KEY_FORWARD => FastForward,
        KeyCode::KEY_REWIND => Rewind,
        KeyCode::KEY_VOLUMEUP => VolumeUp,
        KeyCode::KEY_VOLUMEDOWN => VolumeDown,
        KeyCode::KEY_MUTE => Mute,
        KeyCode::KEY_RED => Red,
        KeyCode::KEY_GREEN => Green,
        KeyCode::KEY_YELLOW | KeyCode::KEY_FAVORITES => Yellow,
        KeyCode::KEY_BLUE => Blue,
        KeyCode::KEY_PVR | KeyCode::KEY_TAPE | KeyCode::KEY_VIDEO => Dvr,
        KeyCode::KEY_PAGEUP => PageUp,
        KeyCode::KEY_PAGEDOWN => PageDown,
        _ => return None,
    })
}

/// Remote-like and not a typing keyboard (never grab the user's keyboard).
fn is_remote(dev: &Device) -> bool {
    dev.supported_keys().is_some_and(|keys| {
        REMOTE_ONLY.iter().any(|k| keys.contains(*k)) && !keys.contains(KeyCode::KEY_A)
    })
}

/// Starts background threads that deliver remote key presses (and auto-repeats) to `on_key`.
/// New devices (a CEC adapter appearing late, a USB IR dongle) are picked up automatically.
pub fn spawn(on_key: impl Fn(RemoteKey) + Send + Sync + 'static) {
    let on_key = Arc::new(on_key);
    let open: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
    thread::Builder::new()
        .name("remote-scan".into())
        .spawn(move || loop {
            for (path, mut dev) in evdev::enumerate() {
                if open.lock().unwrap().contains(&path) || !is_remote(&dev) {
                    continue;
                }
                let name = dev.name().unwrap_or("?").to_string();
                if let Err(e) = dev.grab() {
                    tracing::warn!("remote {name}: cannot grab {}: {e}", path.display());
                }
                tracing::info!("remote control: {name} ({})", path.display());
                open.lock().unwrap().insert(path.clone());
                let (open, on_key) = (open.clone(), on_key.clone());
                let _ = thread::Builder::new().name("remote".into()).spawn(move || {
                    read_device(dev, &*on_key);
                    tracing::info!("remote control removed: {name}");
                    open.lock().unwrap().remove(&path);
                });
            }
            thread::sleep(RESCAN);
        })
        .expect("spawn remote scanner");
}

fn read_device(mut dev: Device, on_key: &(dyn Fn(RemoteKey) + Send + Sync)) {
    loop {
        let events = match dev.fetch_events() {
            Result::Ok(ev) => ev,
            Err(_) => return,
        };
        for ev in events {
            // 1 = press, 2 = autorepeat, 0 = release.
            if let EventSummary::Key(_, code, 1 | 2) = ev.destructure() {
                match map_key(code) {
                    Some(k) => on_key(k),
                    None => tracing::debug!("unmapped remote key {code:?}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_cec_and_ir_keys() {
        assert_eq!(map_key(KeyCode::KEY_NUMERIC_7), Some(Digit(7)));
        assert_eq!(map_key(KeyCode::KEY_1), Some(Digit(1)));
        assert_eq!(map_key(KeyCode::KEY_9), Some(Digit(9)));
        assert_eq!(map_key(KeyCode::KEY_0), Some(Digit(0)));
        assert_eq!(map_key(KeyCode::KEY_KP5), Some(Digit(5)));
        assert_eq!(map_key(KeyCode::KEY_EPG), Some(Guide));
        assert_eq!(map_key(KeyCode::KEY_SELECT), Some(Ok));
        assert_eq!(map_key(KeyCode::KEY_CHANNELUP), Some(ChannelUp));
        assert_eq!(map_key(KeyCode::KEY_A), None);
    }
}
