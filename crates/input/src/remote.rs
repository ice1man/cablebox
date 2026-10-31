//! IR receivers and HDMI-CEC via the kernel input layer.
//!
//! Both show up as evdev devices driven by rc-core (IR keymaps, and `rc-cec` for CEC
//! adapters such as the Raspberry Pi's HDMI port or a Pulse-Eight USB adapter). We grab
//! remote-like devices exclusively so their digit/arrow keys aren't also seen as keyboard
//! input by libinput. Buttons the user taught CableBox are looked up first; see [`crate::learn`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use evdev::{Device, EventSummary, KeyCode, MiscCode, SynchronizationCode};

use crate::learn::{Bindings, Button, Decoder, HoldTimer, Press, Raw};
use crate::pico::{self, Firmware, PicoHost, PicoInfo, UpdateStatus};
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

/// Whether a sysfs device path belongs to an rc-core receiver (`.../rc/rc0/input5`).
fn is_rc_path(path: &Path) -> bool {
    path.to_string_lossy().contains("/rc/rc")
}

/// Whether `/dev/input/eventN` is driven by rc-core.
fn is_rc(path: &Path) -> bool {
    let Some(name) = path.file_name() else { return false };
    let device = Path::new("/sys/class/input").join(name).join("device");
    device.canonicalize().is_ok_and(|p| is_rc_path(&p))
}

/// Remote-like and not a typing keyboard (never grab the user's keyboard). A receiver counts
/// even with an empty keymap, since its buttons can still be learned.
fn is_remote(dev: &Device, rc: bool) -> bool {
    let keys = dev.supported_keys();
    let has_remote_key = keys.is_some_and(|keys| REMOTE_ONLY.iter().any(|k| keys.contains(*k)));
    (rc || has_remote_key) && !keys.is_some_and(|keys| keys.contains(KeyCode::KEY_A))
}

/// What a remote did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RemoteEvent {
    Key(RemoteKey),
    /// A button pressed while capturing; it had no other effect.
    Captured(Button),
    /// The Pico receiver appeared, went away, or reported something new.
    PicoChanged,
    PicoUpdate(UpdateStatus),
}

/// Controls the remote readers from the UI thread.
#[derive(Clone, Default)]
pub struct RemoteHandle {
    bindings: Arc<RwLock<Bindings>>,
    capturing: Arc<AtomicBool>,
    /// Open devices and their names.
    open: Arc<Mutex<HashMap<PathBuf, String>>>,
    pico: Arc<Mutex<Option<PicoInfo>>>,
    /// Hands firmware to the thread serving the receiver.
    pico_update: Arc<Mutex<Option<mpsc::Sender<Firmware>>>>,
}

impl RemoteHandle {
    pub fn set_bindings(&self, bindings: Bindings) {
        *self.bindings.write().unwrap() = bindings;
    }

    /// While capturing, each new button press is reported instead of acting on anything.
    pub fn set_capture(&self, on: bool) {
        self.capturing.store(on, Ordering::Relaxed);
    }

    pub fn capturing(&self) -> bool {
        self.capturing.load(Ordering::Relaxed)
    }

    /// Names of the remotes and receivers currently open.
    pub fn receivers(&self) -> Vec<String> {
        let mut names: Vec<String> = self.open.lock().unwrap().values().cloned().collect();
        // The port stays open while finding out what's on it; only list it once it answers.
        if self.pico().is_none() {
            names.retain(|n| n != PICO_NAME);
        }
        names.sort();
        names
    }

    /// The Pico receiver, once it has identified itself.
    pub fn pico(&self) -> Option<PicoInfo> {
        *self.pico.lock().unwrap()
    }

    /// Starts installing firmware on the Pico receiver; progress arrives as
    /// [`RemoteEvent::PicoUpdate`]. Returns false if none is connected.
    pub fn update_pico(&self, firmware: Firmware) -> bool {
        self.pico_update.lock().unwrap().as_ref().is_some_and(|tx| tx.send(firmware).is_ok())
    }

