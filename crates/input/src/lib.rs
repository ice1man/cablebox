//! Unified remote-control keys. Keyboard, IR (evdev) and HDMI-CEC all map onto [`RemoteKey`].

mod keyboard;
pub mod control;
pub mod remote;

pub use keyboard::from_key_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemoteKey {
    Digit(u8),
    /// Minor-channel separator (`5.1`).
    Dot,
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
    Exit,
    Guide,
    Info,
    Menu,
    ChannelUp,
    ChannelDown,
    /// Jump to the previously watched channel.
    Last,
    Record,
    PlayPause,
    Stop,
    FastForward,
    Rewind,
    VolumeUp,
    VolumeDown,
    Mute,
    Red,
    Green,
    Yellow,
    Blue,
    /// Opens the recordings library.
    Dvr,
    PageUp,
    PageDown,
}

impl RemoteKey {
    /// Parses a key name as used by the control FIFO (`guide`, `ch+`, `7`, `ok`, ...).
    pub fn from_name(name: &str) -> Option<Self> {
        use RemoteKey::*;
        let name = name.trim().to_ascii_lowercase();
        if let [d @ b'0'..=b'9'] = name.as_bytes() {
            return Some(Digit(d - b'0'));
        }
        Some(match name.as_str() {
            "." | "dot" => Dot,
            "up" => Up,
            "down" => Down,
            "left" => Left,
            "right" => Right,
            "ok" | "select" | "enter" => Ok,
            "back" => Back,
            "exit" => Exit,
            "guide" => Guide,
            "info" => Info,
            "menu" => Menu,
            "ch+" | "chup" | "channelup" => ChannelUp,
            "ch-" | "chdown" | "channeldown" => ChannelDown,
            "last" => Last,
            "record" | "rec" => Record,
            "play" | "pause" | "playpause" => PlayPause,
            "stop" => Stop,
            "ff" | "fastforward" => FastForward,
            "rew" | "rewind" => Rewind,
            "vol+" => VolumeUp,
            "vol-" => VolumeDown,
            "mute" => Mute,
            "red" => Red,
            "green" => Green,
            "yellow" => Yellow,
            "blue" => Blue,
            "dvr" | "recordings" => Dvr,
            "pageup" => PageUp,
            "pagedown" => PageDown,
            _ => return None,
        })
    }
}