    fn event(&self, press: Press) -> Option<RemoteEvent> {
        if self.capturing() {
            return (!press.repeat).then_some(RemoteEvent::Captured(press.button));
        }
        resolve(press.button, &self.bindings.read().unwrap()).map(RemoteEvent::Key)
    }

    fn learned(&self, button: Button) -> bool {
        self.capturing() || self.bindings.read().unwrap().contains_key(&button)
    }
}

/// A learned button's key, or else the built-in table's.
fn resolve(button: Button, bindings: &Bindings) -> Option<RemoteKey> {
    bindings.get(&button).copied().or(match button {
        Button::Key(code) => map_key(KeyCode::new(code)),
        Button::Scan(_) | Button::Ir { .. } => None,
    })
}

/// Starts background threads that deliver remote events (and auto-repeats) to `on_event`.
/// New devices (a CEC adapter appearing late, a USB IR dongle) are picked up automatically.
pub fn spawn(bindings: Bindings, on_event: impl Fn(RemoteEvent) + Send + Sync + 'static) -> RemoteHandle {
    let handle = RemoteHandle::default();
    handle.set_bindings(bindings);
    let on_event = Arc::new(on_event);
    let scanner = handle.clone();
    thread::Builder::new()
        .name("remote-scan".into())
        .spawn(move || loop {
            for (path, mut dev) in evdev::enumerate() {
                let rc = is_rc(&path);
                if scanner.open.lock().unwrap().contains_key(&path) || !is_remote(&dev, rc) {
                    continue;
                }
                let name = dev.name().unwrap_or("?").to_string();
                if let Err(e) = dev.grab() {
                    tracing::warn!("remote {name}: cannot grab {}: {e}", path.display());
                }
                tracing::info!("remote control: {name} ({})", path.display());
                scanner.open.lock().unwrap().insert(path.clone(), name.clone());
                let (handle, on_event) = (scanner.clone(), on_event.clone());
                let _ = thread::Builder::new().name("remote".into()).spawn(move || {
                    read_device(dev, rc, &handle, &*on_event);
                    tracing::info!("remote control removed: {name}");
                    handle.open.lock().unwrap().remove(&path);
                });
            }
            let device = PathBuf::from(pico::DEVICE);
            if device.exists() && !scanner.open.lock().unwrap().contains_key(&device) {
                scanner.open.lock().unwrap().insert(device.clone(), PICO_NAME.into());
                let (handle, on_event) = (scanner.clone(), on_event.clone());
                let _ = thread::Builder::new().name("remote-pico".into()).spawn(move || {
                    read_pico(&device, &handle, &*on_event);
                    handle.open.lock().unwrap().remove(&device);
                });
            }
            thread::sleep(RESCAN);
        })
        .expect("spawn remote scanner");
    handle
}

const PICO_NAME: &str = "CableBox IR receiver";

/// Development only: serves just a Pico receiver at `path`, for working on the Settings pages in
/// a window. Never scans or grabs input devices, so the desktop's keyboard is left alone.
pub fn spawn_pico_only(path: PathBuf, bindings: Bindings, on_event: impl Fn(RemoteEvent) + Send + Sync + 'static) -> RemoteHandle {
    let handle = RemoteHandle::default();
    handle.set_bindings(bindings);
    let scanner = handle.clone();
    thread::Builder::new()
        .name("remote-pico".into())
        .spawn(move || loop {
            if path.exists() {
                scanner.open.lock().unwrap().insert(path.clone(), PICO_NAME.into());
                read_pico(&path, &scanner, &on_event);
                scanner.open.lock().unwrap().remove(&path);
            }
            thread::sleep(Duration::from_secs(1));
        })
        .expect("spawn remote reader");
    handle
}

/// Connects the Pico reader to the handle and the event callback.
struct PicoLink<'a> {
    handle: &'a RemoteHandle,
    on_event: &'a (dyn Fn(RemoteEvent) + Send + Sync),
    updates: mpsc::Receiver<Firmware>,
    hold: Mutex<HoldTimer>,
}

impl PicoHost for PicoLink<'_> {
    fn set_info(&self, info: Option<PicoInfo>) {
        let mut pico = self.handle.pico.lock().unwrap();
        if *pico != info {
            *pico = info;
            drop(pico);
            (self.on_event)(RemoteEvent::PicoChanged);
        }
    }

    fn info(&self) -> Option<PicoInfo> {
        self.handle.pico()
    }

    fn command(&self, protocol: ir_proto::Protocol, address: u16, command: u16) {
        let button = Button::Ir { protocol, address, command };
        let Some(repeat) = self.hold.lock().unwrap().seen(button, Instant::now()) else { return };
        match self.handle.event(Press { button, repeat }) {
            Some(event) => (self.on_event)(event),
            None => tracing::debug!("unmapped remote button {button}"),
        }
    }

    fn next_update(&self) -> Option<Firmware> {
        self.updates.try_recv().ok()
    }

    fn update_status(&self, status: UpdateStatus) {
        (self.on_event)(RemoteEvent::PicoUpdate(status));
    }
}

fn read_pico(path: &Path, handle: &RemoteHandle, on_event: &(dyn Fn(RemoteEvent) + Send + Sync)) {
    let (tx, updates) = mpsc::channel();
    *handle.pico_update.lock().unwrap() = Some(tx);
    let link = PicoLink { handle, on_event, updates, hold: Mutex::default() };
    tracing::info!("remote control: {PICO_NAME} ({})", path.display());
    if let Err(e) = pico::serve(path, &link) {
        tracing::info!("remote control removed: {PICO_NAME}: {e}");
    }
    *handle.pico_update.lock().unwrap() = None;
}

fn read_device(mut dev: Device, rc: bool, handle: &RemoteHandle, on_event: &(dyn Fn(RemoteEvent) + Send + Sync)) {
    let mut decoder = Decoder::new(rc);
    loop {
        let events = match dev.fetch_events() {
            Result::Ok(ev) => ev,
            Err(_) => return,
        };
        for ev in events {
            let raw = match ev.destructure() {
                EventSummary::Misc(_, MiscCode::MSC_SCAN, value) => Raw::Scan(value as u32),
                EventSummary::Key(_, code, value) => Raw::Key { code: code.code(), value },
                EventSummary::Synchronization(_, SynchronizationCode::SYN_REPORT, _) => Raw::Sync,
                _ => continue,
            };
            for press in decoder.feed(raw, Instant::now(), |b| handle.learned(b)) {
                match handle.event(press) {
                    Some(event) => on_event(event),
                    None => tracing::debug!("unmapped remote button {}", press.button),
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

    #[test]
    fn recognises_rc_core_devices_by_their_sysfs_path() {
        assert!(is_rc_path(Path::new("/sys/devices/pci0000:00/usb1/1-2/1-2:1.0/rc/rc0/input5")));
        assert!(!is_rc_path(Path::new("/sys/devices/pci0000:00/usb1/1-3/1-3:1.0/0003:046D:C31C.0001/input/input7")));
    }

    #[test]
    fn learned_buttons_win_over_the_built_in_table() {
        let ok = Button::Key(KeyCode::KEY_OK.code());
        let mut bindings = Bindings::new();
        assert_eq!(resolve(ok, &bindings), Some(Ok));
        assert_eq!(resolve(Button::Scan(7), &bindings), None);
        bindings.insert(ok, Guide);
        bindings.insert(Button::Scan(7), Mute);
        assert_eq!(resolve(ok, &bindings), Some(Guide));
        assert_eq!(resolve(Button::Scan(7), &bindings), Some(Mute));
    }

    #[test]
    fn capture_reports_new_presses_and_takes_over_every_button() {
        let handle = RemoteHandle::default();
        let press = Press { button: Button::Key(KeyCode::KEY_OK.code()), repeat: false };
        assert_eq!(handle.event(press), Some(RemoteEvent::Key(Ok)));
        assert!(!handle.learned(Button::Scan(7)));
        handle.set_capture(true);
        assert_eq!(handle.event(press), Some(RemoteEvent::Captured(press.button)));
        assert_eq!(handle.event(Press { repeat: true, ..press }), None);
        assert!(handle.learned(Button::Scan(7)));
    }
}
